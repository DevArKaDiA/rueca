//! OpenTelemetry traces (feature `otel`, off by default; RFC 0008 §6.6):
//! `tracing` spans exported over OTLP/HTTP (protobuf) to
//! `[telemetry].otlp_endpoint` (`<endpoint>/v1/traces`) by the SDK's batch
//! processor. The exporter's HTTP client is `ureq` (already in the tree),
//! not hyper or reqwest. A request's `traceparent` (`_meta`, SEP-414)
//! parents its `mcp` span ([`set_parent`]). The traces API, SDK and OTLP
//! exporter of OpenTelemetry Rust are Beta.

use std::collections::HashMap;

use opentelemetry::propagation::TextMapPropagator;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_http::{Bytes, HttpClient, HttpError, Request, Response};
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{SdkTracer, SdkTracerProvider};
use tracing::Subscriber;
use tracing_opentelemetry::{OpenTelemetryLayer, OpenTelemetrySpanExt};
use tracing_subscriber::registry::LookupSpan;

/// Flushes and stops the exporter on drop.
pub struct OtelGuard(SdkTracerProvider);

impl Drop for OtelGuard {
    fn drop(&mut self) {
        if let Err(e) = self.0.shutdown() {
            eprintln!("rueca: OpenTelemetry shutdown: {e}");
        }
    }
}

/// The layer exporting spans to `endpoint` (none without one, or when the
/// exporter cannot be built: logged, not fatal).
pub fn layer<S>(endpoint: Option<&str>) -> (Option<OpenTelemetryLayer<S, SdkTracer>>, Option<OtelGuard>)
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    let Some(endpoint) = endpoint else { return (None, None) };
    let agent = match crate::oidc::http_agent(endpoint.starts_with("https://")) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("rueca: OTLP exporter for {endpoint}: {e:#}; traces are not exported");
            return (None, None);
        }
    };
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_http_client(UreqClient(agent))
        .with_endpoint(format!("{}/v1/traces", endpoint.trim_end_matches('/')))
        .build();
    let exporter = match exporter {
        Ok(e) => e,
        Err(e) => {
            eprintln!("rueca: OTLP exporter for {endpoint}: {e}; traces are not exported");
            return (None, None);
        }
    };
    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(Resource::builder().with_service_name("rueca").build())
        .build();
    let tracer = provider.tracer("rueca-serve");
    (Some(tracing_opentelemetry::layer().with_tracer(tracer)), Some(OtelGuard(provider)))
}

/// Parents `span` on a W3C `traceparent`.
pub fn set_parent(span: &tracing::Span, traceparent: &str) {
    let carrier = HashMap::from([("traceparent".to_string(), traceparent.to_string())]);
    let cx = TraceContextPropagator::new().extract(&carrier);
    let _ = span.set_parent(cx);
}

/// OTLP/HTTP over `ureq` (blocking: the SDK's batch processor runs it on its
/// own thread).
#[derive(Debug)]
struct UreqClient(ureq::Agent);

#[async_trait::async_trait]
impl HttpClient for UreqClient {
    async fn send_bytes(&self, request: Request<Bytes>) -> Result<Response<Bytes>, HttpError> {
        let (parts, body) = request.into_parts();
        let mut resp = self.0.run(Request::from_parts(parts, body.to_vec()))?;
        let mut out = Response::builder().status(resp.status());
        for (k, v) in resp.headers() {
            out = out.header(k, v);
        }
        let bytes = resp.body_mut().read_to_vec()?;
        Ok(out.body(Bytes::from(bytes))?)
    }
}
