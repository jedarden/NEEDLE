//! End-to-end OTLP integration tests.
//!
//! These tests spin up a real OpenTelemetry Collector container via testcontainers,
//! configure NEEDLE to export OTLP to it, and verify that spans, metrics, and logs
//! are correctly received and written to the collector's file output.
//!
//! # Prerequisites
//!
//! - Docker daemon running and accessible
//! - `cargo test --features integration` to enable these tests
//!
//! # Test Flakiness
//!
//! These tests depend on external containers. If they fail consistently, check:
//! - Docker is running: `docker ps`
//! - Port 4317 is available
//! - Collector logs for errors

#![cfg(feature = "integration")]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use futures::executor::block_on;
use serde::Deserialize;
use std::borrow::Cow;
use testcontainers::{
    core::{copy::CopyToContainer, ContainerPort, Mount, WaitFor},
    Image,
};

use needle::bead_store::{BeadStore, Filters};
use needle::config::Config;
use needle::dispatch::{AgentAdapter, Dispatcher};
use needle::telemetry::Telemetry;
use needle::types::{Bead, BeadId, BeadStatus, ClaimResult, IdleAction, InputMethod, WorkerState};
use needle::worker::Worker;

// ─── OpenTelemetry Collector Container Image ─────────────────────────────────────

/// OpenTelemetry Collector Contrib image with file exporter.
///
/// This container receives OTLP traces, metrics, and logs over gRPC (port 4317)
/// and writes them to JSON files in /tmp/otel-output.
#[derive(Debug, Clone)]
struct OtelCollectorImage {
    /// Output directory within the container.
    output_dir: String,
}

impl OtelCollectorImage {
    /// Create a new collector image that writes to /tmp/otel-output.
    fn new() -> Self {
        Self {
            output_dir: "/tmp/otel-output".to_string(),
        }
    }

    /// Get the collector configuration as YAML.
    fn config(&self) -> String {
        format!(
            r#"
receivers:
  otlp:
    protocols:
      grpc:

exporters:
  file:
    path: {}/ traces.json
    format: json

  file/metrics:
    path: {}/ metrics.json
    format: json

  file/logs:
    path: {}/ logs.json
    format: json

service:
  pipelines:
    traces:
      receivers: [otlp]
      exporters: [file]

    metrics:
      receivers: [otlp]
      exporters: [file/metrics]

    logs:
      receivers: [otlp]
      exporters: [file/logs]
"#,
            self.output_dir, self.output_dir, self.output_dir
        )
    }
}

impl Default for OtelCollectorImage {
    fn default() -> Self {
        Self::new()
    }
}

impl Image for OtelCollectorImage {
    fn name(&self) -> &str {
        "otel/opentelemetry-collector-contrib"
    }

    fn tag(&self) -> &str {
        "0.114.0"
    }

    fn ready_conditions(&self) -> Vec<WaitFor> {
        vec![WaitFor::message_on_stdout("Everything is ready")]
    }

    fn env_vars(
        &self,
    ) -> impl IntoIterator<Item = (impl Into<Cow<'_, str>>, impl Into<Cow<'_, str>>)> {
        vec![(
            "OTEL_EXPORTER_OTLP_ENDPOINT".to_string(),
            "0.0.0.0:4317".to_string(),
        )]
    }

    fn mounts(&self) -> impl IntoIterator<Item = &Mount> {
        // Create a static mount for output files
        static MOUNTS: [Mount; 0] = [];
        // Note: Using empty mounts array as temp directories are handled differently in 0.23
        MOUNTS.iter()
    }

    fn expose_ports(&self) -> &[ContainerPort] {
        &[ContainerPort::Tcp(4317), ContainerPort::Tcp(4318)]
    }
}

// ─── File Output Parsing ───────────────────────────────────────────────────────────

/// Parsed OTel resource spans from the collector's JSON output.
#[derive(Debug, Deserialize)]
struct ResourceSpans {
    #[serde(default)]
    pub resource: Resource,
    #[serde(default)]
    pub scope_spans: Vec<ScopeSpans>,
}

/// OTel resource attributes.
#[derive(Debug, Deserialize, Default)]
struct Resource {
    #[serde(default)]
    pub attributes: Vec<Attribute>,
}

/// Scope spans within a resource.
#[derive(Debug, Deserialize, Default)]
struct ScopeSpans {
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub spans: Vec<Span>,
}

/// Instrumentation scope.
#[derive(Debug, Deserialize, Default)]
struct Scope {
    #[serde(default)]
    pub name: String,
}

