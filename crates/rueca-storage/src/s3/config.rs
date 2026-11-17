//! `s3://` / `gs://` configuration: URL, options map, environment.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use anyhow::{Context, bail};

/// Which S3-compatible API a store speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavour {
    /// Amazon S3 and compatibles (MinIO, R2, LocalStack): `If-None-Match` /
    /// `If-Match` conditions.
    S3,
    /// Google Cloud Storage's XML API with HMAC keys: generation conditions.
    Gcs,
}

/// Static request-signing credentials.
#[derive(Clone)]
pub struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("access_key_id", &self.access_key_id)
            .field("secret_access_key", &"<redacted>")
            .field("session_token", &self.session_token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// Where credentials come from, asked before every request so a provider
/// can rotate them. Phase 0 ships [`StaticCredentials`]; phase 2 adds the
/// instance metadata service, IRSA and pod identity behind this trait.
pub trait CredentialsProvider: Send + Sync + 'static {
    fn credentials(&self) -> anyhow::Result<Credentials>;
}

/// Credentials fixed at open time (options or environment).
#[derive(Clone, Debug)]
pub struct StaticCredentials(pub Credentials);

impl CredentialsProvider for StaticCredentials {
    fn credentials(&self) -> anyhow::Result<Credentials> {
        Ok(self.0.clone())
    }
}

/// Everything [`super::S3Storage`] needs, resolved.
pub(crate) struct S3Config {
    pub flavour: Flavour,
    pub bucket: String,
    /// Empty, or ends with `/`.
    pub prefix: String,
    pub region: String,
    pub endpoint: String,
    pub virtual_hosted: bool,
    pub credentials: Option<Arc<dyn CredentialsProvider>>,
}

/// Accepted option keys, after dropping an `aws_` / `google_` prefix.
const KEYS: &[&str] = &[
    "access_key_id",
    "secret_access_key",
    "session_token",
    "region",
    "endpoint",
    "allow_http",
    "virtual_hosted_style_request",
];

/// A source of settings: the options map first, then the environment.
pub(crate) struct Settings<'a> {
    options: BTreeMap<String, &'a str>,
    env: &'a dyn Fn(&str) -> Option<String>,
}

impl<'a> Settings<'a> {
    pub fn new(
        options: &'a BTreeMap<String, String>,
        env: &'a dyn Fn(&str) -> Option<String>,
    ) -> anyhow::Result<Settings<'a>> {
        let mut normalized = BTreeMap::new();
        for (key, value) in options {
            let short = key.strip_prefix("aws_").or_else(|| key.strip_prefix("google_")).unwrap_or(key);
            let short = if short == "endpoint_url" { "endpoint" } else { short };
            if !KEYS.contains(&short) {
                bail!("unknown store option `{key}` (accepted: {})", KEYS.join(", "));
            }
            normalized.insert(short.to_string(), value.as_str());
        }
        Ok(Settings { options: normalized, env })
    }

    /// Option `key`, else the first set environment variable of `vars`.
    fn get(&self, key: &str, vars: &[&str]) -> Option<String> {
        if let Some(v) = self.options.get(key) {
            return Some((*v).to_string());
        }
        vars.iter().find_map(|var| (self.env)(var).filter(|v| !v.is_empty()))
    }

    fn flag(&self, key: &str, vars: &[&str]) -> anyhow::Result<Option<bool>> {
        let Some(raw) = self.get(key, vars) else { return Ok(None) };
        match raw.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Ok(Some(true)),
            "false" | "0" | "no" => Ok(Some(false)),
            _ => bail!("store option `{key}`: expected true or false, got `{raw}`"),
        }
    }
}

impl S3Config {
    /// `rest` is the URL after `s3://` / `gs://`: `bucket[/prefix]`.
    pub fn resolve(flavour: Flavour, rest: &str, settings: &Settings<'_>) -> anyhow::Result<S3Config> {
        let (bucket, prefix) = split_bucket(rest)?;
        let region = match flavour {
            Flavour::S3 => settings.get("region", &["AWS_REGION", "AWS_DEFAULT_REGION"]),
            Flavour::Gcs => settings.get("region", &[]),
        };
        let region = region.unwrap_or_else(|| default_region(flavour).to_string());
        let custom = endpoint_setting(flavour, settings);
        let endpoint = custom.clone().unwrap_or_else(|| default_endpoint(flavour, &region));
        check_scheme(&endpoint, settings)?;
        // AWS's own endpoint: virtual-hosted; custom endpoints and GCS: path style.
        let default_virtual = flavour == Flavour::S3 && custom.is_none();
        let virtual_hosted = settings
            .flag("virtual_hosted_style_request", &["AWS_VIRTUAL_HOSTED_STYLE_REQUEST"])?
            .unwrap_or(default_virtual);
        let credentials = credentials(flavour, settings)?;
        Ok(S3Config { flavour, bucket, prefix, region, endpoint, virtual_hosted, credentials })
    }
}

fn split_bucket(rest: &str) -> anyhow::Result<(String, String)> {
    let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
    if bucket.is_empty() {
        bail!("store URL has no bucket name");
    }
    let prefix = prefix.trim_matches('/');
    if !prefix.is_empty() {
        crate::check_path(prefix).context("store URL prefix")?;
    }
    let prefix = if prefix.is_empty() { String::new() } else { format!("{prefix}/") };
    Ok((bucket.to_string(), prefix))
}

fn default_region(flavour: Flavour) -> &'static str {
    match flavour {
        Flavour::S3 => "us-east-1",
        Flavour::Gcs => "auto",
    }
}

