//! Fixture tests of the Rust contract hints: one test per framework or
//! rule, negatives included.

use rueca_extract::{Budget, LanguagePack};
use rueca_model::{ContractKind, ContractSide, FileFacts, Language};

use crate::RustPack;

fn facts(src: &str) -> FileFacts {
    RustPack::default().extract("src/x.rs", Language::Rust, src.as_bytes(), &Budget::default()).unwrap()
}

/// `(key, owner qualified name, framework)` of the hints of one side and kind.
fn keys(src: &str, side: ContractSide, kind: ContractKind) -> Vec<(String, String, String)> {
    let f = facts(src);
    f.contracts
        .iter()
        .filter(|h| h.side == side && h.kind == kind)
        .map(|h| {
            let owner = h.owner.map(|o| f.definitions[o as usize].qualified_name.clone()).unwrap_or_default();
            (h.key.clone(), owner, h.framework.clone())
        })
        .collect()
}

fn triples(v: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    v.iter().map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string())).collect()
}

fn provided(src: &str) -> Vec<(String, String, String)> {
    keys(src, ContractSide::Provides, ContractKind::Http)
}

#[test]
fn actix_attribute_routes_and_scopes() {
    let src = r#"
use actix_web::{get, post, web, Responder};

#[get("/collections/{collection_name}")]
async fn get_collection(path: web::Path<String>) -> impl Responder { "" }

/// Doc.
#[post("/collections/{name}/points")]
#[allow(unused)]
async fn upsert(path: web::Path<String>) -> impl Responder { "" }

#[actix_web::route("/both", method = "GET", method = "PUT")]
async fn both() -> impl Responder { "" }

#[get("/scoped")]
async fn scoped() -> impl Responder { "" }

#[test]
fn not_a_route() {}

pub fn config(cfg: &mut web::ServiceConfig) {
    cfg.service(get_collection).service(upsert);
    cfg.service(web::scope("/v1").service(web::scope("/inner").service(scoped)));
}
"#;
    assert_eq!(
        provided(src),
        triples(&[
            ("GET /collections/{}", "get_collection", "actix"),
            ("POST /collections/{}/points", "upsert", "actix"),
            ("GET /both", "both", "actix"),
            ("PUT /both", "both", "actix"),
            ("GET /v1/inner/scoped", "scoped", "actix"),
        ])
    );
}

#[test]
fn attributes_need_a_framework() {
    let src = r#"
#[get("/x")]
fn x() {}
"#;
    assert_eq!(provided(src), vec![]);
    let rocket = r#"
use rocket::get;
#[get("/hello/<name>")]
fn hello(name: &str) -> String { name.into() }
"#;
    assert_eq!(provided(rocket), triples(&[("GET /hello/{}", "hello", "rocket")]));
}

#[test]
fn actix_routes_and_resources() {
    let src = r#"
use actix_web::{web, App};
async fn index() -> &'static str { "" }
async fn create() -> &'static str { "" }
fn app() {
    App::new()
        .route("/", web::get().to(index))
        .service(
            web::scope("/api")
                .route("/items", web::post().to(create))
                .service(web::resource("/items/{id}").route(web::get().to(index)).route(web::delete().to(index)))
                .service(web::resource("/any").to(|| async { "" })),
        );
}
"#;
    assert_eq!(
        provided(src),
        triples(&[
            ("GET /", "index", "actix"),
            ("POST /api/items", "create", "actix"),
            ("GET /api/items/{}", "index", "actix"),
            ("DELETE /api/items/{}", "index", "actix"),
            ("* /api/any", "app", "actix"),
        ])
    );
}

#[test]
fn axum_routes_and_nest() {
    let src = r#"
use axum::{routing::{get, post}, Router};
async fn list() {}
async fn create() {}
mod handlers { pub async fn show() {} }
fn router(state: S) -> Router {
    Router::new()
        .route("/users", get(list).post(create))
        .route("/users/:id", axum::routing::get(handlers::show))
        .route("/any", axum::routing::any(|| async {}))
        .nest("/api/v1", Router::new().route("/items/{id}", get(list)))
        .nest("/other", other_router())
        .with_state(state)
}
"#;
    assert_eq!(
        provided(src),
        triples(&[
            ("GET /users", "list", "axum"),
            ("POST /users", "create", "axum"),
            ("GET /users/{}", "handlers.show", "axum"),
            ("* /any", "router", "axum"),
            ("GET /api/v1/items/{}", "list", "axum"),
        ])
    );
}

