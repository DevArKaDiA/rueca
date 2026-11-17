//! Logs of `rueca serve` (RFC 0008 §6.6): `tracing` to stdout, one JSON
//! object per line (`[telemetry].log_format = "json"`, the default) or the
//! human format (`"pretty"`), filtered by `RUECA_LOG` (default `info`).
//! Fields named `token`, `secret`, `authorization` or `password` (or ending
//! in `_token`, `_secret`, `_password`) are written as `"[redacted]"`.
//! With the `otel` feature and `[telemetry].otlp_endpoint`, spans are also
//! exported over OTLP/HTTP ([`crate::otel`]).

use std::fmt;

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::field::RecordFields;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::{FormatTime, SystemTime};
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields, FormattedFields};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

use crate::config::{LogFormat, TelemetryConfig};

const REDACTED: &str = "[redacted]";

/// Whether a field's value must never be logged.
pub fn is_sensitive(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["token", "secret", "authorization", "password"].contains(&n.as_str())
        || ["_token", "_secret", "_password"].iter().any(|s| n.ends_with(s))
}

/// Keeps the exporter alive; flushes it on drop.
#[must_use = "dropping the guard stops trace export"]
pub struct LogGuard {
    #[cfg(feature = "otel")]
    _otel: Option<crate::otel::OtelGuard>,
}

/// Installs the global subscriber for `rueca serve`. `filter` is
/// `RUECA_LOG` (default `info`). A second call keeps the first subscriber.
pub fn init_logging(telemetry: &TelemetryConfig, filter: Option<&str>) -> LogGuard {
    let filter = filter.and_then(|f| EnvFilter::try_new(f).ok()).unwrap_or_else(|| EnvFilter::new("info"));
    let fmt_layer = match telemetry.log_format {
        LogFormat::Json => tracing_subscriber::fmt::layer()
            .with_writer(std::io::stdout)
            .fmt_fields(JsonFields)
            .event_format(JsonEvent)
            .boxed(),
        LogFormat::Pretty => tracing_subscriber::fmt::layer()
            .with_writer(std::io::stdout)
            .with_target(false)
            .fmt_fields(PlainFields)
            .boxed(),
    };
    #[cfg(feature = "otel")]
    {
        let (otel_layer, guard) = crate::otel::layer(telemetry.otlp_endpoint.as_deref());
        let _ = tracing_subscriber::registry().with(filter).with(fmt_layer).with(otel_layer).try_init();
        LogGuard { _otel: guard }
    }
    #[cfg(not(feature = "otel"))]
    {
        let _ = tracing_subscriber::registry().with(filter).with(fmt_layer).try_init();
        if telemetry.otlp_endpoint.is_some() {
            tracing::warn!(
                "telemetry.otlp_endpoint is set but this build has no `otel` feature: traces are not exported"
            );
        }
        LogGuard {}
    }
}

/// Collects fields into a JSON object, redacting sensitive ones.
#[derive(Default)]
struct JsonVisitor(Map<String, Value>);

impl JsonVisitor {
    fn put(&mut self, field: &Field, value: Value) {
        let v = if is_sensitive(field.name()) { Value::from(REDACTED) } else { value };
        self.0.insert(field.name().to_string(), v);
    }
}

impl Visit for JsonVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field, Value::from(value));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field, Value::from(value));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field, Value::from(value));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field, Value::from(value));
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.put(field, Value::from(value));
    }
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.put(field, Value::from(value.to_string()));
    }
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.put(field, Value::from(format!("{value:?}")));
    }
}

/// Span fields as a JSON object (stored in the span's extensions).
pub struct JsonFields;

impl<'w> FormatFields<'w> for JsonFields {
    fn format_fields<R: RecordFields>(&self, mut writer: Writer<'w>, fields: R) -> fmt::Result {
        let mut v = JsonVisitor::default();
        fields.record(&mut v);
        write!(writer, "{}", Value::Object(v.0))
    }

    fn add_fields(&self, current: &'w mut FormattedFields<Self>, fields: &tracing::span::Record<'_>) -> fmt::Result {
        let mut v = JsonVisitor(match serde_json::from_str(&current.fields) {
            Ok(Value::Object(m)) => m,
            _ => Map::new(),
        });
        fields.record(&mut v);
        current.fields = Value::Object(v.0).to_string();
        Ok(())
    }
}

