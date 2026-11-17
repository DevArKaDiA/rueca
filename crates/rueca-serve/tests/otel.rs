//! Feature `otel`: spans reach an OTLP/HTTP collector (a fake one,
//! in-process), parented on a `traceparent`.

#![cfg(feature = "otel")]

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::http::HeaderMap;
use axum::routing::post;
use tracing_subscriber::layer::SubscriberExt;

#[test]
fn spans_are_exported_over_otlp_http() {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let seen: Arc<Mutex<Vec<(String, usize)>>> = Arc::default();
    let s = seen.clone();
    let app = Router::new().route(
        "/v1/traces",
        post(move |headers: HeaderMap, body: Bytes| {
            let s = s.clone();
            async move {
                let ct = headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                s.lock().unwrap().push((ct, body.len()));
                ""
            }
        }),
    );
    let listener = rt.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    rt.spawn(async move { axum::serve(listener, app).await.unwrap() });

    let (layer, guard) = rueca_serve::otel::layer(Some(&endpoint));
    let subscriber = tracing_subscriber::registry().with(layer);
    tracing::subscriber::with_default(subscriber, || {
        let span = tracing::info_span!("mcp", tool = "search_symbols");
        rueca_serve::otel::set_parent(&span, "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01");
        span.in_scope(|| tracing::info!("inside"));
    });
    drop(guard);
    let seen = seen.lock().unwrap().clone();
    assert!(!seen.is_empty(), "no export reached the collector");
    assert_eq!(seen[0].0, "application/x-protobuf");
    assert!(seen[0].1 > 0);
    rt.shutdown_background();
}
