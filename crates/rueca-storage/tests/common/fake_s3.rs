//! A fake S3 / GCS XML API server over `std::net::TcpListener`, enough for
//! the backend's requests: conditional PUT, conditional and ranged GET,
//! DELETE, ListObjectsV2 (pages of 3 keys) and multipart uploads. It checks
//! that every conditional header is a signed header (AWS rejects unsigned
//! ones); it does not verify signatures.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

const NS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";
const PAGE: usize = 3;
const CONDITIONS: [&str; 4] =
    ["if-match", "if-none-match", "x-goog-if-generation-match", "x-goog-if-generation-not-match"];

struct Obj {
    data: Vec<u8>,
    generation: u64,
}

#[derive(Default)]
struct State {
    objects: BTreeMap<String, Obj>,
    uploads: HashMap<String, BTreeMap<u16, Vec<u8>>>,
    counter: u64,
    completed_uploads: u64,
}

struct Req {
    method: String,
    key: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

struct Resp {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn resp(status: u16) -> Resp {
    Resp { status, headers: Vec::new(), body: Vec::new() }
}

fn xml(status: u16, body: String) -> Resp {
    Resp { status, headers: Vec::new(), body: body.into_bytes() }
}

fn e_tag(generation: u64) -> String {
    format!("\"{generation:032x}\"")
}

pub struct FakeS3 {
    pub endpoint: String,
    state: Arc<Mutex<State>>,
}

impl FakeS3 {
    pub fn start() -> FakeS3 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let state = shared.clone();
                std::thread::spawn(move || serve(stream, &state));
            }
        });
        FakeS3 { endpoint, state }
    }

    pub fn completed_uploads(&self) -> u64 {
        self.state.lock().unwrap().completed_uploads
    }

    pub fn pending_uploads(&self) -> usize {
        self.state.lock().unwrap().uploads.len()
    }
}

fn serve(stream: TcpStream, state: &Mutex<State>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let Some(req) = read_request(&mut reader) else { return };
    let r = handle(&mut state.lock().unwrap(), req);
    let mut out = stream;
    let mut head = format!("HTTP/1.1 {} X\r\ncontent-length: {}\r\nconnection: close\r\n", r.status, r.body.len());
    for (k, v) in &r.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = out.write_all(head.as_bytes());
    let _ = out.write_all(&r.body);
}