/// One JSON object per event: `timestamp`, `level`, `target`, the event's
/// fields, and `spans` (outermost first, each with its `name` and fields).
pub struct JsonEvent;

impl<S, N> FormatEvent<S, N> for JsonEvent
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(&self, ctx: &FmtContext<'_, S, N>, mut writer: Writer<'_>, event: &Event<'_>) -> fmt::Result {
        let mut ts = String::new();
        SystemTime.format_time(&mut Writer::new(&mut ts))?;
        let meta = event.metadata();
        let mut out = Map::new();
        out.insert("timestamp".into(), Value::from(ts));
        out.insert("level".into(), Value::from(meta.level().as_str()));
        out.insert("target".into(), Value::from(meta.target()));
        let mut fields = JsonVisitor::default();
        event.record(&mut fields);
        out.extend(fields.0);
        if let Some(scope) = ctx.event_scope() {
            let spans: Vec<Value> = scope.from_root().map(|span| span_json(&span)).collect();
            out.insert("spans".into(), Value::Array(spans));
        }
        writeln!(writer, "{}", Value::Object(out))
    }
}

fn span_json<R: for<'a> LookupSpan<'a>>(span: &tracing_subscriber::registry::SpanRef<'_, R>) -> Value {
    let mut m = Map::new();
    m.insert("name".into(), Value::from(span.name()));
    let ext = span.extensions();
    if let Some(f) = ext.get::<FormattedFields<JsonFields>>()
        && let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(&f.fields)
    {
        m.extend(fields);
    }
    Value::Object(m)
}

/// `key=value` fields for the human format, redacting sensitive ones.
pub struct PlainFields;

struct PlainVisitor<'a, 'w> {
    writer: &'a mut Writer<'w>,
    result: fmt::Result,
    first: bool,
}

impl Visit for PlainVisitor<'_, '_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.record_debug(field, &format_args!("{value}"));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if self.result.is_err() {
            return;
        }
        let sep = if self.first { "" } else { " " };
        self.first = false;
        self.result = if field.name() == "message" {
            write!(self.writer, "{sep}{value:?}")
        } else if is_sensitive(field.name()) {
            write!(self.writer, "{sep}{}={REDACTED}", field.name())
        } else {
            write!(self.writer, "{sep}{}={value:?}", field.name())
        };
    }
}

impl<'w> FormatFields<'w> for PlainFields {
    fn format_fields<R: RecordFields>(&self, mut writer: Writer<'w>, fields: R) -> fmt::Result {
        let mut v = PlainVisitor { writer: &mut writer, result: Ok(()), first: true };
        fields.record(&mut v);
        v.result
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::fmt::MakeWriter;

    use super::*;

    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Buf {
        type Writer = Buf;
        fn make_writer(&'a self) -> Buf {
            self.clone()
        }
    }

    #[test]
    fn sensitive_names() {
        for n in ["token", "Authorization", "client_secret", "access_token", "password"] {
            assert!(is_sensitive(n), "{n}");
        }
        for n in ["principal", "tool", "tokens_used"] {
            assert!(!is_sensitive(n), "{n}");
        }
    }

    #[test]
    fn json_lines_redact_and_carry_spans() {
        let buf = Buf::default();
        let sub = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer().with_writer(buf.clone()).fmt_fields(JsonFields).event_format(JsonEvent),
        );
        tracing::subscriber::with_default(sub, || {
            let span = tracing::info_span!("mcp", tool = "search_symbols", outcome = tracing::field::Empty);
            let _e = span.enter();
            span.record("outcome", "ok");
            tracing::info!(token = "rueca_secret", authorization = "Bearer x", n = 3, "hello");
        });
        let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(!text.contains("rueca_secret") && !text.contains("Bearer x"), "{text}");
        let v: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(v["message"], "hello");
        assert_eq!(v["token"], REDACTED);
        assert_eq!(v["n"], 3);
        assert_eq!(v["spans"][0]["name"], "mcp");
        assert_eq!(v["spans"][0]["outcome"], "ok");
    }

    #[test]
    fn plain_fields_redact() {
        let buf = Buf::default();
        let sub = tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer().with_writer(buf.clone()).with_ansi(false).fmt_fields(PlainFields));
        tracing::subscriber::with_default(sub, || tracing::info!(password = "hunter2", user = "ana", "login"));
        let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        assert!(text.contains("login password=[redacted] user=ana"), "{text}");
    }
}
