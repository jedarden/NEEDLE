//! OTLP sink for OpenTelemetry export.
//!
//! Exports telemetry as OpenTelemetry signals (traces, metrics, logs) over OTLP
//! to any compliant collector.

use crate::config::OtlpSinkConfig;
use crate::telemetry::TelemetryEvent;
use anyhow::{Context, Result};
use opentelemetry::logs::{AnyValue, Logger, LogRecord, LoggerProvider, Severity};
use opentelemetry::metrics::{Counter, Histogram, MeterProvider, UpDownCounter};
use opentelemetry::KeyValue;
use opentelemetry_sdk::logs::{BatchLogProcessor, SdkLoggerProvider};
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::resource::{Resource, SdkProvidedResourceDetector, TelemetryResourceDetector};
use opentelemetry_sdk::trace::{BatchSpanProcessor, SdkTracerProvider};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// OTLP sink implementing the telemetry Sink trait.
///
/// Wraps the OpenTelemetry SDK providers and translates TelemetryEvent
/// into traces, metrics, and logs per the semantic mapping.
#[derive(Clone)]
pub struct OtlpSink {
    /// Tracer provider for trace export.
    tracer_provider: Arc<SdkTracerProvider>,
    /// Meter provider for metric export.
    meter_provider: Arc<SdkMeterProvider>,
    /// Logger provider for log export.
    logger_provider: Arc<SdkLoggerProvider>,
    /// Cached metric instruments.
    metrics: Metrics,
}

/// Cached metric instruments for efficient recording.
#[derive(Clone)]
struct Metrics {
    /// Counter: `needle.beads.completed`
    beads_completed: Counter<u64>,
    /// Histogram: `needle.beads.duration`
    bead_duration: Histogram<f64>,
    /// Histogram: `needle.strand.duration`
    strand_duration: Histogram<f64>,
    /// Histogram: `needle.agent.duration`
    agent_duration: Histogram<f64>,
    /// Counter: `needle.claim.attempts`
    claim_attempts: Counter<u64>,
    /// Counter: `needle.agent.tokens.input`
    tokens_input: Counter<u64>,
    /// Counter: `needle.agent.tokens.output`
    tokens_output: Counter<u64>,
    /// Counter: `needle.cost.usd`
    cost_usd: Counter<f64>,
    /// UpDownCounter: `needle.beads.claimed`
    beads_claimed: UpDownCounter<i64>,
}

impl OtlpSink {
    /// Create a new OTLP sink from configuration.
    ///
    /// Initializes the OpenTelemetry SDK with batch processors for
    /// non-blocking export of all three signals.
    pub fn new(worker_id: String, session_id: String, config: &OtlpSinkConfig) -> Result<Self> {
        // Build resource attributes from config + computed attributes
        let resource = Self::build_resource(&worker_id, &session_id, config)?;

        // Build exporters based on protocol
        let (tracer_provider, meter_provider, logger_provider) =
            match config.protocol.as_str() {
                "grpc" => Self::build_grpc_providers(config, &resource)?,
                "http" | "http/protobuf" => Self::build_http_providers(config, &resource)?,
                other => anyhow::bail!("invalid OTLP protocol: {other}, must be 'grpc' or 'http'"),
            };

        // Build metric instruments
        let meter = meter_provider.meter("needle");
        let metrics = Metrics {
            beads_completed: meter
                .u64_counter("needle.beads.completed")
                .with_description("Bead terminal outcomes (one per bead.outcome)")
                .build(),
            bead_duration: meter
                .f64_histogram("needle.beads.duration")
                .with_unit("ms")
                .with_description("End-to-end bead lifecycle time")
                .build(),
            strand_duration: meter
                .f64_histogram("needle.strand.duration")
                .with_unit("ms")
                .with_description("Strand evaluation time")
                .build(),
            agent_duration: meter
                .f64_histogram("needle.agent.duration")
                .with_unit("ms")
                .with_description("Agent process runtime")
                .build(),
            claim_attempts: meter
                .u64_counter("needle.claim.attempts")
                .with_description("Claim attempts")
                .build(),
            tokens_input: meter
                .u64_counter("needle.agent.tokens.input")
                .with_unit("{token}")
                .with_description("Input tokens consumed")
                .build(),
            tokens_output: meter
                .u64_counter("needle.agent.tokens.output")
                .with_unit("{token}")
                .with_description("Output tokens produced")
                .build(),
            cost_usd: meter
                .f64_counter("needle.cost.usd")
                .with_unit("USD")
                .with_description("Estimated cost accumulator")
                .build(),
            beads_claimed: meter
                .i64_up_down_counter("needle.beads.claimed")
                .with_unit("{bead}")
                .with_description("Current claimed bead count")
                .build(),
        };

        Ok(OtlpSink {
            tracer_provider: Arc::new(tracer_provider),
            meter_provider: Arc::new(meter_provider),
            logger_provider: Arc::new(logger_provider),
            metrics,
        })
    }

