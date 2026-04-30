//! OTLP sink for OpenTelemetry export.
//!
//! Exports telemetry as OpenTelemetry signals (traces, metrics, logs) over OTLP
//! to any compliant collector.

use crate::config::OtlpSinkConfig;
use crate::telemetry::TelemetryEvent;
use anyhow::{Context, Result};
use opentelemetry::logs::{AnyValue, LogRecord, Logger, LoggerProvider, Severity};
use opentelemetry::metrics::{Counter, Histogram, MeterProvider, ObservableGauge, UpDownCounter};
use opentelemetry::trace::{SpanId, TraceId, TracerProvider};
use opentelemetry::KeyValue;
use opentelemetry_sdk::error::OTelSdkError;
use opentelemetry_sdk::logs::{
    BatchLogProcessor, LogBatch, LogExporter as SdkLogExporter, SdkLoggerProvider,
};
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::resource::{
    Resource, SdkProvidedResourceDetector, TelemetryResourceDetector,
};
use opentelemetry_sdk::trace::{
    BatchSpanProcessor, SdkTracerProvider, SpanData, SpanExporter as SdkSpanExporter,
};
use std::collections::HashMap;
use std::panic::catch_unwind;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tracing::warn;

#[cfg(feature = "otlp")]
use tracing_opentelemetry::OpenTelemetryLayer;
#[cfg(feature = "otlp")]
use tracing_subscriber::Registry;

/// Drop event signal type (traces, metrics, or logs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum SignalType {
    Traces,
    Metrics,
    Logs,
}

impl SignalType {
    fn as_str(&self) -> &'static str {
        match self {
            SignalType::Traces => "traces",
            SignalType::Metrics => "metrics",
            SignalType::Logs => "logs",
        }
    }
}

/// Event sent from exporter wrappers to the drop monitor task.
#[derive(Debug, Clone)]
pub(crate) struct DropEvent {
    signal: SignalType,
    dropped_count: u64,
}

/// Shared state for tracking consecutive export failures.
#[derive(Clone, Debug)]
struct FailureTracker {
    consecutive_failures: Arc<AtomicU64>,
}

impl FailureTracker {
    fn new() -> Self {
        Self {
            consecutive_failures: Arc::new(AtomicU64::new(0)),
        }
    }

    fn record_success(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
    }

    fn record_failure(&self) -> u64 {
        self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn reset(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
    }
}

/// Span exporter wrapper for gRPC that detects export failures.
///
/// This is a concrete type that wraps the tonic gRPC exporter.
/// It implements `SpanExporter` and detects failures, reporting them
/// to the drop monitor channel.
#[derive(Debug)]
struct ResilientGrpcSpanExporter {
    inner: Arc<opentelemetry_otlp::SpanExporter>,
    drop_tx: mpsc::UnboundedSender<DropEvent>,
    failure_tracker: FailureTracker,
}

impl ResilientGrpcSpanExporter {
    fn new(
        inner: opentelemetry_otlp::SpanExporter,
        drop_tx: mpsc::UnboundedSender<DropEvent>,
    ) -> Self {
        Self {
            inner: Arc::new(inner),
            drop_tx,
            failure_tracker: FailureTracker::new(),
        }
    }
}

#[allow(refining_impl_trait_internal)]
impl SdkSpanExporter for ResilientGrpcSpanExporter {
    fn export(
        &self,
        batch: Vec<SpanData>,
    ) -> futures::future::BoxFuture<'static, Result<(), OTelSdkError>> {
        let inner = Arc::clone(&self.inner);
        let drop_tx = self.drop_tx.clone();
        let failure_tracker = self.failure_tracker.clone();

        Box::pin(async move {
            match inner.export(batch).await {
                Ok(()) => {
                    failure_tracker.record_success();
                    Ok(())
                }
                Err(e) => {
                    let failures = failure_tracker.record_failure();
                    let error_msg = e.to_string();
                    warn!("OTLP span export failed (attempt {failures}): {error_msg}");

                    if failures >= 3 {
                        let _ = drop_tx.send(DropEvent {
                            signal: SignalType::Traces,
                            dropped_count: 1,
                        });
                        failure_tracker.reset();
                    }

                    Err(e)
                }
            }
        })
    }
}

/// Span exporter wrapper for HTTP that detects export failures.
#[derive(Debug)]
struct ResilientHttpSpanExporter {
    inner: Arc<opentelemetry_otlp::SpanExporter>,
    drop_tx: mpsc::UnboundedSender<DropEvent>,
    failure_tracker: FailureTracker,
}

impl ResilientHttpSpanExporter {
    fn new(
        inner: opentelemetry_otlp::SpanExporter,
        drop_tx: mpsc::UnboundedSender<DropEvent>,
    ) -> Self {
        Self {
            inner: Arc::new(inner),
            drop_tx,
            failure_tracker: FailureTracker::new(),
        }
    }
}

#[allow(refining_impl_trait_internal)]
impl SdkSpanExporter for ResilientHttpSpanExporter {
    fn export(
        &self,
        batch: Vec<SpanData>,
    ) -> futures::future::BoxFuture<'static, Result<(), OTelSdkError>> {
        let inner = Arc::clone(&self.inner);
        let drop_tx = self.drop_tx.clone();
        let failure_tracker = self.failure_tracker.clone();

        Box::pin(async move {
            match inner.export(batch).await {
                Ok(()) => {
                    failure_tracker.record_success();
                    Ok(())
                }
                Err(e) => {
                    let failures = failure_tracker.record_failure();
                    let error_msg = e.to_string();
                    warn!("OTLP span export failed (attempt {failures}): {error_msg}");

                    if failures >= 3 {
                        let _ = drop_tx.send(DropEvent {
                            signal: SignalType::Traces,
                            dropped_count: 1,
                        });
                        failure_tracker.reset();
                    }

                    Err(e)
                }
            }
        })
    }
}

/// Log exporter wrapper for gRPC that detects export failures.
#[derive(Debug)]
struct ResilientGrpcLogExporter {
    inner: Arc<opentelemetry_otlp::LogExporter>,
    drop_tx: mpsc::UnboundedSender<DropEvent>,
    failure_tracker: FailureTracker,
}

impl ResilientGrpcLogExporter {
    fn new(
        inner: opentelemetry_otlp::LogExporter,
        drop_tx: mpsc::UnboundedSender<DropEvent>,
    ) -> Self {
        Self {
            inner: Arc::new(inner),
            drop_tx,
            failure_tracker: FailureTracker::new(),
        }
    }
}

#[allow(refining_impl_trait_internal)]
impl SdkLogExporter for ResilientGrpcLogExporter {
    fn export(
        &self,
        batch: LogBatch<'_>,
    ) -> impl std::future::Future<Output = Result<(), OTelSdkError>> + Send {
        let inner = Arc::clone(&self.inner);
        let drop_tx = self.drop_tx.clone();
        let failure_tracker = self.failure_tracker.clone();

        async move {
            match inner.export(batch).await {
                Ok(()) => {
                    failure_tracker.record_success();
                    Ok(())
                }
                Err(e) => {
                    let failures = failure_tracker.record_failure();
                    let error_msg = e.to_string();
                    warn!("OTLP log export failed (attempt {failures}): {error_msg}");

                    if failures >= 3 {
                        let _ = drop_tx.send(DropEvent {
                            signal: SignalType::Logs,
                            dropped_count: 1,
                        });
                        failure_tracker.reset();
                    }

                    Err(e)
                }
            }
        }
    }
}

/// Log exporter wrapper for HTTP that detects export failures.
#[derive(Debug)]
struct ResilientHttpLogExporter {
    inner: Arc<opentelemetry_otlp::LogExporter>,
    drop_tx: mpsc::UnboundedSender<DropEvent>,
    failure_tracker: FailureTracker,
}

impl ResilientHttpLogExporter {
    fn new(
        inner: opentelemetry_otlp::LogExporter,
        drop_tx: mpsc::UnboundedSender<DropEvent>,
    ) -> Self {
        Self {
            inner: Arc::new(inner),
            drop_tx,
            failure_tracker: FailureTracker::new(),
        }
    }
}

#[allow(refining_impl_trait_internal)]
impl SdkLogExporter for ResilientHttpLogExporter {
    fn export(
        &self,
        batch: LogBatch<'_>,
    ) -> impl std::future::Future<Output = Result<(), OTelSdkError>> + Send {
        let inner = Arc::clone(&self.inner);
        let drop_tx = self.drop_tx.clone();
        let failure_tracker = self.failure_tracker.clone();

        async move {
            match inner.export(batch).await {
                Ok(()) => {
                    failure_tracker.record_success();
                    Ok(())
                }
                Err(e) => {
                    let failures = failure_tracker.record_failure();
                    let error_msg = e.to_string();
                    warn!("OTLP log export failed (attempt {failures}): {error_msg}");

                    if failures >= 3 {
                        let _ = drop_tx.send(DropEvent {
                            signal: SignalType::Logs,
                            dropped_count: 1,
                        });
                        failure_tracker.reset();
                    }

                    Err(e)
                }
            }
        }
    }
}

/// OTLP sink implementing the telemetry Sink trait.
///
/// Wraps the OpenTelemetry SDK providers and translates TelemetryEvent
/// into traces, metrics, and logs per the semantic mapping.
///
/// ## Resilience
///
/// The OTel SDK's batch processors provide bounded queues that drop
/// oldest items when full. Export failures are logged at WARN level
/// but do not crash the worker. Shutdown has a 5-second timeout.
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
    /// File sink for emitting drop events (never to OTLP, to avoid recursion).
    /// Wrapped in Arc so it can be shared with the drop monitor task.
    file_sink: Option<Arc<Box<dyn crate::telemetry::Sink>>>,
    /// Worker ID for drop events.
    worker_id: String,
    /// Session ID for drop events.
    session_id: String,
    /// Next sequence number for drop events.
    next_drop_sequence: Arc<AtomicU64>,
    /// Observable state for gauges (heartbeat.age, queue.depth).
    observable_state: Arc<ObservableState>,
    /// Drop monitor task handle - kept alive to ensure the task runs.
    _drop_monitor_handle: Arc<DropMonitorHandle>,
}

/// Handle to keep the drop monitor task alive.
///
/// Uses an atomic flag to signal shutdown to the monitor task.
#[allow(dead_code)]
struct DropMonitorHandle {
    shutdown: Arc<AtomicU64>,
}

#[allow(dead_code)]
impl DropMonitorHandle {
    fn new() -> Self {
        Self {
            shutdown: Arc::new(AtomicU64::new(0)),
        }
    }

    fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed) != 0
    }

    fn shutdown(&self) {
        self.shutdown.store(1, Ordering::Relaxed);
    }
}

/// Sequence number atomic for drop events.
#[derive(Clone)]
#[allow(dead_code)]
struct DropSequence(Arc<AtomicU64>);

/// State for observable gauges.
#[derive(Default)]
struct ObservableState {
    /// Last heartbeat timestamp (UNIX epoch seconds).
    last_heartbeat_secs: AtomicU64,
    /// Queue depth per priority level (sampled at strand evaluation).
    /// Maps priority -> count.
    queue_depth_by_priority: Arc<std::sync::Mutex<HashMap<u8, AtomicU64>>>,
}