fn read_request(reader: &mut impl BufRead) -> Option<Req> {
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let mut headers = HashMap::new();
    loop {
        let mut h = String::new();
        reader.read_line(&mut h).ok()?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        let (k, v) = h.split_once(':')?;
        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
    }
    let len: usize = headers.get("content-length").map_or(0, |v| v.parse().unwrap());
    let mut body = vec![0; len];
    reader.read_exact(&mut body).ok()?;
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let query = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(k), decode(v))
        })
        .collect();
    // Path style: /<bucket>/<key>
    let path = decode(path);
    let key = path.trim_start_matches('/').split_once('/').map_or("", |(_, k)| k).to_string();
    Some(Req { method, key, query, headers, body })
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 3 <= b.len() {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

fn handle(st: &mut State, req: Req) -> Resp {
    let signed = req.query.get("X-Amz-SignedHeaders").cloned().unwrap_or_default();
    let signed: Vec<&str> = signed.split(';').collect();
    if CONDITIONS.iter().any(|c| req.headers.contains_key(*c) && !signed.contains(c)) {
        return xml(403, "<Error><Code>AccessDenied</Code><Message>unsigned condition</Message></Error>".into());
    }
    let q = |k: &str| req.query.get(k).cloned();
    match (req.method.as_str(), req.key.is_empty()) {
        ("GET", true) => list(st, &req),
        ("PUT", false) if q("uploadId").is_some() => upload_part(st, &req),
        ("PUT", false) => put(st, &req),
        ("POST", false) if req.query.contains_key("uploads") => initiate(st),
        ("POST", false) => complete(st, &req),
        ("DELETE", false) if q("uploadId").is_some() => {
            st.uploads.remove(&q("uploadId").unwrap());
            resp(204)
        }
        ("DELETE", false) => {
            st.objects.remove(&req.key);
            resp(204)
        }
        ("GET", false) => get(st, &req),
        _ => resp(400),
    }
}

/// The failure status of a write's conditions, if any.
fn write_check(current: Option<&Obj>, h: &HashMap<String, String>) -> Option<u16> {
    if h.get("if-none-match").map(String::as_str) == Some("*") && current.is_some() {
        return Some(412);
    }
    if let Some(want) = h.get("if-match") {
        let Some(cur) = current else { return Some(404) };
        if *want != e_tag(cur.generation) {
            return Some(412);
        }
    }
    if let Some(want) = h.get("x-goog-if-generation-match") {
        let have = current.map_or(0, |c| c.generation);
        if want.parse::<u64>().unwrap() != have {
            return Some(412);
        }
    }
    None
}

fn store(st: &mut State, key: &str, data: Vec<u8>) -> Resp {
    st.counter += 1;
    let generation = st.counter;
    st.objects.insert(key.to_string(), Obj { data, generation });
    let mut r = resp(200);
    r.headers.push(("etag".into(), e_tag(generation)));
    r.headers.push(("x-goog-generation".into(), generation.to_string()));
    r
}

fn put(st: &mut State, req: &Req) -> Resp {
    if let Some(status) = write_check(st.objects.get(&req.key), &req.headers) {
        return xml(status, "<Error><Code>PreconditionFailed</Code></Error>".into());
    }
    store(st, &req.key, req.body.clone())
}

fn get(st: &mut State, req: &Req) -> Resp {
    let Some(obj) = st.objects.get(&req.key) else {
        return xml(404, "<Error><Code>NoSuchKey</Code></Error>".into());
    };
    let h = &req.headers;
    let unchanged = h.get("if-none-match") == Some(&e_tag(obj.generation))
        || h.get("x-goog-if-generation-not-match") == Some(&obj.generation.to_string());
    let mut r = if unchanged { resp(304) } else { ranged(&obj.data, h.get("range")) };
    r.headers.push(("etag".into(), e_tag(obj.generation)));
    r.headers.push(("x-goog-generation".into(), obj.generation.to_string()));
    r
}

fn ranged(data: &[u8], range: Option<&String>) -> Resp {
    let Some(spec) = range.and_then(|r| r.strip_prefix("bytes=")) else {
        return Resp { status: 200, headers: Vec::new(), body: data.to_vec() };
    };
    let (a, b) = spec.split_once('-').unwrap();
    let (a, b): (usize, usize) = (a.parse().unwrap(), b.parse().unwrap());
    if a >= data.len() {
        return resp(416);
    }
    Resp { status: 206, headers: Vec::new(), body: data[a..=b.min(data.len() - 1)].to_vec() }
}

fn list(st: &mut State, req: &Req) -> Resp {
    let prefix = req.query.get("prefix").cloned().unwrap_or_default();
    let after = req.query.get("continuation-token").cloned();
    let keys: Vec<&String> =
        st.objects.keys().filter(|k| k.starts_with(&prefix) && after.as_ref().is_none_or(|a| *k > a)).collect();
    let truncated = keys.len() > PAGE;
    let mut body = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListBucketResult xmlns=\"{NS}\"><Name>b</Name><Prefix>{prefix}</Prefix><MaxKeys>{PAGE}</MaxKeys><IsTruncated>{truncated}</IsTruncated>"
    );
    for k in keys.iter().take(PAGE) {
        let o = &st.objects[*k];
        body.push_str(&format!(
            "<Contents><Key>{k}</Key><LastModified>2026-11-17T10:00:00.000Z</LastModified><ETag>{}</ETag><Size>{}</Size><StorageClass>STANDARD</StorageClass></Contents>",
            e_tag(o.generation).replace('"', "&quot;"),
            o.data.len()
        ));
    }
    if truncated {
        body.push_str(&format!("<NextContinuationToken>{}</NextContinuationToken>", keys[PAGE - 1]));
    }
    body.push_str("</ListBucketResult>");
    xml(200, body)
}

fn initiate(st: &mut State) -> Resp {
    st.counter += 1;
    let id = format!("upload-{}", st.counter);
    st.uploads.insert(id.clone(), BTreeMap::new());
    xml(
        200,
        format!(
            "<InitiateMultipartUploadResult xmlns=\"{NS}\"><Bucket>b</Bucket><Key>k</Key><UploadId>{id}</UploadId></InitiateMultipartUploadResult>"
        ),
    )
}

fn upload_part(st: &mut State, req: &Req) -> Resp {
    let number: u16 = req.query["partNumber"].parse().unwrap();
    let Some(parts) = st.uploads.get_mut(&req.query["uploadId"]) else { return resp(404) };
    parts.insert(number, req.body.clone());
    let mut r = resp(200);
    r.headers.push(("etag".into(), format!("\"part-{number}\"")));
    r
}

fn complete(st: &mut State, req: &Req) -> Resp {
    if let Some(status) = write_check(st.objects.get(&req.key), &req.headers) {
        return xml(status, "<Error><Code>PreconditionFailed</Code></Error>".into());
    }
    let Some(parts) = st.uploads.remove(&req.query["uploadId"]) else { return resp(404) };
    let data: Vec<u8> = parts.into_values().flatten().collect();
    st.completed_uploads += 1;
    let mut r = store(st, &req.key, data);
    let tag = r.headers[0].1.replace('"', "&quot;");
    r.body = format!("<CompleteMultipartUploadResult xmlns=\"{NS}\"><Location>x</Location><Bucket>b</Bucket><Key>{}</Key><ETag>{tag}</ETag></CompleteMultipartUploadResult>", req.key).into_bytes();
    r
}