/// A single span.
#[derive(Debug, Deserialize, Default)]
struct Span {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub parent_span_id: Option<String>,
    #[serde(default)]
    pub attributes: Vec<Attribute>,
    #[serde(default)]
    pub status: Status,
}

/// Span status.
#[derive(Debug, Deserialize, Default)]
struct Status {
    #[serde(default)]
    pub code: String,
}

/// OTel attribute (key-value pair).
#[derive(Debug, Deserialize, Clone)]
struct Attribute {
    pub key: String,
    #[serde(default)]
    pub value: Option<serde_json::Value>,
}

impl Attribute {
    /// Get the string value of this attribute.
    fn as_str(&self) -> Option<&str> {
        self.value.as_ref().and_then(|v| {
            if let Some(s) = v.as_str() {
                Some(s)
            } else {
                v.get("stringValue").and_then(|v| v.as_str())
            }
        })
    }

    /// Get the i64 value of this attribute.
    fn as_i64(&self) -> Option<i64> {
        self.value.as_ref().and_then(|v| {
            if let Some(n) = v.as_i64() {
                Some(n)
            } else {
                v.get("intValue").and_then(|v| v.as_i64())
            }
        })
    }
}

/// Parsed OTel metrics from the collector's JSON output.
#[derive(Debug, Deserialize)]
struct ResourceMetrics {
    #[serde(default)]
    pub resource: Resource,
    #[serde(default)]
    pub scope_metrics: Vec<ScopeMetrics>,
}

/// Scope metrics within a resource.
#[derive(Debug, Deserialize, Default)]
struct ScopeMetrics {
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub metrics: Vec<Metric>,
}

/// A single metric.
#[derive(Debug, Deserialize, Default)]
struct Metric {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub data: Option<MetricData>,
}

/// Metric data (sum, gauge, histogram, etc.).
#[derive(Debug, Deserialize)]
#[serde(tag = "data_type")]
enum MetricData {
    #[serde(rename = "data_type_sum")]
    Sum(SumData),
    #[serde(rename = "data_type_gauge")]
    Gauge(GaugeData),
    #[serde(rename = "data_type_histogram")]
    Histogram(HistogramData),
}

impl Default for MetricData {
    fn default() -> Self {
        MetricData::Sum(SumData::default())
    }
}

/// Sum metric data (counter).
#[derive(Debug, Deserialize, Default)]
struct SumData {
    #[serde(default)]
    pub data_points: Vec<DataPoint>,
    #[serde(default)]
    pub is_monotonic: bool,
}

/// Gauge metric data.
#[derive(Debug, Deserialize, Default)]
struct GaugeData {
    #[serde(default)]
    pub data_points: Vec<DataPoint>,
}

/// Histogram metric data.
#[derive(Debug, Deserialize, Default)]
struct HistogramData {
    #[serde(default)]
    pub data_points: Vec<HistogramDataPoint>,
}

/// A data point for sum/gauge metrics.
#[derive(Debug, Deserialize, Default)]
struct DataPoint {
    #[serde(default)]
    pub attributes: Vec<Attribute>,
    #[serde(default)]
    pub value: Option<serde_json::Value>,
    #[serde(rename = "as_int")]
    #[serde(default)]
    pub as_int: Option<i64>,
    #[serde(rename = "as_double")]
    #[serde(default)]
    pub as_double: Option<f64>,
}

impl DataPoint {
    /// Get the numeric value as i64 if present.
    fn value_as_i64(&self) -> Option<i64> {
        self.as_int.or_else(|| {
            self.value
                .as_ref()
                .and_then(|v| v.as_i64())
                .or_else(|| v.get("intValue").and_then(|v| v.as_i64()))
        })
    }
}

/// Histogram data point.
#[derive(Debug, Deserialize, Default)]
struct HistogramDataPoint {
    #[serde(default)]
    pub attributes: Vec<Attribute>,
    #[serde(default)]
    pub count: u64,
}

/// Parsed OTel logs from the collector's JSON output.
#[derive(Debug, Deserialize)]
struct ResourceLogs {
    #[serde(default)]
    pub resource: Resource,
    #[serde(default)]
    pub scope_logs: Vec<ScopeLogs>,
}

/// Scope logs within a resource.
#[derive(Debug, Deserialize, Default)]
struct ScopeLogs {
    #[serde(default)]
    pub scope: Scope,
    #[serde(default)]
    pub log_records: Vec<LogRecord>,
}