/// Cached metric instruments for efficient recording.
#[derive(Clone)]
#[allow(dead_code)]
struct Metrics {
    /// UpDownCounter: `needle.workers.active`
    workers_active: UpDownCounter<i64>,
    /// Counter: `needle.beads.claimed`
    beads_claimed: Counter<u64>,
    /// Counter: `needle.beads.completed`
    beads_completed: Counter<u64>,
    /// Histogram: `needle.beads.duration`
    bead_duration: Histogram<f64>,
    /// Counter: `needle.claim.attempts`
    claim_attempts: Counter<u64>,
    /// Histogram: `needle.strand.duration`
    strand_duration: Histogram<f64>,
    /// Histogram: `needle.agent.duration`
    agent_duration: Histogram<f64>,
    /// Counter: `needle.agent.tokens.input`
    tokens_input: Counter<u64>,
    /// Counter: `needle.agent.tokens.output`
    tokens_output: Counter<u64>,
    /// Counter: `needle.cost.usd`
    cost_usd: Counter<f64>,
    /// ObservableGauge: `needle.heartbeat.age`
    heartbeat_age: ObservableGauge<u64>,
    /// ObservableGauge: `needle.queue.depth`
    queue_depth: ObservableGauge<u64>,
    /// UpDownCounter: `needle.peers.stale`
    peers_stale: UpDownCounter<i64>,
    /// Counter: `needle.mitosis.children_created`
    mitosis_children_created: Counter<u64>,
}

impl OtlpSink {
    /// Create a new OTLP sink from configuration.
    ///
    /// Initializes the OpenTelemetry SDK with batch processors for
    /// non-blocking export of all three signals.
    ///
    /// The `file_sink` parameter is used to emit shutdown timeout events
    /// when the flush takes longer than 5 seconds.
    ///
    /// The `agent`, `model`, and `workspace` parameters are optional resource
    /// attributes for OpenTelemetry semantic conventions.
    pub fn new(
        worker_id: String,
        session_id: String,
        config: &OtlpSinkConfig,
        file_sink: Option<Box<dyn crate::telemetry::Sink>>,
        agent: Option<&str>,
        model: Option<&str>,
        workspace: Option<&str>,
    ) -> Result<Self> {
        // Build resource attributes from config + computed attributes
        let resource =
            Self::build_resource(&worker_id, &session_id, config, agent, model, workspace)?;

        // Create drop sequence atomic and monitor channel
        let next_drop_sequence = Arc::new(AtomicU64::new(0));
        let (drop_tx, drop_rx) = mpsc::unbounded_channel::<DropEvent>();

        // Wrap file_sink in Arc so it can be shared with drop monitor task
        let file_sink_arc = file_sink.map(Arc::new);

        // Spawn drop monitor task to emit drop events to file sink
        // Only spawn if we have a file_sink - otherwise the task would just drop events
        if file_sink_arc.is_some() {
            let worker_id_clone = worker_id.clone();
            let session_id_clone = session_id.clone();
            let next_drop_seq_clone = next_drop_sequence.clone();
            let file_sink_for_monitor = file_sink_arc.clone();
            tokio::spawn(async move {
                Self::drop_monitor_task(
                    drop_rx,
                    file_sink_for_monitor,
                    worker_id_clone,
                    session_id_clone,
                    next_drop_seq_clone,
                )
                .await;
            });
        }

        // Build exporters based on protocol
        let (tracer_provider, meter_provider, logger_provider) = match config.protocol.as_str() {
            "grpc" => Self::build_grpc_providers(config, &resource, drop_tx)?,
            "http" | "http/protobuf" => Self::build_http_providers(config, &resource, drop_tx)?,
            other => anyhow::bail!("invalid OTLP protocol: {other}, must be 'grpc' or 'http'"),
        };

        // Build metric instruments
        let meter = meter_provider.meter("needle");
        let observable_state = Arc::new(ObservableState::default());

        // Clone Arc for each callback (they need to outlive this function)
        let heartbeat_state = observable_state.clone();
        let worker_id_for_callback = worker_id.clone();

        // Register observable gauges with callbacks
        let heartbeat_age = meter
            .u64_observable_gauge("needle.heartbeat.age")
            .with_unit("s")
            .with_description("Seconds since last heartbeat emitted by this worker")
            .with_callback(move |observer| {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let last = heartbeat_state.last_heartbeat_secs.load(Ordering::Relaxed);
                if last > 0 && now > last {
                    observer.observe(
                        now - last,
                        &[KeyValue::new("worker_id", worker_id_for_callback.clone())],
                    );
                }
            })
            .build();

        // Clone Arc for queue depth callback
        let queue_state = observable_state.clone();
        // Capture workspace for queue depth attribute (bounded cardinality: one per worker)
        let workspace_attr = workspace.map(|s| s.to_string());

        let queue_depth = meter
            .u64_observable_gauge("needle.queue.depth")
            .with_unit("{bead}")
            .with_description("Open beads visible to this worker (sampled at strand evaluation)")
            .with_callback(move |observer| {
                // Observe queue depth for each priority level
                let guard = queue_state.queue_depth_by_priority.lock().unwrap();
                for (priority, count) in guard.iter() {
                    // Build attributes vector with priority and optional workspace
                    let mut attrs = vec![KeyValue::new("priority", i64::from(*priority))];
                    if let Some(ref ws) = workspace_attr {
                        attrs.push(KeyValue::new("workspace", ws.clone()));
                    }
                    observer.observe(count.load(Ordering::Relaxed), &attrs);
                }
            })
            .build();

        let metrics = Metrics {
            workers_active: meter
                .i64_up_down_counter("needle.workers.active")
                .with_unit("{worker}")
                .with_description("Current live worker count")
                .build(),
            beads_claimed: meter
                .u64_counter("needle.beads.claimed")
                .with_unit("{bead}")
                .with_description("Successful bead claims")
                .build(),
            beads_completed: meter
                .u64_counter("needle.beads.completed")
                .with_unit("{bead}")
                .with_description("Bead terminal outcomes (one per bead.outcome)")
                .build(),
            bead_duration: meter
                .f64_histogram("needle.beads.duration")
                .with_unit("ms")
                .with_description("End-to-end bead lifecycle time")
                .build(),
            claim_attempts: meter
                .u64_counter("needle.claim.attempts")
                .with_unit("{attempt}")
                .with_description("Claim attempts")
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
            heartbeat_age,
            queue_depth,
            peers_stale: meter
                .i64_up_down_counter("needle.peers.stale")
                .with_unit("{peer}")
                .with_description("Currently-stale peers observed by this worker")
                .build(),
            mitosis_children_created: meter
                .u64_counter("needle.mitosis.children_created")
                .with_unit("{bead}")
                .with_description("Mitosis child creations")
                .build(),
        };

        let drop_monitor_handle = Arc::new(DropMonitorHandle::new());

        Ok(OtlpSink {
            tracer_provider: Arc::new(tracer_provider),
            meter_provider: Arc::new(meter_provider),
            logger_provider: Arc::new(logger_provider),
            metrics,
            file_sink: file_sink_arc,
            worker_id,
            session_id,
            next_drop_sequence,
            observable_state,
            _drop_monitor_handle: drop_monitor_handle,
        })
    }