#[test]
fn reqwest_consumers() {
    let src = r#"
use reqwest::{Client, Method};
async fn calls(client: &Client, base: &str, id: u64) {
    reqwest::get("https://api.example.com/v1/status?x=1").await;
    client.post(format!("{}/api/generate", base)).send().await;
    client.get(format!("{base}/collections/{id}")).send().await;
    client.request(Method::PUT, &format!("{base}/collections/{id}/points")).send().await;
    self.http_client.delete(base.to_string() + "/x/" + &id.to_string()).send().await;
    client.get(url).send().await;
    map.get("/not/a/client");
}
"#;
    assert_eq!(
        keys(src, ContractSide::Consumes, ContractKind::Http),
        triples(&[
            ("GET /v1/status", "calls", "reqwest"),
            ("POST /api/generate", "calls", "reqwest"),
            ("GET /collections/{}", "calls", "reqwest"),
            ("PUT /collections/{}/points", "calls", "reqwest"),
            ("DELETE /x/{}", "calls", "reqwest"),
        ])
    );
}

#[test]
fn tonic_server_impls() {
    let src = r#"
use tonic::{Request, Response, Status};
pub struct PointsService;
#[tonic::async_trait]
impl Points for PointsService {
    async fn upsert(&self, request: Request<UpsertPoints>) -> Result<Response<PointsOperationResponse>, Status> { todo!() }
    async fn query_batch(&self, request: Request<QueryBatchPoints>) -> Result<Response<QueryBatchResponse>, Status> { todo!() }
    fn helper(&self) -> u32 { 0 }
}
impl<T: Points> api::points_server::Points for Wrapper<T> {
    async fn count(&self, request: tonic::Request<CountPoints>) -> Result<tonic::Response<CountResponse>, tonic::Status> { todo!() }
}
impl Interceptor for PoolInterceptor {
    fn call(&mut self, request: Request<()>) -> Result<Request<()>, Status> { Ok(request) }
}
impl Display for PointsService {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result { Ok(()) }
}
"#;
    assert_eq!(
        keys(src, ContractSide::Provides, ContractKind::Rpc),
        triples(&[
            ("Points/Upsert", "PointsService.upsert", "tonic"),
            ("Points/QueryBatch", "PointsService.query_batch", "tonic"),
            ("Points/Count", "Wrapper.count", "tonic"),
        ])
    );
    // Without tonic in the file, nothing.
    let plain = src.replace("tonic", "other");
    assert_eq!(keys(&plain, ContractSide::Provides, ContractKind::Rpc), vec![]);
}

#[test]
fn tonic_clients() {
    let src = r#"
use api::grpc::qdrant::points_internal_client::PointsInternalClient;
use api::grpc::qdrant::raft_client::RaftClient;
struct Remote { raft: RaftClient<Channel> }
impl Remote {
    async fn with_points_client<T, O>(&self, f: impl Fn(PointsInternalClient<Channel>) -> O) -> T { todo!() }
    async fn upsert(&self) {
        self.with_points_client(|mut client| async move {
            client.upsert(tonic::Request::new(req)).await
        }).await;
    }
    async fn peers(&self) {
        self.raft.add_peer_to_known(req).await;
        let mut client = RaftClient::new(channel);
        let client = client.max_decoding_message_size(usize::MAX);
        client.who_is(req).await;
        RaftClient::connect(uri).await?.send_message(m).await;
        self.raft.clone().who_is(req).await.map_err(|e| e).unwrap();
    }
    async fn other(&self, client: reqwest::Client) {
        client.upsert(x).await;
    }
}
"#;
    assert_eq!(
        keys(src, ContractSide::Consumes, ContractKind::Rpc),
        triples(&[
            ("PointsInternal/Upsert", "Remote.upsert", "tonic"),
            ("Raft/AddPeerToKnown", "Remote.peers", "tonic"),
            ("Raft/WhoIs", "Remote.peers", "tonic"),
            ("Raft/SendMessage", "Remote.peers", "tonic"),
            ("Raft/WhoIs", "Remote.peers", "tonic"),
        ])
    );
}

#[test]
fn pascal_names() {
    assert_eq!(super::grpc::pascal("upsert"), "Upsert");
    assert_eq!(super::grpc::pascal("query_batch"), "QueryBatch");
    assert_eq!(super::grpc::pascal("get_v2"), "GetV2");
}