fn endpoint_setting(flavour: Flavour, settings: &Settings<'_>) -> Option<String> {
    let vars: &[&str] = match flavour {
        Flavour::S3 => &["AWS_ENDPOINT_URL_S3", "AWS_ENDPOINT_URL"],
        Flavour::Gcs => &[],
    };
    settings.get("endpoint", vars).map(|e| e.trim_end_matches('/').to_string())
}

fn default_endpoint(flavour: Flavour, region: &str) -> String {
    match flavour {
        Flavour::S3 => format!("https://s3.{region}.amazonaws.com"),
        Flavour::Gcs => "https://storage.googleapis.com".to_string(),
    }
}

fn check_scheme(endpoint: &str, settings: &Settings<'_>) -> anyhow::Result<()> {
    if endpoint.starts_with("https://") {
        return Ok(());
    }
    if !endpoint.starts_with("http://") {
        bail!("store endpoint `{endpoint}` must be an http:// or https:// URL");
    }
    if settings.flag("allow_http", &["AWS_ALLOW_HTTP"])? != Some(true) {
        bail!("store endpoint `{endpoint}` is plain HTTP: set the option allow_http=true to use it");
    }
    Ok(())
}

fn credentials(flavour: Flavour, settings: &Settings<'_>) -> anyhow::Result<Option<Arc<dyn CredentialsProvider>>> {
    let (key_vars, secret_vars): (&[&str], &[&str]) = match flavour {
        Flavour::S3 => (&["AWS_ACCESS_KEY_ID"], &["AWS_SECRET_ACCESS_KEY"]),
        Flavour::Gcs => {
            (&["GOOGLE_HMAC_KEY_ID", "AWS_ACCESS_KEY_ID"], &["GOOGLE_HMAC_SECRET", "AWS_SECRET_ACCESS_KEY"])
        }
    };
    let key = settings.get("access_key_id", key_vars);
    let secret = settings.get("secret_access_key", secret_vars);
    let token_vars: &[&str] = if flavour == Flavour::S3 { &["AWS_SESSION_TOKEN"] } else { &[] };
    let token = settings.get("session_token", token_vars);
    match (key, secret) {
        (Some(access_key_id), Some(secret_access_key)) => {
            let creds = Credentials { access_key_id, secret_access_key, session_token: token };
            Ok(Some(Arc::new(StaticCredentials(creds))))
        }
        (None, None) => Ok(None),
        _ => bail!("store credentials need both access_key_id and secret_access_key"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(flavour: Flavour, rest: &str, opts: &[(&str, &str)], env: &[(&str, &str)]) -> anyhow::Result<S3Config> {
        let options: BTreeMap<String, String> = opts.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let env: Vec<(String, String)> = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let lookup = move |name: &str| env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
        let settings = Settings::new(&options, &lookup)?;
        S3Config::resolve(flavour, rest, &settings)
    }

    #[test]
    fn aws_defaults() {
        let c = resolve(Flavour::S3, "bkt/a/b/", &[], &[]).unwrap();
        assert_eq!((c.bucket.as_str(), c.prefix.as_str()), ("bkt", "a/b/"));
        assert_eq!(c.region, "us-east-1");
        assert_eq!(c.endpoint, "https://s3.us-east-1.amazonaws.com");
        assert!(c.virtual_hosted);
        assert!(c.credentials.is_none());
    }

    #[test]
    fn options_win_over_environment() {
        let env = [("AWS_REGION", "eu-west-1"), ("AWS_ACCESS_KEY_ID", "envkey"), ("AWS_SECRET_ACCESS_KEY", "s")];
        let c = resolve(Flavour::S3, "b", &[("aws_region", "us-west-2")], &env).unwrap();
        assert_eq!(c.region, "us-west-2");
        let creds = c.credentials.unwrap().credentials().unwrap();
        assert_eq!(creds.access_key_id, "envkey");
        assert!(!format!("{creds:?}").contains("\"s\""));
    }

    #[test]
    fn custom_endpoint_is_path_style_and_http_needs_opt_in() {
        let err = resolve(Flavour::S3, "b", &[("endpoint", "http://localhost:9000")], &[]);
        assert!(err.err().unwrap().to_string().contains("allow_http"));
        let opts = [("endpoint", "http://localhost:9000/"), ("allow_http", "true")];
        let c = resolve(Flavour::S3, "b", &opts, &[]).unwrap();
        assert_eq!(c.endpoint, "http://localhost:9000");
        assert!(!c.virtual_hosted);
    }

    #[test]
    fn gcs_defaults_and_hmac_env() {
        let env = [("GOOGLE_HMAC_KEY_ID", "GOOG1"), ("GOOGLE_HMAC_SECRET", "sec")];
        let c = resolve(Flavour::Gcs, "b/p", &[], &env).unwrap();
        assert_eq!(c.endpoint, "https://storage.googleapis.com");
        assert_eq!(c.region, "auto");
        assert!(!c.virtual_hosted);
        assert_eq!(c.credentials.unwrap().credentials().unwrap().access_key_id, "GOOG1");
    }

    #[test]
    fn rejects_bad_input() {
        assert!(resolve(Flavour::S3, "b", &[("aws_regoin", "x")], &[]).is_err());
        assert!(resolve(Flavour::S3, "", &[], &[]).is_err());
        assert!(resolve(Flavour::S3, "b/../x", &[], &[]).is_err());
        assert!(resolve(Flavour::S3, "b", &[("access_key_id", "k")], &[]).is_err());
        assert!(resolve(Flavour::S3, "b", &[("allow_http", "maybe"), ("endpoint", "http://x")], &[]).is_err());
    }
}