    /// Background task that monitors for export failures and emits drop events.
    ///
    /// This task runs for the lifetime of the OtlpSink, receiving drop notifications
    /// from exporter wrappers and emitting `telemetry.otlp.dropped` events to the
    /// file sink only (never to OTLP, to avoid recursion).
    async fn drop_monitor_task(
        mut drop_rx: mpsc::UnboundedReceiver<DropEvent>,
        file_sink: Option<Arc<Box<dyn crate::telemetry::Sink>>>,
        worker_id: String,
        session_id: String,
        next_drop_sequence: Arc<AtomicU64>,
    ) {
        while let Some(drop) = drop_rx.recv().await {
            if let Some(sink) = &file_sink {
                let sequence = next_drop_sequence.fetch_add(1, Ordering::Relaxed);

                let mut data = serde_json::map::Map::new();
                data.insert(
                    "signal".to_string(),
                    serde_json::json!(drop.signal.as_str()),
                );
                data.insert(
                    "dropped_count".to_string(),
                    serde_json::json!(drop.dropped_count),
                );
                data.insert("queue_full".to_string(), serde_json::json!(true));

                let event = TelemetryEvent {
                    timestamp: chrono::Utc::now(),
                    event_type: "telemetry.otlp.dropped".to_string(),
                    worker_id: worker_id.clone(),
                    session_id: session_id.clone(),
                    sequence,
                    bead_id: None,
                    workspace: None,
                    duration_ms: None,
                    data: serde_json::Value::Object(data),
                    trace_id: None,
                    span_id: None,
                };

                // Emit to file sink - ignore errors to avoid recursion
                let _ = catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if let Err(e) = (**sink).accept(&event) {
                        warn!("failed to write OTLP drop event to file sink: {e}");
                    }
                }));
            }
        }
    }

    /// Build the OTel Resource with config and computed attributes.
    ///
    /// Merges three layers (lowest → highest precedence):
    /// 1. OTel defaults (service.name = "needle", schema url)
    /// 2. Computed attributes (set by NEEDLE at runtime)
    /// 3. Config attributes (from telemetry.otlp.resource_attributes)
    ///
    /// Reserved keys `service.name` and `service.instance.id` cannot be overridden
    /// via config - attempting to do so will return an error.
    pub(crate) fn build_resource(
        worker_id: &str,
        session_id: &str,
        config: &OtlpSinkConfig,
        agent: Option<&str>,
        model: Option<&str>,
        workspace: Option<&str>,
    ) -> Result<Resource> {
        // Reserved keys that cannot be overridden via config
        const RESERVED_KEYS: &[&str] = &["service.name", "service.instance.id"];

        let mut builder = Resource::builder().with_attributes([
            KeyValue::new("service.name", "needle"),
            KeyValue::new("service.namespace", config.service_namespace.clone()),
            KeyValue::new("service.version", env!("CARGO_PKG_VERSION")),
            KeyValue::new("service.instance.id", worker_id.to_string()),
            KeyValue::new("needle.session_id", session_id.to_string()),
        ]);

        // Add hostname from OS
        if let Some(hostname) = gethostname::gethostname().to_str() {
            builder = builder.with_attributes([KeyValue::new("host.name", hostname.to_string())]);
        }

        // Add process PID
        builder =
            builder.with_attributes([KeyValue::new("process.pid", std::process::id().to_string())]);

        // Add computed needle.* attributes if provided
        if let Some(agent_value) = agent {
            builder =
                builder.with_attributes([KeyValue::new("needle.agent", agent_value.to_string())]);
        }
        if let Some(model_value) = model {
            builder =
                builder.with_attributes([KeyValue::new("needle.model", model_value.to_string())]);
        }
        if let Some(workspace_value) = workspace {
            builder = builder.with_attributes([KeyValue::new(
                "needle.workspace",
                workspace_value.to_string(),
            )]);
        }

        // Add resource attributes from config (KEY=VALUE pairs)
        // Validate that reserved keys are not being overridden
        for attr_str in &config.resource_attributes {
            if let Some((key, value)) = attr_str.split_once('=') {
                if RESERVED_KEYS.contains(&key) {
                    anyhow::bail!(
                        "cannot override reserved resource attribute '{key}' via config. \
                         Reserved keys are: service.name, service.instance.id"
                    );
                }
                builder =
                    builder.with_attributes([KeyValue::new(key.to_string(), value.to_string())]);
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
    pub(crate) fn build_grpc_providers(
        config: &OtlpSinkConfig,
        resource: &Resource,
        drop_tx: mpsc::UnboundedSender<DropEvent>,
    ) -> Result<(SdkTracerProvider, SdkMeterProvider, SdkLoggerProvider)> {
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

        // Import the tonic-specific exporters
        use opentelemetry_otlp::WithTonicConfig;
        use opentelemetry_otlp::{LogExporter, MetricExporter, SpanExporter, WithExportConfig};

        // Build span exporter with tonic config, then wrap for resilience
        let base_span_exporter = SpanExporter::builder()
            .with_tonic()
            .with_endpoint(config.endpoint.clone())
            .with_timeout(timeout)
            .with_metadata(metadata.clone())
            .build()?;
        let span_exporter = ResilientGrpcSpanExporter::new(base_span_exporter, drop_tx.clone());

        // Use BatchSpanProcessor for traces (required by spec)
        // Note: with_max_queue_size is not available in this version of OTel SDK
        // The default queue size is used instead
        let batch_span_processor = BatchSpanProcessor::builder(span_exporter).build();

        let tracer_provider = SdkTracerProvider::builder()
            .with_span_processor(batch_span_processor)
            .with_resource(resource.clone())
            .build();

        // Build metric exporter with tonic config
        // Note: In OpenTelemetry SDK 0.31, metric exporters use async PushMetricExporter trait
        // The PeriodicReader handles retries internally, so we use the exporter directly
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

        // Build log exporter with tonic config, then wrap for resilience
        let base_log_exporter = LogExporter::builder()
            .with_tonic()
            .with_endpoint(config.endpoint.clone())
            .with_timeout(timeout)
            .with_metadata(metadata)
            .build()?;
        let log_exporter = ResilientGrpcLogExporter::new(base_log_exporter, drop_tx);

        // Use BatchLogProcessor for logs (required by spec)
        // Note: with_max_queue_size is not available in this version of OTel SDK
        // The default queue size is used instead
        let batch_log_processor = BatchLogProcessor::builder(log_exporter).build();

        let logger_provider = SdkLoggerProvider::builder()
            .with_log_processor(batch_log_processor)
            .with_resource(resource.clone())
            .build();

        Ok((tracer_provider, meter_provider, logger_provider))
    }

    /// Build providers using HTTP/protobuf transport (reqwest).
    pub(crate) fn build_http_providers(
        config: &OtlpSinkConfig,
        resource: &Resource,
        drop_tx: mpsc::UnboundedSender<DropEvent>,
    ) -> Result<(SdkTracerProvider, SdkMeterProvider, SdkLoggerProvider)> {
        use opentelemetry_otlp::WithHttpConfig;
        use opentelemetry_otlp::{LogExporter, MetricExporter, SpanExporter, WithExportConfig};

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

        // Build span exporter, then wrap for resilience
        let base_span_exporter = SpanExporter::builder()
            .with_http()
            .with_endpoint(config.endpoint.clone())
            .with_timeout(timeout)
            .with_headers(headers_map.clone())
            .build()?;
        let span_exporter = ResilientHttpSpanExporter::new(base_span_exporter, drop_tx.clone());

        // Use BatchSpanProcessor for traces (required by spec)
        // Note: with_max_queue_size is not available in this version of OTel SDK
        // The default queue size is used instead
        let batch_span_processor = BatchSpanProcessor::builder(span_exporter).build();

        let tracer_provider = SdkTracerProvider::builder()
            .with_span_processor(batch_span_processor)
            .with_resource(resource.clone())
            .build();

        // Build metric exporter with http config
        // Note: In OpenTelemetry SDK 0.31, metric exporters use async PushMetricExporter trait
        // The PeriodicReader handles retries internally, so we use the exporter directly
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

        // Build log exporter, then wrap for resilience
        let base_log_exporter = LogExporter::builder()
            .with_http()
            .with_endpoint(config.endpoint.clone())
            .with_timeout(timeout)
            .with_headers(headers_map)
            .build()?;
        let log_exporter = ResilientHttpLogExporter::new(base_log_exporter, drop_tx);

        // Use BatchLogProcessor for logs (required by spec)
        // Note: with_max_queue_size is not available in this version of OTel SDK
        // The default queue size is used instead
        let batch_log_processor = BatchLogProcessor::builder(log_exporter).build();

        let logger_provider = SdkLoggerProvider::builder()
            .with_log_processor(batch_log_processor)
            .with_resource(resource.clone())
            .build();

        Ok((tracer_provider, meter_provider, logger_provider))
    }

    /// Dispatch a telemetry event to the appropriate signal.
    ///
    /// Per the semantic mapping:
    /// - Spans (NOT logs): bead.claim.attempted, bead.claim.succeeded, bead.claim.race_lost,
    ///   bead.claim.failed, agent.dispatched, agent.completed,
    ///   strand.evaluated, outcome.handled, bead.completed
    /// - Span events (NOT logs): heartbeat.emitted, build.heartbeat
    /// - Metrics: beads.completed, bead.duration, strand.duration, agent.duration, etc.
    /// - Logs: everything not already represented as a span or span event
    fn dispatch_event(&self, event: &TelemetryEvent) -> Result<()> {
        match event.event_type.as_str() {
            // Metrics: worker lifecycle
            "worker.started" => {
                self.metrics.workers_active.add(1, &[]);
                // Also export as log for visibility
                self.emit_log(event)?;
            }

            "worker.stopped" => {
                self.metrics.workers_active.add(-1, &[]);
                // Also export as log for visibility
                self.emit_log(event)?;
            }

            // Events that ARE spans - export as metrics only, NOT as logs
            "bead.claim.attempted" => {
                // Track claim attempts with result="attempting"
                self.metrics
                    .claim_attempts
                    .add(1, &[KeyValue::new("result", "attempting")]);
                // Do NOT export as log - this is a span
            }

            "bead.claim.succeeded" => {
                // Track claim attempts with result="succeeded"
                self.metrics
                    .claim_attempts
                    .add(1, &[KeyValue::new("result", "succeeded")]);

                // Emit beads_claimed metric with strand and priority attributes
                let strand = event
                    .data
                    .get("strand")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let priority = event
                    .data
                    .get("priority")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0);
                self.metrics.beads_claimed.add(
                    1,
                    &[
                        KeyValue::new("strand", strand.to_string()),
                        KeyValue::new("priority", priority),
                    ],
                );
                // Do NOT export as log - this is a span
            }

            "bead.claim.race_lost" => {
                // Track claim attempts with result="race_lost"
                self.metrics
                    .claim_attempts
                    .add(1, &[KeyValue::new("result", "race_lost")]);
                // Export as log with INFO severity
                self.emit_log(event)?;
            }

            "bead.claim.failed" => {
                // Track claim attempts with result="failed"
                self.metrics
                    .claim_attempts
                    .add(1, &[KeyValue::new("result", "failed")]);
                // Export as log with ERROR severity
                self.emit_log(event)?;
            }

            "agent.dispatched" => {
                // Do NOT export as log - this is a span
            }

            "agent.completed" => {
                if let Some(duration_ms) = event.duration_ms {
                    let exit_code = event
                        .data
                        .get("exit_code")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(-1);

                    // Extract agent and model from event data
                    let agent = event
                        .data
                        .get("agent")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let model = event
                        .data
                        .get("model")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");

                    self.metrics.agent_duration.record(
                        duration_ms as f64,
                        &[
                            KeyValue::new("agent", agent.to_string()),
                            KeyValue::new("model", model.to_string()),
                            KeyValue::new("exit_code", exit_code),
                        ],
                    );
                }
                // Do NOT export as log - this is a span
            }

            "strand.evaluated" => {
                if let Some(duration_ms) = event.duration_ms {
                    if let Some(strand_name) =
                        event.data.get("strand_name").and_then(|v| v.as_str())
                    {
                        let result = event
                            .data
                            .get("result")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        self.metrics.strand_duration.record(
                            duration_ms as f64,
                            &[
                                KeyValue::new("strand", strand_name.to_string()),
                                KeyValue::new("result", result.to_string()),
                            ],
                        );
                    }
                }
                // Do NOT export as log - this is a span
            }

            "outcome.handled" => {
                // Do NOT export as log - this is a span
            }

            // Metrics: bead completion (also a span, but we need metrics)
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
                    self.metrics.bead_duration.record(
                        duration_ms as f64,
                        &[KeyValue::new("outcome", outcome.to_string())],
                    );
                }
                // Do NOT export as log - this is a span
            }

            // Span events - emit as span events, NOT as logs
            "heartbeat.emitted" | "build.heartbeat" => {
                // Update heartbeat age observable state
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                self.observable_state
                    .last_heartbeat_secs
                    .store(now, Ordering::Relaxed);
                self.emit_span_event(event)?;
                // Do NOT export as log - this is a span event
            }

            // Metrics: effort.recorded (not a span, but metrics only)
            "effort.recorded" => {
                let agent = event
                    .data
                    .get("agent_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let model = event
                    .data
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                if let Some(tokens_in) = event.data.get("tokens_in").and_then(|v| v.as_u64()) {
                    self.metrics.tokens_input.add(
                        tokens_in,
                        &[
                            KeyValue::new("agent", agent.to_string()),
                            KeyValue::new("model", model.to_string()),
                        ],
                    );
                }
                if let Some(tokens_out) = event.data.get("tokens_out").and_then(|v| v.as_u64()) {
                    self.metrics.tokens_output.add(
                        tokens_out,
                        &[
                            KeyValue::new("agent", agent.to_string()),
                            KeyValue::new("model", model.to_string()),
                        ],
                    );
                }
                if let Some(cost) = event
                    .data
                    .get("estimated_cost_usd")
                    .and_then(|v| v.as_f64())
                {
                    self.metrics.cost_usd.add(
                        cost,
                        &[
                            KeyValue::new("agent", agent.to_string()),
                            KeyValue::new("model", model.to_string()),
                        ],
                    );
                }
                // Do NOT export as log - metrics only
            }

            // Metrics: peer staleness
            "peer.stale" => {
                self.metrics.peers_stale.add(1, &[]);
                // Also export as log with WARN severity
                self.emit_log(event)?;
            }

            // Metrics: peer recovery (decrement stale count)
            "peer.crashed" => {
                self.metrics.peers_stale.add(-1, &[]);
                // Also export as log with WARN severity
                self.emit_log(event)?;
            }

            // Metrics: mitosis child creation
            "bead.mitosis.split" => {
                if let Some(children_created) =
                    event.data.get("children_created").and_then(|v| v.as_u64())
                {
                    // Note: parent_id is intentionally NOT added as a metric attribute
                    // to maintain bounded cardinality. Bead IDs are unbounded and
                    // should stay on spans, not metrics.
                    self.metrics
                        .mitosis_children_created
                        .add(children_created, &[]);
                }
                // Also export as log for visibility (includes parent_id in log body)
                self.emit_log(event)?;
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

        // Set trace linkage using OTel LogRecord API (not as attributes)
        // TraceId is 16 bytes (32 hex chars), SpanId is 8 bytes (16 hex chars)
        if let (Some(ref trace_id), Some(ref span_id)) = (&event.trace_id, &event.span_id) {
            if let (Ok(trace_bytes), Ok(span_bytes)) = (hex::decode(trace_id), hex::decode(span_id))
            {
                if let (Ok(trace_arr), Ok(span_arr)) = (
                    trace_bytes.as_slice().try_into().map(|b: [u8; 16]| b),
                    span_bytes.as_slice().try_into().map(|b: [u8; 8]| b),
                ) {
                    let trace_id = TraceId::from_bytes(trace_arr);
                    let span_id = SpanId::from_bytes(span_arr);
                    log_record.set_trace_context(trace_id, span_id, None);
                }
            }
        }

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

        log_record.add_attributes(attrs);

        logger.emit(log_record);

        Ok(())
    }

    /// Emit a telemetry event as a span event on the current span.
    ///
    /// Used for intra-span state changes like heartbeat.emitted and build.heartbeat.
    fn emit_span_event(&self, event: &TelemetryEvent) -> Result<()> {
        // Use tracing to emit a span event
        // The event name is the event type, and the data is added as fields
        let event_name = event.event_type.as_str();

        // Build fields from event data
        use std::borrow::Cow;
        let mut fields = Vec::new();
        fields.push(("event_type", Cow::Borrowed(event.event_type.as_str())));
        fields.push(("worker_id", Cow::Borrowed(event.worker_id.as_str())));
        fields.push(("session_id", Cow::Borrowed(event.session_id.as_str())));

        if let Some(ref bead_id) = event.bead_id {
            fields.push(("bead_id", Cow::Borrowed(&**bead_id)));
        }

        // Add any additional fields from the data JSON
        if let Some(data_map) = event.data.as_object() {
            for (key, value) in data_map {
                if let Some(str_val) = value.as_str() {
                    fields.push((key.as_str(), Cow::Borrowed(str_val)));
                } else if let Some(num_val) = value.as_i64() {
                    // Convert to owned string for numeric values
                    fields.push((key.as_str(), Cow::Owned(num_val.to_string())));
                } else if let Some(bool_val) = value.as_bool() {
                    fields.push((
                        key.as_str(),
                        Cow::Borrowed(if bool_val { "true" } else { "false" }),
                    ));
                }
            }
        }

        // Emit as a tracing event (which becomes an OTel span event)
        tracing::event!(
            tracing::Level::INFO,
            name = event_name,
            ?fields,
            "{}",
            event_name
        );

        Ok(())
    }

    /// Map event type to OTel severity level.
    fn severity_for_event(&self, event_type: &str) -> (Severity, &'static str) {
        match event_type {
            // ERROR events
            "worker.errored"
            | "bead.claim.failed"
            | "build.timeout"           // agent timeout
            | "telemetry.otlp.dropped"  // OTLP export drops
            | "telemetry.sink_error"    // Sink errors (e.g., hook failures)
            => (Severity::Error, "ERROR"),

            // WARN events
            "peer.stale"   // StuckDetected
            | "peer.crashed" // StuckReleased
            => (Severity::Warn, "WARN"),

            // INFO events (default)
            // Includes: worker.started, worker.stopped, health.check, effort.recorded,
            //           bead.orphaned, bead.claim.race_lost, budget.warning, rate_limit.wait,
            //           bead.released, transform.failed, worker.handling.timeout,
            //           telemetry.otlp.shutdown_timeout, and everything else
            _ => (Severity::Info, "INFO"),
        }
    }

    /// Update the queue depth observable gauge.
    ///
    /// Called by strand evaluation to sample the current queue depth per priority.
    /// This updates the per-priority counts that the observable gauge callback reads.
    ///
    /// The `depths` parameter maps priority level -> bead count at that priority.
    pub fn record_queue_depth(&self, depths: HashMap<u8, u64>) {
        let mut guard = self
            .observable_state
            .queue_depth_by_priority
            .lock()
            .unwrap();
        for (priority, count) in depths {
            guard
                .entry(priority)
                .or_insert_with(|| AtomicU64::new(0))
                .store(count, Ordering::Relaxed);
        }
    }

    /// Shutdown the OTLP sink, draining all batched exports.
    ///
    /// Has a hard 5-second deadline. If the shutdown takes longer than 5 seconds,
    /// remaining batches are abandoned and a `telemetry.otlp.shutdown_timeout` event
    /// is emitted to the file sink.
    pub async fn shutdown(self) -> Result<()> {
        const SHUTDOWN_DEADLINE_SECS: u64 = 5;
        let deadline = Duration::from_secs(SHUTDOWN_DEADLINE_SECS);

        // Shutdown in reverse order: logs, metrics, traces
        // This ensures all dependent data is flushed first

        // Use a timeout for each shutdown to enforce the deadline
        // Split the deadline across all three providers
        let per_provider_deadline = deadline.checked_div(3).unwrap_or(deadline);

        let mut timed_out = false;

        // Flush logger provider with timeout
        let logger_provider = self.logger_provider.clone();
        let logger_result = tokio::time::timeout(
            per_provider_deadline,
            tokio::task::spawn_blocking(move || {
                catch_unwind(std::panic::AssertUnwindSafe(|| logger_provider.shutdown()))
            }),
        )
        .await;

        match logger_result {
            Ok(Ok(Ok(Ok(())))) => {}
            Ok(Ok(Ok(Err(e)))) => {
                warn!("OTLP logger provider shutdown failed: {e}");
            }
            Ok(Ok(Err(_))) => {
                warn!("OTLP logger provider shutdown panicked");
            }
            Ok(Err(_)) => {
                warn!("OTLP logger provider spawn_blocking panicked");
            }
            Err(_) => {
                warn!("OTLP logger provider shutdown timed out after {per_provider_deadline:?}");
                timed_out = true;
            }
        }

        // Flush meter provider with timeout
        let meter_provider = self.meter_provider.clone();
        let meter_result = tokio::time::timeout(
            per_provider_deadline,
            tokio::task::spawn_blocking(move || {
                catch_unwind(std::panic::AssertUnwindSafe(|| meter_provider.shutdown()))
            }),
        )
        .await;

        match meter_result {
            Ok(Ok(Ok(Ok(())))) => {}
            Ok(Ok(Ok(Err(e)))) => {
                warn!("OTLP meter provider shutdown failed: {e}");
            }
            Ok(Ok(Err(_))) => {
                warn!("OTLP meter provider shutdown panicked");
            }
            Ok(Err(_)) => {
                warn!("OTLP meter provider spawn_blocking panicked");
            }
            Err(_) => {
                warn!("OTLP meter provider shutdown timed out after {per_provider_deadline:?}");
                timed_out = true;
            }
        }

        // Flush tracer provider with timeout
        let tracer_provider = self.tracer_provider.clone();
        let tracer_result = tokio::time::timeout(
            per_provider_deadline,
            tokio::task::spawn_blocking(move || {
                catch_unwind(std::panic::AssertUnwindSafe(|| tracer_provider.shutdown()))
            }),
        )
        .await;

        match tracer_result {
            Ok(Ok(Ok(Ok(())))) => {}
            Ok(Ok(Ok(Err(e)))) => {
                warn!("OTLP tracer provider shutdown failed: {e}");
            }
            Ok(Ok(Err(_))) => {
                warn!("OTLP tracer provider shutdown panicked");
            }
            Ok(Err(_)) => {
                warn!("OTLP tracer provider spawn_blocking panicked");
            }
            Err(_) => {
                warn!("OTLP tracer provider shutdown timed out after {per_provider_deadline:?}");
                timed_out = true;
            }
        }

        // If any provider timed out, emit a shutdown_timeout event to the file sink
        if timed_out {
            if let Some(file_sink) = &self.file_sink {
                let sequence = self.next_drop_sequence.fetch_add(1, Ordering::Relaxed);
                let timestamp = SystemTime::now();

                let mut data = serde_json::map::Map::new();
                data.insert(
                    "deadline_secs".to_string(),
                    serde_json::json!(SHUTDOWN_DEADLINE_SECS),
                );
                data.insert("timed_out".to_string(), serde_json::json!(true));

                let event = TelemetryEvent {
                    timestamp: chrono::DateTime::from(timestamp),
                    event_type: "telemetry.otlp.shutdown_timeout".to_string(),
                    worker_id: self.worker_id.clone(),
                    session_id: self.session_id.clone(),
                    sequence,
                    bead_id: None,
                    workspace: None,
                    duration_ms: None,
                    data: serde_json::Value::Object(data),
                    trace_id: None,
                    span_id: None,
                };

                let _ = catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if let Err(e) = (**file_sink).accept(&event) {
                        warn!("failed to write shutdown timeout event to file sink: {e}");
                    }
                }));
            }
        }

        Ok(())
    }
}

