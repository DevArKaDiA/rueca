//! The service's metrics (RFC 0008 §6.6), rendered as Prometheus text on
//! `/metrics`: `metrics` 0.24 handles over a `metrics-exporter-prometheus`
//! recorder that belongs to the app (not installed globally, so several
//! apps in one process, as in the tests, keep separate registries).
//! Durations are real Prometheus histograms (`_bucket`, `_sum`, `_count`).
//! No repository name is ever a label.

use std::sync::Arc;
use std::time::Duration;

use metrics::{Counter, Gauge, Histogram, Key, Label, Level, Metadata, Recorder, SharedString, Unit};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle, PrometheusRecorder};

/// Bucket bounds (seconds) of every `_seconds` histogram.
const BUCKETS: &[f64] =
    &[0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.15, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0];

const META: Metadata<'static> = Metadata::new("rueca_serve", Level::INFO, Some(module_path!()));

/// Descriptions rendered as `# HELP`.
const HELP: &[(&str, &str)] = &[
    ("rueca_mcp_requests_total", "MCP requests by tool and outcome (ok, error, deadline, denied)"),
    ("rueca_mcp_request_seconds", "MCP tool call latency by tool and scope (repo, repos, org)"),
    ("rueca_auth_failures_total", "Authentication failures by reason (missing, invalid, scope, issuer)"),
    ("rueca_rate_limited_total", "Requests refused by a rate limit, by stage (ip, principal)"),
    ("rueca_generation", "Organization generation served"),
    ("rueca_generation_lag_seconds", "Seconds since the served generation was published"),
    ("rueca_cache_requests_total", "Disk cache lookups by cache and result (hit, miss)"),
    ("rueca_cache_bytes", "Bytes held by the disk cache, by cache"),
    ("rueca_segment_open_seconds", "Time to open and verify a segment"),
    ("rueca_generation_warm_seconds", "Time to warm the engine on a new generation before serving it"),
    ("rueca_store_requests_total", "Object storage requests by operation and result"),
    ("rueca_jobs", "Job objects by kind and state (as of the last discovery)"),
    ("rueca_queue_depth", "Leasable jobs by kind and priority (0 never indexed … 4 tag missing, failed)"),
    ("rueca_job_seconds", "Job run time by kind and result (done, pending, failed, lost, error)"),
    ("rueca_publish_seconds", "Indexing pipeline stage time (fetch, export, index, upload, generation)"),
    ("rueca_cas_conflicts_total", "Compare-and-swap writes lost to another writer, by object (catalog, job, pointer)"),
    ("rueca_gc_deleted_bytes_total", "Bytes deleted by garbage collection, by object class"),
    (
        "rueca_webhook_deliveries_total",
        "Webhook deliveries by host and answer (accepted, ignored, duplicate, unauthorized, too_large, invalid)",
    ),
    (
        "rueca_webhook_confirmations_total",
        "Webhook hints confirmed with ls-remote, by host and result (changed, unchanged, failed)",
    ),
];

/// The app's metric registry.
#[derive(Clone)]
pub struct Metrics {
    recorder: Arc<PrometheusRecorder>,
    handle: PrometheusHandle,
}

impl Default for Metrics {
    fn default() -> Metrics {
        Metrics::new()
    }
}

impl Metrics {
    pub fn new() -> Metrics {
        let builder = PrometheusBuilder::new();
        let builder = match builder.set_buckets_for_metric(Matcher::Suffix("_seconds".into()), BUCKETS) {
            Ok(b) => b,
            // Unreachable: BUCKETS is not empty.
            Err(_) => PrometheusBuilder::new(),
        };
        let recorder = builder.build_recorder();
        for (name, help) in HELP {
            let unit = name.ends_with("_seconds").then_some(Unit::Seconds);
            recorder.describe_counter((*name).into(), unit, SharedString::from(*help));
        }
        let handle = recorder.handle();
        Metrics { recorder: Arc::new(recorder), handle }
    }

    /// The Prometheus text exposition.
    pub fn render(&self) -> String {
        self.handle.render()
    }

    pub fn counter(&self, name: &'static str, labels: &[(&'static str, &str)]) -> Counter {
        self.recorder.register_counter(&key(name, labels), &META)
    }

    pub fn gauge(&self, name: &'static str, labels: &[(&'static str, &str)]) -> Gauge {
        self.recorder.register_gauge(&key(name, labels), &META)
    }

    pub fn histogram(&self, name: &'static str, labels: &[(&'static str, &str)]) -> Histogram {
        self.recorder.register_histogram(&key(name, labels), &META)
    }

    /// One MCP request: `tool` (`-` for protocol methods), `outcome`,
    /// `scope`, and the tool call's duration when it ran.
    pub fn mcp_request(&self, tool: &str, outcome: &str, scope: &str, took: Option<Duration>) {
        self.counter("rueca_mcp_requests_total", &[("tool", tool), ("outcome", outcome)]).increment(1);
        if let Some(d) = took {
            self.histogram("rueca_mcp_request_seconds", &[("tool", tool), ("scope", scope)]).record(d.as_secs_f64());
        }
    }

    pub fn auth_failure(&self, reason: &str) {
        self.counter("rueca_auth_failures_total", &[("reason", reason)]).increment(1);
    }

    pub fn rate_limited(&self, stage: &str) {
        self.counter("rueca_rate_limited_total", &[("stage", stage)]).increment(1);
    }

    pub fn cache_requests(&self, cache: &str, result: &str, n: u64) {
        if n > 0 {
            self.counter("rueca_cache_requests_total", &[("cache", cache), ("result", result)]).increment(n);
        }
    }
}

fn key(name: &'static str, labels: &[(&'static str, &str)]) -> Key {
    let labels: Vec<Label> = labels.iter().map(|(k, v)| Label::new(*k, v.to_string())).collect();
    Key::from_parts(name, labels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_counters_and_histograms() {
        let m = Metrics::new();
        m.mcp_request("search_symbols", "ok", "org", Some(Duration::from_millis(3)));
        m.auth_failure("missing");
        let text = m.render();
        assert!(text.contains(r#"rueca_mcp_requests_total{tool="search_symbols",outcome="ok"} 1"#), "{text}");
        assert!(text.contains("rueca_mcp_request_seconds_bucket"), "{text}");
        assert!(text.contains("# HELP rueca_auth_failures_total"), "{text}");
        // A second registry is independent.
        assert!(!Metrics::new().render().contains("rueca_mcp_requests_total{"));
    }
}