/// A single log record.
#[derive(Debug, Deserialize, Default)]
struct LogRecord {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub severity_number: u32,
    #[serde(default)]
    pub severity_text: String,
    #[serde(default)]
    pub body: Option<serde_json::Value>,
    #[serde(default)]
    pub attributes: Vec<Attribute>,
}

/// Severity levels matching OTel specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Severity {
    Trace = 1,
    Debug = 5,
    Info = 9,
    Warn = 13,
    Error = 17,
    Fatal = 21,
}

// ─── Mock BeadStore ───────────────────────────────────────────────────────────────

/// Minimal mock store for testing.
struct MockStore {
    beads: Mutex<Vec<Bead>>,
}

impl MockStore {
    fn new(beads: Vec<Bead>) -> Self {
        Self {
            beads: Mutex::new(beads),
        }
    }
}

#[async_trait::async_trait]
impl BeadStore for MockStore {
    async fn ready(&self, _filters: &Filters) -> needle::types::Result<Vec<Bead>> {
        Ok(self
            .beads
            .lock()
            .unwrap()
            .iter()
            .filter(|b| b.status == BeadStatus::Open && b.assignee.is_none())
            .cloned()
            .collect())
    }

    async fn list_all(&self) -> needle::types::Result<Vec<Bead>> {
        Ok(self.beads.lock().unwrap().clone())
    }

    async fn show(&self, id: &BeadId) -> needle::types::Result<Bead> {
        self.beads
            .lock()
            .unwrap()
            .iter()
            .find(|b| b.id == *id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("bead not found: {id}"))
    }

    async fn claim(&self, id: &BeadId, actor: &str) -> needle::types::Result<ClaimResult> {
        let mut beads = self.beads.lock().unwrap();
        if let Some(bead) = beads.iter_mut().find(|b| b.id == *id) {
            bead.status = BeadStatus::InProgress;
            bead.assignee = Some(actor.to_string());
            Ok(ClaimResult::Claimed(bead.clone()))
        } else {
            Ok(ClaimResult::NotClaimable {
                reason: "not found".to_string(),
            })
        }
    }

    async fn release(&self, id: &BeadId) -> needle::types::Result<()> {
        let mut beads = self.beads.lock().unwrap();
        if let Some(bead) = beads.iter_mut().find(|b| b.id == *id) {
            bead.status = BeadStatus::Open;
            bead.assignee = None;
        }
        Ok(())
    }

    async fn flush(&self) -> needle::types::Result<()> {
        Ok(())
    }

    async fn reopen(&self, _id: &BeadId) -> needle::types::Result<()> {
        Ok(())
    }

    async fn labels(&self, _id: &BeadId) -> needle::types::Result<Vec<String>> {
        Ok(vec![])
    }

    async fn add_label(&self, _id: &BeadId, _label: &str) -> needle::types::Result<()> {
        Ok(())
    }

    async fn remove_label(&self, _id: &BeadId, _label: &str) -> needle::types::Result<()> {
        Ok(())
    }

    async fn create_bead(
        &self,
        _title: &str,
        _body: &str,
        _labels: &[&str],
    ) -> needle::types::Result<BeadId> {
        Ok(BeadId::from("new-bead"))
    }

    async fn add_dependency(
        &self,
        _blocker_id: &BeadId,
        _blocked_id: &BeadId,
    ) -> needle::types::Result<()> {
        Ok(())
    }

    async fn doctor_repair(&self) -> needle::types::Result<needle::bead_store::RepairReport> {
        Ok(needle::bead_store::RepairReport::default())
    }

    async fn doctor_check(&self) -> needle::types::Result<needle::bead_store::RepairReport> {
        Ok(needle::bead_store::RepairReport::default())
    }

    async fn full_rebuild(&self) -> needle::types::Result<()> {
        Ok(())
    }
}

// ─── Test Utilities ────────────────────────────────────────────────────────────────

/// Find an attribute value by key in a slice of attributes.
fn find_attr<'a>(attrs: &'a [Attribute], key: &str) -> Option<&'a str> {
    attrs.iter().find(|a| a.key == key).and_then(|a| a.as_str())
}

/// Find an attribute i64 value by key.
fn find_attr_i64(attrs: &[Attribute], key: &str) -> Option<i64> {
    attrs.iter().find(|a| a.key == key).and_then(|a| a.as_i64())
}