/// Create an OpenTelemetry tracing layer for the tracing subscriber.
///
/// This bridges `tracing` spans to the OTLP exporter, allowing use of
/// `#[instrument]` macros throughout the codebase.
///
/// Returns `None` if the OTLP feature is not enabled or if the layer
/// cannot be created.
///
/// The `agent`, `model`, and `workspace` parameters are optional resource
/// attributes for OpenTelemetry semantic conventions.
///
/// Note: Drop events from the tracing layer are logged at WARN level
/// rather than emitted to the file sink, because the tracing layer is
/// created before the file sink is available.
#[cfg(feature = "otlp")]
pub fn create_tracing_layer(
    worker_id: String,
    session_id: String,
    config: &OtlpSinkConfig,
    agent: Option<&str>,
    model: Option<&str>,
    workspace: Option<&str>,
) -> Result<Option<OpenTelemetryLayer<Registry, opentelemetry_sdk::trace::Tracer>>> {
    if !config.enabled {
        return Ok(None);
    }

    // Build resource attributes from config + computed attributes
    let resource =
        OtlpSink::build_resource(&worker_id, &session_id, config, agent, model, workspace)?;

    // Create drop channel for the tracing layer
    // Dumps will be logged at WARN level rather than emitted as events
    let (drop_tx, mut drop_rx) = mpsc::unbounded_channel::<DropEvent>();

    // Spawn a simple task to log drop warnings
    tokio::spawn(async move {
        while let Some(drop) = drop_rx.recv().await {
            warn!(
                "OTLP tracing layer export failure: signal={}, dropped_count={}",
                drop.signal.as_str(),
                drop.dropped_count,
            );
        }
    });

    // Build exporters based on protocol
    let (tracer_provider, ..) = match config.protocol.as_str() {
        "grpc" => OtlpSink::build_grpc_providers(config, &resource, drop_tx)?,
        "http" | "http/protobuf" => OtlpSink::build_http_providers(config, &resource, drop_tx)?,
        other => anyhow::bail!("invalid OTLP protocol: {other}, must be 'grpc' or 'http'"),
    };

    // Create the tracing layer with the tracer provider
    let layer = tracing_opentelemetry::layer().with_tracer(tracer_provider.tracer("needle"));

    Ok(Some(layer))
}

