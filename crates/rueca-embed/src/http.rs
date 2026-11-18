//! The HTTP agent shared by `rueca models pull` and the remote providers:
//! rustls + ring with the operating system's trust store.

use std::sync::Arc;
use std::time::Duration;

use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

/// An agent with a 30 s connect timeout. `recv_body`: the timeout for
/// reading a response body (`None` for long downloads). `status_as_error`:
/// whether 4xx / 5xx answers are errors (`false` lets the caller read the
/// body and `Retry-After`).
pub(crate) fn agent(recv_body: Option<Duration>, status_as_error: bool) -> ureq::Agent {
    let tls = TlsConfig::builder()
        .provider(TlsProvider::Rustls)
        .root_certs(RootCerts::PlatformVerifier)
        .unversioned_rustls_crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_body(recv_body)
        .http_status_as_error(status_as_error)
        .build()
        .into()
}