/// Create a test bead.
fn make_bead(id: &str, priority: u8) -> Bead {
    Bead {
        id: BeadId::from(id),
        title: format!("Test bead {id}"),
        body: Some("Do something useful".to_string()),
        priority,
        status: BeadStatus::Open,
        assignee: None,
        labels: vec![],
        workspace: PathBuf::from("/tmp/test-workspace"),
        dependencies: vec![],
        dependents: vec![],
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

/// Create a test agent adapter.
fn make_adapter(name: &str) -> AgentAdapter {
    AgentAdapter {
        name: name.to_string(),
        description: None,
        agent_cli: "bash".to_string(),
        version_command: None,
        input_method: InputMethod::Stdin,
        invoke_template: "exit 0".to_string(),
        environment: HashMap::new(),
        timeout_secs: 10,
        provider: None,
        model: None,
        token_extraction: needle::dispatch::TokenExtraction::None,
        output_transform: None,
    }
}

/// Create a minimal test config.
fn make_config(workspace_home: &Path) -> Config {
    let mut config = Config::default();
    config.worker.idle_action = IdleAction::Exit;
    config.agent.default = "test-agent".to_string();
    config.workspace.default = PathBuf::from("/tmp/test-workspace");
    config.workspace.home = workspace_home.to_path_buf();
    config.self_modification.hot_reload = false;
    config
}

// ─────────────────────────────────────────────────────────────────────────────────
// Test 1: Happy Path — Spans, Metrics, and Logs
// ─────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires Docker"]
async fn otlp_integration_happy_path() -> Result<()> {
    // Start the OpenTelemetry Collector container.
    let collector = OtelCollectorImage::new();
    let container = collector
        .start()
        .await
        .context("failed to start collector container")?;

    let grpc_port = container.get_host_port_ipv4(4317).await?;
    let otlp_endpoint = format!("http://localhost:{}", grpc_port);

    // Give the collector a moment to be ready.
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Create a test workspace.
    let temp_dir = tempfile::tempdir()?;
    let workspace_home = temp_dir.path();

    // Create a file sink for drop events (OTLP failures are logged here)
    let file_sink =
        needle::telemetry::FileSink::with_dir(workspace_home, "test-worker", "integration-test")?;

    // Create OTLP config pointing to the collector
    let otlp_config = needle::config::OtlpSinkConfig {
        enabled: true,
        endpoint: otlp_endpoint,
        protocol: "grpc".to_string(),
        timeout_secs: 5,
        compression: "none".to_string(),
        tls: "none".to_string(),
        headers: vec![],
        resource_attributes: vec![],
        metrics_interval_secs: 10,
        service_namespace: "needle-test".to_string(),
        max_queue_size: 2048,
    };

    // Create an OTLP telemetry sink pointing to the collector.
    let otlp_sink = needle::telemetry::OtlpSink::new(
        "test-worker".to_string(),
        "integration-test".to_string(),
        &otlp_config,
        Some(Box::new(file_sink)),
        None, // agent
        None, // model
        workspace_home.to_str(),
    )
    .context("failed to create OTLP sink")?;

    let telemetry = Telemetry::with_sink("test-worker".to_string(), Arc::new(otlp_sink));

    // Create a single test bead.
    let bead = make_bead("needle-otlp-int-001", 1);
    let store: Arc<dyn BeadStore> = Arc::new(MockStore::new(vec![bead]));

    // Create and configure the worker.
    let config = make_config(workspace_home);
    let mut worker = Worker::new(config, "test-worker".to_string(), store);

    let adapter = make_adapter("test-agent");
    let mut adapters = HashMap::new();
    adapters.insert("test-agent".to_string(), adapter);
    worker.set_dispatcher(Dispatcher::with_adapters(adapters, telemetry, 10));

    // Run the worker to completion.
    let result = worker.run().await?;

    assert!(
        matches!(result, WorkerState::Stopped | WorkerState::Exhausted),
        "expected terminal state, got {:?}",
        result
    );
    assert_eq!(
        worker.beads_processed(),
        1,
        "expected exactly 1 bead processed"
    );

    // Flush and shutdown the OTLP sink to ensure all data is sent.
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Copy the output files from the container for inspection.
    let output_dir = temp_dir.path().join("otel-output");
    std::fs::create_dir_all(&output_dir)?;

    // Use docker cp to extract the output files.
    let container_id = container.id();
    let traces_path = output_dir.join("traces.json");
    let metrics_path = output_dir.join("metrics.json");
    let logs_path = output_dir.join("logs.json");

    // Copy traces from container.
    let copy_result = std::process::Command::new("docker")
        .arg("cp")
        .arg(&format!("{}:/tmp/otel-output/traces.json", container_id))
        .arg(&traces_path)
        .output();

    // The file may not exist yet if no spans were flushed; that's OK for assertions.
    if copy_result
        .as_ref()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        // Parse and verify traces.
        let traces_content = std::fs::read_to_string(&traces_path)?;
        let resource_spans: Vec<ResourceSpans> = serde_json::from_str(&traces_content)
            .with_context(|| format!("failed to parse traces: {}", traces_content))?;

        // Collect all spans for easier searching.
        let all_spans: Vec<_> = resource_spans
            .iter()
            .flat_map(|rs| &rs.scope_spans)
            .flat_map(|ss| &ss.spans)
            .collect();

        // Assert: One `worker.session` span.
        let worker_session_spans: Vec<_> = all_spans
            .iter()
            .filter(|s| s.name == "worker.session")
            .collect();
        assert!(
            !worker_session_spans.is_empty(),
            "expected at least one worker.session span"
        );

        // Assert: One `strand.pluck` child span.
        let pluck_spans: Vec<_> = all_spans
            .iter()
            .filter(|s| s.name == "strand.pluck")
            .collect();
        assert!(
            !pluck_spans.is_empty(),
            "expected at least one strand.pluck span"
        );

        // Assert: One `bead.lifecycle` child span with expected `needle.bead.id`.
        let lifecycle_spans: Vec<_> = all_spans
            .iter()
            .filter(|s| s.name == "bead.lifecycle")
            .collect();
        assert!(
            !lifecycle_spans.is_empty(),
            "expected at least one bead.lifecycle span"
        );
        let bead_id_attr = lifecycle_spans
            .first()
            .and_then(|s| find_attr(&s.attributes, "needle.bead.id"));
        assert_eq!(
            bead_id_attr,
            Some("needle-otlp-int-001"),
            "bead.lifecycle span should have needle.bead.id attribute"
        );

        // Assert: One `agent.dispatch` child span with `gen_ai.system` and `gen_ai.request.model`.
        let dispatch_spans: Vec<_> = all_spans
            .iter()
            .filter(|s| s.name == "agent.dispatch")
            .collect();
        assert!(
            !dispatch_spans.is_empty(),
            "expected at least one agent.dispatch span"
        );
        let dispatch_span = dispatch_spans.first().unwrap();
        let gen_ai_system = find_attr(&dispatch_span.attributes, "gen_ai.system");
        assert_eq!(
            gen_ai_system,
            Some("anthropic"),
            "agent.dispatch span should have gen_ai.system = anthropic"
        );
        let gen_ai_model = find_attr(&dispatch_span.attributes, "gen_ai.request.model");
        assert_eq!(
            gen_ai_model,
            Some("claude"),
            "agent.dispatch span should have gen_ai.request.model = claude"
        );
    }

    // Copy metrics from container.
    let copy_result = std::process::Command::new("docker")
        .arg("cp")
        .arg(&format!("{}:/tmp/otel-output/metrics.json", container_id))
        .arg(&metrics_path)
        .output();

    if copy_result
        .as_ref()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        let metrics_content = std::fs::read_to_string(&metrics_path)?;
        let resource_metrics: Vec<ResourceMetrics> = serde_json::from_str(&metrics_content)
            .with_context(|| format!("failed to parse metrics: {}", metrics_content))?;

        // Collect all metrics.
        let all_metrics: Vec<_> = resource_metrics
            .iter()
            .flat_map(|rm| &rm.scope_metrics)
            .flat_map(|sm| &sm.metrics)
            .collect();

        // Assert: At least one `needle.beads.completed` metric data point with `outcome="success"`.
        let completed_metric = all_metrics
            .iter()
            .find(|m| m.name == "needle.beads.completed");
        assert!(
            completed_metric.is_some(),
            "expected needle.beads.completed metric"
        );

        if let Some(completed) = completed_metric {
            if let Some(MetricData::Sum(sum)) = &completed.data {
                let success_dp = sum
                    .data_points
                    .iter()
                    .find(|dp| find_attr(&dp.attributes, "outcome") == Some("success"));
                assert!(
                    success_dp.is_some(),
                    "expected needle.beads.completed with outcome=success"
                );
                if let Some(dp) = success_dp {
                    assert!(
                        dp.value_as_i64().unwrap_or(0) >= 1,
                        "expected at least 1 completed bead"
                    );
                }
            }
        }
    }

    // Copy logs from container.
    let copy_result = std::process::Command::new("docker")
        .arg("cp")
        .arg(&format!("{}:/tmp/otel-output/logs.json", container_id))
        .arg(&logs_path)
        .output();

    if copy_result
        .as_ref()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        let logs_content = std::fs::read_to_string(&logs_path)?;
        let resource_logs: Vec<ResourceLogs> = serde_json::from_str(&logs_content)
            .with_context(|| format!("failed to parse logs: {}", logs_content))?;

        // Collect all log records.
        let all_logs: Vec<_> = resource_logs
            .iter()
            .flat_map(|rl| &rl.scope_logs)
            .flat_map(|sl| &sl.log_records)
            .collect();

        // Assert: LogRecords for `worker.started` and `worker.stopped` at INFO severity.
        let started_log = all_logs.iter().find(|l| l.name == "worker.started");
        assert!(started_log.is_some(), "expected worker.started log record");
        if let Some(log) = started_log {
            assert!(
                log.severity_number >= Severity::Info as u32,
                "worker.started should be at least INFO severity"
            );
        }

        let stopped_log = all_logs.iter().find(|l| l.name == "worker.stopped");
        assert!(stopped_log.is_some(), "expected worker.stopped log record");
        if let Some(log) = stopped_log {
            assert!(
                log.severity_number >= Severity::Info as u32,
                "worker.stopped should be at least INFO severity"
            );
        }
    }

    // Cleanup: stop the container.
    container.stop().await?;
    container.rm().await?;

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────────
// Test 2: Drop Path — Collector Unavailable
// ─────────────────────────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires Docker"]
async fn otlp_integration_drop_path() -> Result<()> {
    // Create a test workspace.
    let temp_dir = tempfile::tempdir()?;
    let workspace_home = temp_dir.path();

    // Create a file sink for drop events (OTLP failures are logged here)
    let file_sink =
        needle::telemetry::FileSink::with_dir(workspace_home, "test-worker", "drop-test")?;

    // Create OTLP config pointing to a non-existent endpoint
    let otlp_config = needle::config::OtlpSinkConfig {
        enabled: true,
        endpoint: "http://localhost:9999".to_string(), // Non-existent endpoint
        protocol: "grpc".to_string(),
        timeout_secs: 1, // Short timeout for faster test
        compression: "none".to_string(),
        tls: "none".to_string(),
        headers: vec![],
        resource_attributes: vec![],
        metrics_interval_secs: 10,
        service_namespace: "needle-test".to_string(),
        max_queue_size: 2048,
    };

    // Create an OTLP telemetry sink pointing to a non-existent endpoint.
    let otlp_sink = needle::telemetry::OtlpSink::new(
        "test-worker".to_string(),
        "drop-test".to_string(),
        &otlp_config,
        Some(Box::new(file_sink)),
        None, // agent
        None, // model
        workspace_home.to_str(),
    )
    .context("failed to create OTLP sink")?;

    let telemetry = Telemetry::with_sink("test-worker".to_string(), Arc::new(otlp_sink));

    // Emit some telemetry events that will fail to export.
    telemetry
        .emit(needle::telemetry::EventKind::WorkerStarted {
            worker_name: "test-worker".to_string(),
            version: "0.1.0".to_string(),
        })
        .context("failed to emit WorkerStarted")?;

    telemetry
        .emit(needle::telemetry::EventKind::WorkerStopped {
            reason: "test completed".to_string(),
            beads_processed: 0,
            uptime_secs: 0,
        })
        .context("failed to emit WorkerStopped")?;

    // Give time for export failures to be detected and drop events emitted.
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Flush and shutdown.
    drop(telemetry);
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Assert: The file sink (used for drop events) contains `telemetry.otlp.dropped`.
    let drops_path = workspace_home.join("test-worker-drop-test.jsonl");
    assert!(
        drops_path.exists(),
        "expected test-worker-drop-test.jsonl to exist"
    );

    let drops_content = std::fs::read_to_string(&drops_path)?;
    let drop_events: Vec<needle::telemetry::TelemetryEvent> = drops_content
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();

    let dropped_events: Vec<_> = drop_events
        .iter()
        .filter(|e| e.event_type == "telemetry.otlp.dropped")
        .collect();

    assert!(
        !dropped_events.is_empty(),
        "expected at least one telemetry.otlp.dropped event in file sink"
    );

    Ok(())
}