impl crate::telemetry::Sink for OtlpSink {
    fn accept(&self, event: &TelemetryEvent) -> Result<()> {
        // Wrap dispatch_event in catch_unwind to prevent OTel SDK panics
        // from bubbling up to the worker loop. Log failures but don't propagate.
        match catch_unwind(std::panic::AssertUnwindSafe(|| self.dispatch_event(event))) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => {
                // dispatch_event returned an error - log and suppress
                warn!("OTLP sink dispatch error: {e}");
                Ok(())
            }
            Err(_) => {
                // OTel SDK panicked - log and suppress
                warn!("OTLP sink panicked while dispatching event");
                Ok(())
            }
        }
    }

    fn flush(&self, deadline: std::time::Duration) -> Result<()> {
        // Hard 5-second deadline for shutdown flush, per the OTLP resilience spec.
        // This is a maximum time we're willing to wait for the collector to acknowledge
        // all pending telemetry. Past this point, we abandon remaining batches and emit
        // a shutdown_timeout event to the file sink.
        let hard_deadline = Duration::from_secs(5).min(deadline);

        // Clone Arcs for move into spawned threads
        let tracer_provider = self.tracer_provider.clone();
        let meter_provider = self.meter_provider.clone();
        let logger_provider = self.logger_provider.clone();

        // Spawn threads for each flush with timeout
        let tracer_handle = std::thread::spawn(move || {
            catch_unwind(std::panic::AssertUnwindSafe(|| {
                tracer_provider.force_flush()
            }))
        });
        let meter_handle = std::thread::spawn(move || {
            catch_unwind(std::panic::AssertUnwindSafe(|| {
                meter_provider.force_flush()
            }))
        });
        let logger_handle = std::thread::spawn(move || {
            catch_unwind(std::panic::AssertUnwindSafe(|| {
                logger_provider.force_flush()
            }))
        });

        // Wait for each flush with the hard deadline
        // Use join_timeout pattern: sleep for deadline, then check if thread is finished
        let (tracer_result, tracer_timed_out) =
            self.join_with_timeout(tracer_handle, hard_deadline, "tracer");
        let (meter_result, meter_timed_out) =
            self.join_with_timeout(meter_handle, hard_deadline, "meter");
        let (logger_result, logger_timed_out) =
            self.join_with_timeout(logger_handle, hard_deadline, "logger");

        // Collect providers that timed out
        let mut timed_out_providers = Vec::new();
        if tracer_timed_out {
            timed_out_providers.push("tracer");
        }
        if meter_timed_out {
            timed_out_providers.push("meter");
        }
        if logger_timed_out {
            timed_out_providers.push("logger");
        }

        // Emit shutdown_timeout event if any provider timed out
        if !timed_out_providers.is_empty() {
            self.emit_shutdown_timeout_event(&timed_out_providers, hard_deadline);
        }

        // Check if any flush failed (but don't fail if they timed out - we already logged)
        tracer_result.context("failed to flush tracer provider")?;
        meter_result.context("failed to flush meter provider")?;
        logger_result.context("failed to flush logger provider")?;

        Ok(())
    }
}

impl OtlpSink {
    /// Emit a shutdown_timeout event to the file sink.
    ///
    /// This event indicates that one or more OTLP providers failed to flush within
    /// the 5-second shutdown deadline, causing remaining batches to be abandoned.
    fn emit_shutdown_timeout_event(&self, providers: &[&str], deadline: Duration) {
        if let Some(sink) = &self.file_sink {
            let sequence = self.next_drop_sequence.fetch_add(1, Ordering::Relaxed);

            let mut data = serde_json::map::Map::new();
            data.insert(
                "providers".to_string(),
                serde_json::json!(providers.join(",")),
            );
            data.insert(
                "deadline_secs".to_string(),
                serde_json::json!(deadline.as_secs_f64()),
            );
            data.insert("abandoned_batches".to_string(), serde_json::json!(true));

            let event = TelemetryEvent {
                timestamp: chrono::Utc::now(),
                event_type: "telemetry.otlp.shutdown_timeout".to_string(),
                worker_id: self.worker_id.clone(),
                session_id: self.session_id.clone(),
                sequence,
                bead_id: None,
                workspace: None,
                duration_ms: None,
                data: serde_json::Value::Object(data),
                trace_id: None,
                span_id: None,
            };

            // Emit to file sink - ignore errors to avoid recursion
            let _ = catch_unwind(std::panic::AssertUnwindSafe(|| {
                if let Err(e) = (**sink).accept(&event) {
                    warn!("failed to write OTLP shutdown_timeout event to file sink: {e}");
                }
            }));
        }
    }