    /// Build the OTel Resource with config and computed attributes.
    fn build_resource(worker_id: &str, session_id: &str, config: &OtlpSinkConfig) -> Result<Resource> {
        let mut builder = Resource::builder()
            .with_attributes([
                KeyValue::new("service.name", "needle"),
                KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
                KeyValue::new("service.instance.id", worker_id.to_string()),
                KeyValue::new("needle.session_id", session_id.to_string()),
            ]);

        // Add hostname from OS
        if let Some(hostname) = gethostname::gethostname().to_str() {
            builder = builder.with_attributes([KeyValue::new("host.name", hostname.to_string())]);
        }

        // Add process PID
        builder = builder.with_attributes([KeyValue::new("process.pid", std::process::id().to_string())]);

        // Add resource attributes from config (KEY=VALUE pairs)
        for attr_str in &config.resource_attributes {
            if let Some((key, value)) = attr_str.split_once('=') {
                builder = builder.with_attributes([KeyValue::new(key.to_string(), value.to_string())]);
            }
        }

        // Detect SDK-provided attributes (telemetry.sdk.*)
        let resource = builder
            .with_detector(Box::new(TelemetryResourceDetector))
            .with_detector(Box::new(SdkProvidedResourceDetector))
            .build();

        Ok(resource)
    }

    /// Build providers using gRPC transport (tonic).
    fn build_grpc_providers(
        config: &OtlpSinkConfig,
        resource: &Resource,
    ) -> Result<(SdkTracerProvider, SdkMeterProvider, SdkLoggerProvider)> {
        use opentelemetry_otlp::{LogExporter, MetricExporter, SpanExporter, WithExportConfig, WithTonicConfig};
        use tonic::metadata::{MetadataKey, MetadataMap, MetadataValue};

        let timeout = Duration::from_secs(config.timeout_secs);

        // Build metadata map from headers
        let mut metadata = MetadataMap::new();
        for header in &config.headers {
            if let Some((key, value)) = header.split_once(": ") {
                if let Ok(key_val) = MetadataKey::from_bytes(key.as_bytes()) {
                    if let Ok(metadata_value) = MetadataValue::try_from(value) {
                        metadata.insert(key_val, metadata_value);
                    }
                }
            } else if let Some((key, value)) = header.split_once(':') {
                if let Ok(key_val) = MetadataKey::from_bytes(key.as_bytes()) {
                    if let Ok(metadata_value) = MetadataValue::try_from(value) {
                        metadata.insert(key_val, metadata_value);
                    }
                }
            }
        }

        // Build span exporter with tonic config
        let span_exporter = SpanExporter::builder()
            .with_tonic()
            .with_endpoint(config.endpoint.clone())
            .with_timeout(timeout)
            .with_metadata(metadata.clone())
            .build()?;

        // Use BatchSpanProcessor for traces (required by spec)
        let batch_span_processor = BatchSpanProcessor::builder(span_exporter).build();

        let tracer_provider = SdkTracerProvider::builder()
            .with_span_processor(batch_span_processor)
            .with_resource(resource.clone())
            .build();

        // Build metric exporter with tonic config
        let metric_exporter = MetricExporter::builder()
            .with_tonic()
            .with_endpoint(config.endpoint.clone())
            .with_timeout(timeout)
            .with_metadata(metadata.clone())
            .build()?;

        // Use PeriodicReader for metrics with 10s export interval (required by spec)
        let metric_reader = PeriodicReader::builder(metric_exporter)
            .with_interval(Duration::from_secs(config.metrics_interval_secs))
            .build();

        let meter_provider = SdkMeterProvider::builder()
            .with_reader(metric_reader)
            .with_resource(resource.clone())
            .build();

        // Build log exporter with tonic config
        let log_exporter = LogExporter::builder()
            .with_tonic()
            .with_endpoint(config.endpoint.clone())
            .with_timeout(timeout)
            .with_metadata(metadata)
            .build()?;

        // Use BatchLogProcessor for logs (required by spec)
        let batch_log_processor = BatchLogProcessor::builder(log_exporter).build();

        let logger_provider = SdkLoggerProvider::builder()
            .with_log_processor(batch_log_processor)
            .with_resource(resource.clone())
            .build();

        Ok((
            tracer_provider,
            meter_provider,
            logger_provider,
        ))
    }