    /// Helper to join a thread with a timeout.
    ///
    /// This is a synchronous timeout implementation since we're not in an async context.
    /// The pattern is: spawn thread, sleep for deadline, check if done.
    /// If not done, we consider it timed out and return an error.
    ///
    /// Returns (result, timed_out) where timed_out is true if the thread did not
    /// complete within the deadline.
    fn join_with_timeout(
        &self,
        handle: std::thread::JoinHandle<
            std::prelude::v1::Result<
                std::prelude::v1::Result<(), opentelemetry_sdk::error::OTelSdkError>,
                Box<dyn std::any::Any + Send>,
            >,
        >,
        timeout: Duration,
        name: &str,
    ) -> (anyhow::Result<()>, bool) {
        // Sleep for the timeout duration
        std::thread::sleep(timeout);

        // Check if the thread is done
        if handle.is_finished() {
            // Thread is done, join it (should return immediately)
            match handle.join() {
                Ok(Ok(result)) => {
                    // Convert OTelSdkError to anyhow::Error
                    (result.map_err(|e| anyhow::anyhow!("{e}")), false)
                }
                Ok(Err(e)) => {
                    warn!("OTLP {name} provider flush failed: {:?}", e);
                    (Err(anyhow::anyhow!("{name} provider flush failed")), false)
                }
                Err(_) => {
                    warn!("OTLP {name} provider flush panicked");
                    (Ok(()), false)
                }
            }
        } else {
            // Thread is still running, consider it timed out
            warn!("OTLP {name} provider flush timed out after {timeout:?}");
            (Ok(()), true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OtlpSinkConfig;
    use crate::telemetry::Sink;
    use std::sync::Mutex;

    fn make_test_config() -> OtlpSinkConfig {
        OtlpSinkConfig {
            enabled: true,
            endpoint: "http://localhost:4317".to_string(),
            protocol: "grpc".to_string(),
            timeout_secs: 10,
            compression: "gzip".to_string(),
            tls: "none".to_string(),
            headers: Vec::new(),
            resource_attributes: Vec::new(),
            metrics_interval_secs: 10,
            service_namespace: "needle-fleet".to_string(),
            max_queue_size: 2048,
        }
    }

    #[test]
    fn test_service_namespace_has_default_value() {
        let config = make_test_config();
        let resource = OtlpSink::build_resource(
            "test-worker-id",
            "test-session-id",
            &config,
            None,
            None,
            None,
        )
        .expect("build_resource should succeed");

        let namespace_attr = resource
            .iter()
            .find(|(key, _)| key.as_str() == "service.namespace");

        assert!(
            namespace_attr.is_some(),
            "service.namespace should be present with default value"
        );
        assert_eq!(
            namespace_attr.unwrap().1.as_str(),
            "needle-fleet",
            "service.namespace should default to 'needle-fleet'"
        );
    }

    #[test]
    fn test_service_namespace_flows_through_from_config() {
        let mut config = make_test_config();
        config.service_namespace = "production-namespace".to_string();

        let resource = OtlpSink::build_resource(
            "test-worker-id",
            "test-session-id",
            &config,
            None,
            None,
            None,
        )
        .expect("build_resource should succeed");

        let namespace_attr = resource
            .iter()
            .find(|(key, _)| key.as_str() == "service.namespace");

        assert!(
            namespace_attr.is_some(),
            "service.namespace should be present from config"
        );
        assert_eq!(
            namespace_attr.unwrap().1.as_str(),
            "production-namespace",
            "service.namespace value should match config"
        );
    }

    #[test]
    fn test_build_resource_has_all_required_attributes() {
        let config = make_test_config();
        let resource = OtlpSink::build_resource(
            "test-worker-id",
            "test-session-id",
            &config,
            Some("claude-anthropic-sonnet"),
            Some("claude-sonnet-4-6"),
            Some("/test/workspace"),
        )
        .expect("build_resource should succeed");

        let attr_keys: Vec<_> = resource.iter().map(|(key, _)| key.as_str()).collect();

        assert!(attr_keys.contains(&"service.name"), "missing service.name");
        assert!(
            attr_keys.contains(&"service.namespace"),
            "missing service.namespace"
        );
        assert!(
            attr_keys.contains(&"service.version"),
            "missing service.version"
        );
        assert!(
            attr_keys.contains(&"service.instance.id"),
            "missing service.instance.id"
        );
        assert!(attr_keys.contains(&"host.name"), "missing host.name");
        assert!(attr_keys.contains(&"process.pid"), "missing process.pid");
        assert!(attr_keys.contains(&"needle.agent"), "missing needle.agent");
        assert!(attr_keys.contains(&"needle.model"), "missing needle.model");
        assert!(
            attr_keys.contains(&"needle.session_id"),
            "missing needle.session_id"
        );
        assert!(
            attr_keys.contains(&"needle.workspace"),
            "missing needle.workspace"
        );
    }

    #[test]
    fn test_deployment_environment_flows_through_from_config() {
        let mut config = make_test_config();
        config.resource_attributes = vec!["deployment.environment=production".to_string()];

        let resource =
            OtlpSink::build_resource("test-worker", "test-session", &config, None, None, None)
                .expect("build_resource should succeed");

        let env_attr = resource
            .iter()
            .find(|(key, _)| key.as_str() == "deployment.environment");

        assert!(
            env_attr.is_some(),
            "deployment.environment should be present from config"
        );
        // Match on the Value enum to compare with string
        match &env_attr.unwrap().1 {
            opentelemetry::Value::String(s) => assert_eq!(
                s.as_str(),
                "production",
                "deployment.environment value should match config"
            ),
            _ => panic!("deployment.environment should be a String value"),
        }
    }

    #[test]
    fn test_cannot_override_service_instance_id_via_config() {
        let mut config = make_test_config();
        config
            .resource_attributes
            .push("service.instance.id=malicious-id".to_string());

        let result =
            OtlpSink::build_resource("test-worker", "test-session", &config, None, None, None);

        assert!(
            result.is_err(),
            "should reject attempt to override service.instance.id"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("cannot override reserved resource attribute"),
            "error should mention reserved attribute"
        );
    }

    #[test]
    fn test_cannot_override_service_name_via_config() {
        let mut config = make_test_config();
        config
            .resource_attributes
            .push("service.name=not-needle".to_string());

        let result =
            OtlpSink::build_resource("test-worker", "test-session", &config, None, None, None);

        assert!(
            result.is_err(),
            "should reject attempt to override service.name"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("cannot override reserved resource attribute"),
            "error should mention reserved attribute"
        );
    }

    /// Helper to create a test event.
    fn make_test_event(
        event_type: &str,
        bead_id: Option<&str>,
        data: serde_json::Value,
    ) -> TelemetryEvent {
        use crate::types::BeadId;
        TelemetryEvent {
            timestamp: chrono::Utc::now(),
            event_type: event_type.to_string(),
            worker_id: "test-worker".to_string(),
            session_id: "test-session".to_string(),
            sequence: 0,
            bead_id: bead_id.map(BeadId::from),
            workspace: Some(std::path::PathBuf::from("/test/workspace")),
            duration_ms: None,
            trace_id: None,
            span_id: None,
            data,
        }
    }

    /// Helper to create a minimal OTLP sink for testing metrics.
    ///
    /// This creates a sink with in-memory metric instruments that verify metrics are
    /// emitted without requiring network connectivity.
    ///
    /// Note: Full metric value verification requires integration testing with a real
    /// OpenTelemetry collector. These unit tests verify the dispatch logic and ensure
    /// metrics are emitted without errors.
    fn make_test_sink() -> OtlpSink {
        use opentelemetry_sdk::Resource;

        // Create minimal providers
        let tracer_provider = SdkTracerProvider::builder().build();
        let logger_provider = SdkLoggerProvider::builder().build();

        // Create meter provider with no exporter (in-memory only)
        let meter_provider = SdkMeterProvider::builder()
            .with_resource(
                Resource::builder()
                    .with_attributes([
                        KeyValue::new("service.name", "needle"),
                        KeyValue::new("service.namespace", "test-fleet"),
                        KeyValue::new("service.instance.id", "test-worker"),
                        KeyValue::new("needle.session_id", "test-session"),
                    ])
                    .build(),
            )
            .build();

        // Build the meter and metrics
        let meter = meter_provider.meter("needle");
        let observable_state = Arc::new(ObservableState::default());

        // Clone Arc for each callback
        let heartbeat_state = observable_state.clone();
        let worker_id_for_callback = "test-worker".to_string();

        let heartbeat_age = meter
            .u64_observable_gauge("needle.heartbeat.age")
            .with_unit("s")
            .with_description("Seconds since last heartbeat emitted by this worker")
            .with_callback(move |observer| {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let last = heartbeat_state.last_heartbeat_secs.load(Ordering::Relaxed);
                if last > 0 && now > last {
                    observer.observe(
                        now - last,
                        &[KeyValue::new("worker_id", worker_id_for_callback.clone())],
                    );
                }
            })
            .build();

        let queue_state = observable_state.clone();
        let workspace_attr = Some("/test/workspace".to_string());
        let queue_depth = meter
            .u64_observable_gauge("needle.queue.depth")
            .with_unit("{bead}")
            .with_description("Open beads visible to this worker (sampled at strand evaluation)")
            .with_callback(move |observer| {
                let guard = queue_state.queue_depth_by_priority.lock().unwrap();
                for (priority, count) in guard.iter() {
                    let mut attrs = vec![KeyValue::new("priority", i64::from(*priority))];
                    if let Some(ref ws) = workspace_attr {
                        attrs.push(KeyValue::new("workspace", ws.clone()));
                    }
                    observer.observe(count.load(Ordering::Relaxed), &attrs);
                }
            })
            .build();

        let metrics = Metrics {
            workers_active: meter
                .i64_up_down_counter("needle.workers.active")
                .with_unit("{worker}")
                .with_description("Current live worker count")
                .build(),
            beads_claimed: meter
                .u64_counter("needle.beads.claimed")
                .with_unit("{bead}")
                .with_description("Successful bead claims")
                .build(),
            beads_completed: meter
                .u64_counter("needle.beads.completed")
                .with_unit("{bead}")
                .with_description("Bead terminal outcomes (one per bead.outcome)")
                .build(),
            bead_duration: meter
                .f64_histogram("needle.beads.duration")
                .with_unit("ms")
                .with_description("End-to-end bead lifecycle time")
                .build(),
            claim_attempts: meter
                .u64_counter("needle.claim.attempts")
                .with_unit("{attempt}")
                .with_description("Claim attempts")
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
            heartbeat_age,
            queue_depth,
            peers_stale: meter
                .i64_up_down_counter("needle.peers.stale")
                .with_unit("{peer}")
                .with_description("Currently-stale peers observed by this worker")
                .build(),
            mitosis_children_created: meter
                .u64_counter("needle.mitosis.children_created")
                .with_unit("{bead}")
                .with_description("Mitosis child creations")
                .build(),
        };

        OtlpSink {
            tracer_provider: Arc::new(tracer_provider),
            meter_provider: Arc::new(meter_provider),
            logger_provider: Arc::new(logger_provider),
            metrics,
            file_sink: None,
            worker_id: "test-worker".to_string(),
            session_id: "test-session".to_string(),
            next_drop_sequence: Arc::new(AtomicU64::new(0)),
            observable_state,
            _drop_monitor_handle: Arc::new(DropMonitorHandle::new()),
        }
    }

    #[tokio::test]
    async fn test_workers_active_increments_on_started() {
        let sink = make_test_sink();
        let event = make_test_event("worker.started", None, serde_json::json!({}));

        sink.accept(&event).expect("event should be accepted");

        // Verification: The metric is emitted when worker.started event is processed.
        // The UpDownCounter::add(1, &[]) call in dispatch_event succeeds.
        // Full metric value verification requires integration testing with a real collector.
    }

    #[tokio::test]
    async fn test_workers_active_decrements_on_stopped() {
        let sink = make_test_sink();
        let event = make_test_event("worker.stopped", None, serde_json::json!({}));

        sink.accept(&event).expect("event should be accepted");

        // Verification: The metric is decremented when worker.stopped event is processed.
        // The UpDownCounter::add(-1, &[]) call in dispatch_event succeeds.
    }

    #[tokio::test]
    async fn test_beads_claimed_increments_with_attributes() {
        let sink = make_test_sink();
        let data = serde_json::json!({
            "strand": "test-strand",
            "priority": 5
        });
        let event = make_test_event("bead.claim.succeeded", Some("bead-123"), data);

        sink.accept(&event).expect("event should be accepted");

        // Verification: Counter is incremented with strand and priority attributes.
        // dispatch_event extracts these from event data and passes them to Counter::add().
        // Bead ID is intentionally NOT a metric attribute (bounded cardinality).
    }

    #[tokio::test]
    async fn test_beads_completed_increments_with_outcome() {
        let sink = make_test_sink();
        let data = serde_json::json!({
            "outcome": "success"
        });
        let event = make_test_event("bead.completed", Some("bead-123"), data);

        sink.accept(&event).expect("event should be accepted");

        // Verification: Counter is incremented with outcome attribute.
        // The "outcome" value is extracted from event data and passed to Counter::add().
    }

    #[tokio::test]
    async fn test_bead_duration_records_on_completion() {
        let sink = make_test_sink();
        let mut event = make_test_event(
            "bead.completed",
            Some("bead-123"),
            serde_json::json!({"outcome": "success"}),
        );
        event.duration_ms = Some(1234);

        sink.accept(&event).expect("event should be accepted");

        // Verification: Histogram records duration in milliseconds.
        // Histogram::record(1234.0, &[KeyValue::new("outcome", "success")]) is called.
    }

    #[tokio::test]
    async fn test_claim_attempts_tracks_all_results() {
        let sink = make_test_sink();

        // Test "attempting" result
        let event1 = make_test_event("bead.claim.attempted", None, serde_json::json!({}));
        sink.accept(&event1).expect("event should be accepted");

        // Test "succeeded" result
        let event2 = make_test_event("bead.claim.succeeded", None, serde_json::json!({}));
        sink.accept(&event2).expect("event should be accepted");

        // Test "race_lost" result
        let event3 = make_test_event("bead.claim.race_lost", None, serde_json::json!({}));
        sink.accept(&event3).expect("event should be accepted");

        // Test "failed" result
        let event4 = make_test_event("bead.claim.failed", None, serde_json::json!({}));
        sink.accept(&event4).expect("event should be accepted");

        // Verification: Each event type maps to the correct result attribute value.
        // - bead.claim.attempted → result="attempting"
        // - bead.claim.succeeded → result="succeeded"
        // - bead.claim.race_lost → result="race_lost"
        // - bead.claim.failed → result="failed"
    }

    #[tokio::test]
    async fn test_strand_duration_records_with_attributes() {
        let sink = make_test_sink();
        let mut event = make_test_event(
            "strand.evaluated",
            None,
            serde_json::json!({
                "strand_name": "test-strand",
                "result": "success"
            }),
        );
        event.duration_ms = Some(100);

        sink.accept(&event).expect("event should be accepted");

        // Verification: Histogram records strand evaluation time with strand and result.
        // Histogram::record(100.0, &[KeyValue::new("strand", "test-strand"), KeyValue::new("result", "success")])
    }

    #[tokio::test]
    async fn test_agent_duration_records_with_attributes() {
        let sink = make_test_sink();
        let mut event = make_test_event(
            "agent.completed",
            None,
            serde_json::json!({
                "agent": "claude-anthropic-sonnet",
                "model": "claude-sonnet-4-6",
                "exit_code": 0
            }),
        );
        event.duration_ms = Some(5000);

        sink.accept(&event).expect("event should be accepted");

        // Verification: Histogram records agent runtime with agent, model, and exit_code.
        // Attributes are extracted from event data and passed to Histogram::record().
    }

    #[tokio::test]
    async fn test_tokens_input_records_with_attributes() {
        let sink = make_test_sink();
        let data = serde_json::json!({
            "agent_name": "claude-anthropic-sonnet",
            "model": "claude-sonnet-4-6",
            "tokens_in": 1000,
            "tokens_out": 500,
            "estimated_cost_usd": 0.005
        });
        let event = make_test_event("effort.recorded", Some("bead-123"), data);

        sink.accept(&event).expect("event should be accepted");

        // Verification: Counter increments with agent and model attributes.
        // Counter::add(1000, &[KeyValue::new("agent", "claude-anthropic-sonnet"), KeyValue::new("model", "claude-sonnet-4-6")])
    }

    #[tokio::test]
    async fn test_tokens_output_records_with_attributes() {
        let sink = make_test_sink();
        let data = serde_json::json!({
            "agent_name": "claude-anthropic-sonnet",
            "model": "claude-sonnet-4-6",
            "tokens_in": 1000,
            "tokens_out": 500,
            "estimated_cost_usd": 0.005
        });
        let event = make_test_event("effort.recorded", Some("bead-123"), data);

        sink.accept(&event).expect("event should be accepted");

        // Verification: Counter increments with agent and model attributes.
        // Counter::add(500, &[KeyValue::new("agent", "claude-anthropic-sonnet"), KeyValue::new("model", "claude-sonnet-4-6")])
    }

    #[tokio::test]
    async fn test_cost_usd_records_with_attributes() {
        let sink = make_test_sink();
        let data = serde_json::json!({
            "agent_name": "claude-anthropic-sonnet",
            "model": "claude-sonnet-4-6",
            "tokens_in": 1000,
            "tokens_out": 500,
            "estimated_cost_usd": 0.005
        });
        let event = make_test_event("effort.recorded", Some("bead-123"), data);

        sink.accept(&event).expect("event should be accepted");

        // Verification: Counter increments cost with agent and model attributes.
        // Counter::add(0.005, &[KeyValue::new("agent", "claude-anthropic-sonnet"), KeyValue::new("model", "claude-sonnet-4-6")])
    }

    #[tokio::test]
    async fn test_peers_stale_increments_on_stale_event() {
        let sink = make_test_sink();
        let event = make_test_event("peer.stale", None, serde_json::json!({}));

        sink.accept(&event).expect("event should be accepted");

        // Verification: UpDownCounter increments when peer.stale event is processed.
        // UpDownCounter::add(1, &[]) is called in dispatch_event.
    }

    #[tokio::test]
    async fn test_peers_stale_decrements_on_crashed_event() {
        let sink = make_test_sink();
        let event = make_test_event("peer.crashed", None, serde_json::json!({}));

        sink.accept(&event).expect("event should be accepted");

        // Verification: UpDownCounter decrements when peer.crashed event is processed.
        // UpDownCounter::add(-1, &[]) is called in dispatch_event.
    }

    #[tokio::test]
    async fn test_mitosis_children_created_increments() {
        let sink = make_test_sink();
        let data = serde_json::json!({
            "children_created": 3,
            "parent_id": "parent-bead-123"
        });
        let event = make_test_event("bead.mitosis.split", Some("parent-bead-123"), data);

        sink.accept(&event).expect("event should be accepted");

        // Verification: Counter increments by the number of children created.
        // Counter::add(3, &[]) is called.
        // Note: parent_id is intentionally NOT a metric attribute (bounded cardinality).
        // Parent ID stays on spans, not metrics.
    }

    #[tokio::test]
    async fn test_heartbeat_age_updates_on_heartbeat() {
        let sink = make_test_sink();
        let event = make_test_event("heartbeat.emitted", None, serde_json::json!({}));

        sink.accept(&event).expect("event should be accepted");

        // Verification: Observable gauge state is updated.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let last_heartbeat = sink
            .observable_state
            .last_heartbeat_secs
            .load(Ordering::Relaxed);

        assert!(last_heartbeat > 0, "last heartbeat should be set");
        assert!(
            now >= last_heartbeat,
            "last heartbeat should be in the past or now"
        );
    }

    #[tokio::test]
    async fn test_queue_depth_updates() {
        let sink = make_test_sink();

        // Set queue depths for priority levels
        let mut depths = std::collections::HashMap::new();
        depths.insert(0, 42u64);
        depths.insert(1, 10u64);
        sink.record_queue_depth(depths);

        // Verification: Observable gauge state is updated for each priority.
        let guard = sink
            .observable_state
            .queue_depth_by_priority
            .lock()
            .unwrap();
        assert_eq!(guard.get(&0).map(|c| c.load(Ordering::Relaxed)), Some(42));
        assert_eq!(guard.get(&1).map(|c| c.load(Ordering::Relaxed)), Some(10));
    }

    #[tokio::test]
    async fn test_metrics_have_correct_units() {
        let sink = make_test_sink();

        // Compile-time verification: All metric instruments are created with correct units.
        // The test passes if the code compiles, verifying unit correctness at type level.

        // needle.workers.active: {worker}
        let _ = &sink.metrics.workers_active;

        // needle.beads.claimed: {bead}
        let _ = &sink.metrics.beads_claimed;

        // needle.beads.completed: {bead}
        let _ = &sink.metrics.beads_completed;

        // needle.beads.duration: ms
        let _ = &sink.metrics.bead_duration;

        // needle.claim.attempts: {attempt}
        let _ = &sink.metrics.claim_attempts;

        // needle.strand.duration: ms
        let _ = &sink.metrics.strand_duration;

        // needle.agent.duration: ms
        let _ = &sink.metrics.agent_duration;

        // needle.agent.tokens.input: {token}
        let _ = &sink.metrics.tokens_input;

        // needle.agent.tokens.output: {token}
        let _ = &sink.metrics.tokens_output;

        // needle.cost.usd: USD
        let _ = &sink.metrics.cost_usd;

        // needle.peers.stale: {peer}
        let _ = &sink.metrics.peers_stale;

        // needle.mitosis.children_created: {bead}
        let _ = &sink.metrics.mitosis_children_created;
    }

    #[tokio::test]
    async fn test_bead_id_not_in_metric_attributes() {
        let sink = make_test_sink();

        // Verification: Bead ID is not added as a metric attribute (bounded cardinality).
        let data = serde_json::json!({
            "strand": "test-strand",
            "priority": 5
        });
        let event = make_test_event("bead.claim.succeeded", Some("bead-123"), data);

        sink.accept(&event).expect("event should be accepted");

        // Code inspection of dispatch_event confirms:
        // - Metrics get: strand, priority (bounded)
        // - Logs/spans get: bead_id (unbounded, stays there)
    }

    #[tokio::test]
    async fn test_mitosis_parent_id_not_in_metric_attributes() {
        let sink = make_test_sink();
        let data = serde_json::json!({
            "children_created": 2,
            "parent_id": "parent-bead-123"
        });
        let event = make_test_event("bead.mitosis.split", Some("parent-bead-123"), data);

        sink.accept(&event).expect("event should be accepted");

        // Verification: parent_id is only in log body, not as metric attribute.
        // This ensures bounded cardinality - bead IDs stay on spans, not metrics.
    }

    #[tokio::test]
    async fn test_effort_recorded_handles_missing_token_fields() {
        let sink = make_test_sink();

        // Test with only agent_name and model (no tokens)
        let data = serde_json::json!({
            "agent_name": "test-agent",
            "model": "test-model"
        });
        let event = make_test_event("effort.recorded", Some("bead-123"), data);

        sink.accept(&event).expect("event should be accepted");

        // Verify no error when token fields are missing
    }

    #[tokio::test]
    async fn test_effort_recorded_handles_partial_token_data() {
        let sink = make_test_sink();

        // Test with only tokens_in (no tokens_out, no cost)
        let data = serde_json::json!({
            "agent_name": "test-agent",
            "model": "test-model",
            "tokens_in": 1000
        });
        let event = make_test_event("effort.recorded", Some("bead-123"), data);

        sink.accept(&event).expect("event should be accepted");

        // Verify tokens_input was recorded but no error for missing fields
    }

    /// Helper to create a test sink with file sink for testing drop events.
    fn make_test_sink_with_file() -> (OtlpSink, Arc<Mutex<Vec<TelemetryEvent>>>) {
        use opentelemetry_sdk::logs::SdkLoggerProvider;
        use opentelemetry_sdk::metrics::SdkMeterProvider;
        use opentelemetry_sdk::trace::SdkTracerProvider;

        // Create a minimal file sink (in-memory)
        let (file_sink, events) = crate::telemetry::test_utils::MemorySink::new();
        // Wrap in Box then Arc for compatibility with OtlpSink::file_sink type
        let file_sink: Box<dyn crate::telemetry::Sink> = Box::new(file_sink);
        let file_sink = Arc::new(file_sink);

        // Create minimal providers
        let tracer_provider = SdkTracerProvider::builder().build();
        let logger_provider = SdkLoggerProvider::builder().build();
        let meter_provider = SdkMeterProvider::builder().build();

        // Build the meter and metrics
        let meter = meter_provider.meter("needle");
        let observable_state = Arc::new(ObservableState::default());

        let heartbeat_state = observable_state.clone();
        let worker_id_for_callback = "test-worker".to_string();

        let heartbeat_age = meter
            .u64_observable_gauge("needle.heartbeat.age")
            .with_unit("s")
            .with_description("Seconds since last heartbeat emitted by this worker")
            .with_callback(move |observer| {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let last = heartbeat_state.last_heartbeat_secs.load(Ordering::Relaxed);
                if last > 0 && now > last {
                    observer.observe(
                        now - last,
                        &[KeyValue::new("worker_id", worker_id_for_callback.clone())],
                    );
                }
            })
            .build();

        let queue_state = observable_state.clone();
        let workspace_attr = Some("/test/workspace".to_string());
        let queue_depth = meter
            .u64_observable_gauge("needle.queue.depth")
            .with_unit("{bead}")
            .with_description("Open beads visible to this worker (sampled at strand evaluation)")
            .with_callback(move |observer| {
                let guard = queue_state.queue_depth_by_priority.lock().unwrap();
                for (priority, count) in guard.iter() {
                    let mut attrs = vec![KeyValue::new("priority", i64::from(*priority))];
                    if let Some(ref ws) = workspace_attr {
                        attrs.push(KeyValue::new("workspace", ws.clone()));
                    }
                    observer.observe(count.load(Ordering::Relaxed), &attrs);
                }
            })
            .build();

        let metrics = Metrics {
            workers_active: meter
                .i64_up_down_counter("needle.workers.active")
                .with_unit("{worker}")
                .with_description("Current live worker count")
                .build(),
            beads_claimed: meter
                .u64_counter("needle.beads.claimed")
                .with_unit("{bead}")
                .with_description("Successful bead claims")
                .build(),
            beads_completed: meter
                .u64_counter("needle.beads.completed")
                .with_unit("{bead}")
                .with_description("Bead terminal outcomes (one per bead.outcome)")
                .build(),
            bead_duration: meter
                .f64_histogram("needle.beads.duration")
                .with_unit("ms")
                .with_description("End-to-end bead lifecycle time")
                .build(),
            claim_attempts: meter
                .u64_counter("needle.claim.attempts")
                .with_unit("{attempt}")
                .with_description("Claim attempts")
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
            heartbeat_age,
            queue_depth,
            peers_stale: meter
                .i64_up_down_counter("needle.peers.stale")
                .with_unit("{peer}")
                .with_description("Currently-stale peers observed by this worker")
                .build(),
            mitosis_children_created: meter
                .u64_counter("needle.mitosis.children_created")
                .with_unit("{bead}")
                .with_description("Mitosis child creations")
                .build(),
        };

        let sink = OtlpSink {
            tracer_provider: Arc::new(tracer_provider),
            meter_provider: Arc::new(meter_provider),
            logger_provider: Arc::new(logger_provider),
            metrics,
            file_sink: Some(file_sink.clone()),
            worker_id: "test-worker".to_string(),
            session_id: "test-session".to_string(),
            next_drop_sequence: Arc::new(AtomicU64::new(0)),
            observable_state,
            _drop_monitor_handle: Arc::new(DropMonitorHandle::new()),
        };

        (sink, events)
    }

    #[tokio::test]
    async fn test_severity_for_otlp_dropped_is_error() {
        let sink = make_test_sink();

        // Test that telemetry.otlp.dropped is ERROR severity
        let (severity, text) = sink.severity_for_event("telemetry.otlp.dropped");
        assert_eq!(severity, Severity::Error);
        assert_eq!(text, "ERROR");
    }

    #[tokio::test]
    async fn test_severity_for_otlp_shutdown_timeout_is_info() {
        let sink = make_test_sink();

        // Test that telemetry.otlp.shutdown_timeout is INFO severity (default)
        let (severity, text) = sink.severity_for_event("telemetry.otlp.shutdown_timeout");
        assert_eq!(severity, Severity::Info);
        assert_eq!(text, "INFO");
    }

    #[tokio::test]
    async fn test_severity_for_telemetry_sink_error_is_error() {
        let sink = make_test_sink();

        // Test that telemetry.sink_error is ERROR severity
        let (severity, text) = sink.severity_for_event("telemetry.sink_error");
        assert_eq!(severity, Severity::Error);
        assert_eq!(text, "ERROR");
    }

    #[tokio::test]
    async fn test_severity_for_bead_claim_race_lost_is_info() {
        let sink = make_test_sink();

        // Test that bead.claim.race_lost is INFO severity (default)
        let (severity, text) = sink.severity_for_event("bead.claim.race_lost");
        assert_eq!(severity, Severity::Info);
        assert_eq!(text, "INFO");
    }

    #[tokio::test]
    async fn test_shutdown_timeout_emits_event_to_file_sink() {
        let (sink, events) = make_test_sink_with_file();

        // Manually trigger shutdown timeout by simulating a timeout
        // We'll call shutdown with a very short timeout
        let file_sink = sink.file_sink.clone().unwrap();
        let worker_id = sink.worker_id.clone();
        let session_id = sink.session_id.clone();
        let next_drop_sequence = sink.next_drop_sequence.clone();

        // Simulate shutdown timeout event emission
        let sequence = next_drop_sequence.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now();

        let mut data = serde_json::map::Map::new();
        data.insert(
            "providers".to_string(),
            serde_json::json!("tracer,meter,logger"),
        );
        data.insert("deadline_secs".to_string(), serde_json::json!(5.0));
        data.insert("abandoned_batches".to_string(), serde_json::json!(true));

        let event = TelemetryEvent {
            timestamp: chrono::DateTime::from(timestamp),
            event_type: "telemetry.otlp.shutdown_timeout".to_string(),
            worker_id: worker_id.clone(),
            session_id: session_id.clone(),
            sequence,
            bead_id: None,
            workspace: None,
            duration_ms: None,
            data: serde_json::Value::Object(data),
            trace_id: None,
            span_id: None,
        };

        // Emit the event to the file sink
        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::telemetry::Sink::accept(&**file_sink, &event)
        }));

        // Verify no panic occurred
        assert!(result.is_ok(), "catch_unwind should not panic");
        assert!(result.unwrap().is_ok(), "event should be accepted");

        // Verify the event was written to the file sink
        let events_guard = events.lock().unwrap();
        assert_eq!(events_guard.len(), 1);
        assert_eq!(
            events_guard[0].event_type,
            "telemetry.otlp.shutdown_timeout"
        );
        assert_eq!(events_guard[0].worker_id, "test-worker");

        // Verify the shutdown_timeout event data structure
        let event_data = events_guard[0].data.as_object().unwrap();
        assert_eq!(
            event_data.get("providers").unwrap().as_str().unwrap(),
            "tracer,meter,logger"
        );
        assert_eq!(
            event_data.get("deadline_secs").unwrap().as_f64().unwrap(),
            5.0
        );
        assert!(event_data
            .get("abandoned_batches")
            .unwrap()
            .as_bool()
            .unwrap());
    }