    /// Build providers using HTTP/protobuf transport (reqwest).
    fn build_http_providers(
        config: &OtlpSinkConfig,
        resource: &Resource,
    ) -> Result<(SdkTracerProvider, SdkMeterProvider, SdkLoggerProvider)> {
        use opentelemetry_otlp::{LogExporter, MetricExporter, SpanExporter, WithExportConfig};
        use opentelemetry_otlp::WithHttpConfig;

        let timeout = Duration::from_secs(config.timeout_secs);

        // Build headers map from config
        let mut headers_map = HashMap::new();
        for header in &config.headers {
            if let Some((key, value)) = header.split_once(": ") {
                headers_map.insert(key.to_string(), value.to_string());
            } else if let Some((key, value)) = header.split_once(':') {
                headers_map.insert(key.to_string(), value.to_string());
            }
        }

        // Build span exporter
        let span_exporter = SpanExporter::builder()
            .with_http()
            .with_endpoint(config.endpoint.clone())
            .with_timeout(timeout)
            .with_headers(headers_map.clone())
            .build()?;

        // Use BatchSpanProcessor for traces (required by spec)
        let batch_span_processor = BatchSpanProcessor::builder(span_exporter).build();

        let tracer_provider = SdkTracerProvider::builder()
            .with_span_processor(batch_span_processor)
            .with_resource(resource.clone())
            .build();

        // Build metric exporter
        let metric_exporter = MetricExporter::builder()
            .with_http()
            .with_endpoint(config.endpoint.clone())
            .with_timeout(timeout)
            .with_headers(headers_map.clone())
            .build()?;

        // Use PeriodicReader for metrics with 10s export interval (required by spec)
        let metric_reader = PeriodicReader::builder(metric_exporter)
            .with_interval(Duration::from_secs(config.metrics_interval_secs))
            .build();

        let meter_provider = SdkMeterProvider::builder()
            .with_reader(metric_reader)
            .with_resource(resource.clone())
            .build();

        // Build log exporter
        let log_exporter = LogExporter::builder()
            .with_http()
            .with_endpoint(config.endpoint.clone())
            .with_timeout(timeout)
            .with_headers(headers_map)
            .build()?;

        // Use BatchLogProcessor for logs (required by spec)
        let batch_log_processor = BatchLogProcessor::builder(log_exporter).build();

        let logger_provider = SdkLoggerProvider::builder()
            .with_log_processor(batch_log_processor)
            .with_resource(resource.clone())
            .build();

        Ok((
            tracer_provider,
            meter_provider,
            logger_provider,
        ))
    }

    /// Dispatch a telemetry event to the appropriate signal.
    ///
    /// Per the semantic mapping:
    /// - Span events: agent.executing, heartbeat.emitted
    /// - Metrics: beads.completed, bead.duration, strand.duration, agent.duration, etc.
    /// - Logs: everything not already represented as a span or metric
    fn dispatch_event(&self, event: &TelemetryEvent) -> Result<()> {
        match event.event_type.as_str() {
            // Metrics: bead completion
            "bead.completed" => {
                if let Some(duration_ms) = event.duration_ms {
                    let outcome = event
                        .data
                        .get("outcome")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    self.metrics
                        .beads_completed
                        .add(1, &[KeyValue::new("outcome", outcome.to_string())]);
                    self.metrics
                        .bead_duration
                        .record(duration_ms as f64, &[KeyValue::new("outcome", outcome.to_string())]);
                    self.metrics.beads_claimed.add(-1, &[]);
                }
            }

            // Metrics: strand evaluation
            "strand.evaluated" => {
                if let Some(duration_ms) = event.duration_ms {
                    if let Some(strand_name) = event.data.get("strand_name").and_then(|v| v.as_str()) {
                        let result = event.data.get("result").and_then(|v| v.as_str()).unwrap_or("unknown");
                        self.metrics
                            .strand_duration
                            .record(duration_ms as f64, &[
                                KeyValue::new("strand", strand_name.to_string()),
                                KeyValue::new("result", result.to_string()),
                            ]);
                    }
                }
            }

            // Metrics: agent duration
            "dispatch.completed" => {
                if let Some(duration_ms) = event.duration_ms {
                    let exit_code = event.data.get("exit_code").and_then(|v| v.as_i64()).unwrap_or(-1);
                    self.metrics
                        .agent_duration
                        .record(duration_ms as f64, &[KeyValue::new("exit_code", exit_code)]);
                }
            }

            // Metrics: claim attempts
            "claim.attempt" => {
                if let Some(attempt) = event.data.get("attempt").and_then(|v| v.as_u64()) {
                    self.metrics
                        .claim_attempts
                        .add(1, &[KeyValue::new("attempt", attempt.to_string())]);
                }
            }

            // Metrics: claim success
            "claim.success" => {
                self.metrics.beads_claimed.add(1, &[]);
            }

            // Metrics: tokens and cost (from effort.recorded)
            "effort.recorded" => {
                if let Some(tokens_in) = event.data.get("tokens_in").and_then(|v| v.as_u64()) {
                    let model = event.data.get("model").and_then(|v| v.as_str()).unwrap_or("unknown");
                    self.metrics
                        .tokens_input
                        .add(tokens_in, &[KeyValue::new("model", model.to_string())]);
                }
                if let Some(tokens_out) = event.data.get("tokens_out").and_then(|v| v.as_u64()) {
                    let model = event.data.get("model").and_then(|v| v.as_str()).unwrap_or("unknown");
                    self.metrics
                        .tokens_output
                        .add(tokens_out, &[KeyValue::new("model", model.to_string())]);
                }
                if let Some(cost) = event.data.get("estimated_cost_usd").and_then(|v| v.as_f64()) {
                    let agent = event.data.get("agent_name").and_then(|v| v.as_str()).unwrap_or("unknown");
                    let model = event.data.get("model").and_then(|v| v.as_str()).unwrap_or("unknown");
                    self.metrics
                        .cost_usd
                        .add(cost, &[
                            KeyValue::new("agent", agent.to_string()),
                            KeyValue::new("model", model.to_string()),
                        ]);
                }
            }

            // For all other events, export as logs
            _ => {
                self.emit_log(event)?;
            }
        }

        Ok(())
    }