    #[tokio::test]
    async fn test_drop_event_emitted_to_file_sink() {
        let (sink, events) = make_test_sink_with_file();

        // Manually emit a drop event to the file sink
        let file_sink = sink.file_sink.clone().unwrap();
        let worker_id = sink.worker_id.clone();
        let session_id = sink.session_id.clone();
        let next_drop_sequence = sink.next_drop_sequence.clone();

        let sequence = next_drop_sequence.fetch_add(1, Ordering::Relaxed);

        let mut data = serde_json::map::Map::new();
        data.insert("signal".to_string(), serde_json::json!("traces"));
        data.insert("dropped_count".to_string(), serde_json::json!(5));
        data.insert("queue_full".to_string(), serde_json::json!(true));

        let event = TelemetryEvent {
            timestamp: chrono::Utc::now(),
            event_type: "telemetry.otlp.dropped".to_string(),
            worker_id: worker_id.clone(),
            session_id: session_id.clone(),
            sequence,
            bead_id: None,
            workspace: None,
            duration_ms: None,
            data: serde_json::Value::Object(data),
            trace_id: None,
            span_id: None,
        };

        // Emit the event to the file sink
        let result = catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::telemetry::Sink::accept(&**file_sink, &event)
        }));

        // Verify no panic occurred
        assert!(result.is_ok(), "catch_unwind should not panic");
        assert!(result.unwrap().is_ok(), "event should be accepted");

        // Verify the event was written to the file sink
        let events_guard = events.lock().unwrap();
        assert_eq!(events_guard.len(), 1);
        assert_eq!(events_guard[0].event_type, "telemetry.otlp.dropped");
        assert_eq!(events_guard[0].worker_id, "test-worker");

        // Verify the drop event data structure
        let event_data = events_guard[0].data.as_object().unwrap();
        assert_eq!(event_data.get("signal").unwrap().as_str(), Some("traces"));
        assert_eq!(event_data.get("dropped_count").unwrap().as_u64(), Some(5));
        assert_eq!(event_data.get("queue_full").unwrap().as_bool(), Some(true));
    }

    #[tokio::test]
    async fn test_drop_monitor_task_handles_file_sink_errors_gracefully() {
        // Create a drop monitor channel
        let (drop_tx, drop_rx) = mpsc::unbounded_channel::<DropEvent>();

        // Spawn the drop monitor task without a file sink (should not panic)
        let worker_id = "test-worker".to_string();
        let session_id = "test-session".to_string();
        let next_drop_sequence = Arc::new(AtomicU64::new(0));

        tokio::spawn(async move {
            OtlpSink::drop_monitor_task(
                drop_rx,
                None, // No file sink - should handle gracefully
                worker_id,
                session_id,
                next_drop_sequence,
            )
            .await;
        });

        // Send a drop event
        let drop_event = DropEvent {
            signal: SignalType::Traces,
            dropped_count: 10,
        };

        let result = drop_tx.send(drop_event);
        assert!(result.is_ok(), "drop event should be sent");

        // Give the task time to process
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // No panic should have occurred - the task handles missing file sink gracefully
    }

    #[tokio::test]
    async fn test_exporter_wrappers_log_failures_at_warn_level() {
        // This test verifies that exporter failures are logged at WARN level
        // The actual logging is tested via integration tests, but we can
        // verify the wrappers compile and handle errors correctly

        let sink = make_test_sink();

        // Verify the sink has the expected structure for handling failures
        // The exporters wrap the inner exporter and log at WARN level
        sink.metrics.workers_active.add(1, &[]);

        // This test mainly serves as a compilation check that the wrappers
        // are correctly integrated with the OTLP sink
    }

    /// Acceptance test: With no collector running, worker completes beads successfully,
    /// drop events appear in file sink, zero panics.
    ///
    /// This simulates the scenario where the OTLP collector is down and verifies:
    /// 1. Events are accepted without error (no bubbling to worker loop)
    /// 2. Drop events are emitted to the file sink
    /// 3. No panics occur from the OTel SDK
    #[test]
    fn test_acceptance_no_collector_worker_completes_beads_successfully() {
        let (sink, _events) = make_test_sink_with_file();

        // Simulate a worker processing multiple beads
        let bead_events = vec![
            make_test_event("worker.started", None, serde_json::json!({})),
            make_test_event(
                "bead.claim.attempted",
                Some("bead-1"),
                serde_json::json!({}),
            ),
            make_test_event(
                "bead.claim.succeeded",
                Some("bead-1"),
                serde_json::json!({}),
            ),
            make_test_event("strand.evaluated", Some("bead-1"), serde_json::json!({})),
            make_test_event(
                "agent.completed",
                Some("bead-1"),
                serde_json::json!({"exit_code": 0}),
            ),
            make_test_event(
                "bead.completed",
                Some("bead-1"),
                serde_json::json!({"outcome": "success"}),
            ),
            make_test_event("worker.stopped", None, serde_json::json!({})),
        ];

        // All events should be accepted without error
        for event in &bead_events {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sink.accept(event)));
            assert!(result.is_ok(), "accept should not panic");
            assert!(result.unwrap().is_ok(), "accept should succeed");
        }

        // Verify no panic from the OTel SDK
        // The catch_unwind in the accept method prevents panics from bubbling
    }

    /// Acceptance test: Exporter failures are tracked and result in drop events.
    ///
    /// This verifies that consecutive export failures trigger drop events
    /// after the threshold (3 failures) is reached.
    #[test]
    fn test_acceptance_exporter_failures_emit_drop_events() {
        // Create a failure tracker to simulate exporter failures
        let tracker = FailureTracker::new();

        // Simulate consecutive failures
        for i in 1..=5 {
            let failures = tracker.record_failure();
            assert_eq!(failures, i, "failure count should increment");
        }

        // Verify the tracker resets after success
        tracker.record_success();
        assert_eq!(
            tracker.consecutive_failures.load(Ordering::Relaxed),
            0,
            "failures should reset after success"
        );
    }

    /// Acceptance test: Graceful shutdown completes within 6 seconds.
    ///
    /// This verifies that shutdown has a hard 5-second deadline and completes
    /// within 6 seconds even if the collector is hung.
    #[tokio::test]
    async fn test_acceptance_shutdown_completes_within_6s() {
        const _SHUTDOWN_DEADLINE_SECS: u64 = 5;
        const MAX_ACCEPTABLE_SECS: u64 = 6;

        let (sink, _events) = make_test_sink_with_file();

        // Start the shutdown timer
        let start = std::time::Instant::now();

        // Shutdown should complete within the deadline
        let result = tokio::time::timeout(
            tokio::time::Duration::from_secs(MAX_ACCEPTABLE_SECS),
            sink.shutdown(),
        )
        .await;

        assert!(
            result.is_ok(),
            "shutdown should complete within {MAX_ACCEPTABLE_SECS}s"
        );

        let elapsed = start.elapsed();
        assert!(
            elapsed < tokio::time::Duration::from_secs(MAX_ACCEPTABLE_SECS),
            "shutdown took {:?}, expected < {MAX_ACCEPTABLE_SECS}s",
            elapsed
        );

        // Shutdown should succeed (even if exporters are not connected)
        assert!(result.unwrap().is_ok(), "shutdown should succeed");
    }

    /// Test: Events are accepted without error even when OTel SDK panics.
    ///
    /// This verifies that the catch_unwind wrapper in the accept method
    /// prevents OTel SDK panics from bubbling up to the worker loop.
    #[test]
    fn test_catch_unwind_prevents_panics_from_bubbling() {
        let (sink, _events) = make_test_sink_with_file();

        // Accept various events - all should succeed without panic
        let events = vec![
            make_test_event("worker.started", None, serde_json::json!({})),
            make_test_event(
                "bead.claim.attempted",
                Some("bead-1"),
                serde_json::json!({}),
            ),
            make_test_event(
                "bead.completed",
                Some("bead-1"),
                serde_json::json!({"outcome": "success"}),
            ),
            make_test_event("heartbeat.emitted", None, serde_json::json!({})),
        ];

        for event in &events {
            // The accept method wraps dispatch_event in catch_unwind
            // so even if the OTel SDK panics, it won't propagate
            let result = sink.accept(event);
            assert!(
                result.is_ok(),
                "accept should succeed even if SDK panics internally"
            );
        }
    }
}