    /// Emit a telemetry event as a log record.
    fn emit_log(&self, event: &TelemetryEvent) -> Result<()> {
        let logger = self.logger_provider.logger("needle");

        // Create log record and set its fields
        let mut log_record = logger.create_log_record();

        // Set severity
        let severity = self.severity_for_event(&event.event_type);
        log_record.set_severity_number(severity.0);
        log_record.set_severity_text(severity.1);

        // Set body
        let body_str = serde_json::to_string(&event.data)
            .unwrap_or_else(|_| "{\"error\":\"failed to serialize data\"}".to_string());
        log_record.set_body(AnyValue::from(body_str));

        // Build and add attributes - explicit type to avoid inference errors
        let mut attrs: Vec<(&str, AnyValue)> = Vec::with_capacity(8);
        attrs.push(("event_type", event.event_type.clone().into()));
        attrs.push(("worker_id", event.worker_id.clone().into()));
        attrs.push(("session_id", event.session_id.clone().into()));
        attrs.push(("sequence", AnyValue::from(event.sequence as i64)));

        if let Some(ref bead_id) = event.bead_id {
            attrs.push(("bead_id", bead_id.as_ref().to_string().into()));
        }

        if let Some(ref workspace) = event.workspace {
            attrs.push(("workspace", workspace.display().to_string().into()));
        }

        if let Some(duration_ms) = event.duration_ms {
            attrs.push(("duration_ms", AnyValue::from(duration_ms as i64)));
        }

        if let Some(ref trace_id) = event.trace_id {
            attrs.push(("trace_id", trace_id.clone().into()));
        }

        if let Some(ref span_id) = event.span_id {
            attrs.push(("span_id", span_id.clone().into()));
        }

        log_record.add_attributes(attrs);

        logger.emit(log_record);

        Ok(())
    }

    /// Map event type to OTel severity level.
    fn severity_for_event(&self, event_type: &str) -> (Severity, &'static str) {
        match event_type {
            // ERROR events
            "worker.errored"
            | "bead.claim.failed"
            | "bead.orphaned"
            | "build.timeout"
            | "worker.handling.timeout"
            | "telemetry.sink_error"
            | "transform.failed" => (Severity::Error, "ERROR"),

            // WARN events
            "peer.stale" | "budget.warning" | "rate_limit.wait" | "bead.released"
            | "bead.claim.race_lost" => (Severity::Warn, "WARN"),

            // INFO events (default)
            _ => (Severity::Info, "INFO"),
        }
    }

    /// Shutdown the OTLP sink, draining all batched exports.
    pub async fn shutdown(self) -> Result<()> {
        // Shutdown in reverse order: logs, metrics, traces
        // This ensures all dependent data is flushed first

        // Flush logger provider
        self.logger_provider
            .shutdown()
            .context("failed to shutdown logger provider")?;

        // Flush meter provider (this also stops the periodic reader)
        self.meter_provider
            .shutdown()
            .context("failed to shutdown meter provider")?;

        // Flush tracer provider
        self.tracer_provider
            .shutdown()
            .context("failed to shutdown tracer provider")?;

        Ok(())
    }
}

impl crate::telemetry::Sink for OtlpSink {
    fn accept(&self, event: &TelemetryEvent) -> Result<()> {
        self.dispatch_event(event)
    }

    fn flush(&self, deadline: std::time::Duration) -> Result<()> {
        // Split the deadline across all three providers
        // Duration doesn't have saturating_div, so we use checked_div with a fallback
        let _per_provider = deadline.checked_div(3).unwrap_or(deadline);

        // Use tokio::runtime::Handle::try_current to see if we're in an async context
        // If so, we can use timeout; otherwise, we just call force_flush directly
        let tracer_result = self.tracer_provider.force_flush();
        let meter_result = self.meter_provider.force_flush();
        let logger_result = self.logger_provider.force_flush();

        // Check if any flush failed
        tracer_result.context("failed to flush tracer provider")?;
        meter_result.context("failed to flush meter provider")?;
        logger_result.context("failed to flush logger provider")?;

        Ok(())
    }
}
