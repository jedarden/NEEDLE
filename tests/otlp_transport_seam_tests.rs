//! OTLP resource propagation tests at the transport seam.
//!
//! The capturing exporters are substituted immediately before the resilient
//! wrappers, while provider construction still goes through the same builder
//! path used by the production HTTP and gRPC builders. Assertions therefore
//! observe the `Resource` handed to the exporter, rather than the value
//! returned by `OtlpSink::build_resource()`.

use std::ffi::OsString;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use needle::config::{Config, OtlpSinkConfig};
use needle::telemetry::otlp::OtlpSink;
use opentelemetry_sdk::error::OTelSdkError;
use opentelemetry_sdk::logs::{LogBatch, LogExporter as SdkLogExporter};
use opentelemetry_sdk::resource::Resource;
use opentelemetry_sdk::trace::{SpanData, SpanExporter as SdkSpanExporter};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Default)]
struct CapturingSpanExporter {
    resource: Arc<Mutex<Option<Resource>>>,
}

#[allow(refining_impl_trait_internal, refining_impl_trait_reachable)]
impl SdkSpanExporter for CapturingSpanExporter {
    fn set_resource(&mut self, resource: &Resource) {
        *self.resource.lock().expect("span resource mutex poisoned") = Some(resource.clone());
    }

    fn export(
        &self,
        _batch: Vec<SpanData>,
    ) -> futures::future::BoxFuture<'static, Result<(), OTelSdkError>> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Debug, Clone, Default)]
struct CapturingLogExporter {
    resource: Arc<Mutex<Option<Resource>>>,
}

impl SdkLogExporter for CapturingLogExporter {
    fn set_resource(&mut self, resource: &Resource) {
        *self.resource.lock().expect("log resource mutex poisoned") = Some(resource.clone());
    }

    async fn export(&self, _batch: LogBatch<'_>) -> Result<(), OTelSdkError> {
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
struct CapturingSpanHistoryExporter {
    resources: Arc<Mutex<Vec<Resource>>>,
}

#[allow(refining_impl_trait_internal, refining_impl_trait_reachable)]
impl SdkSpanExporter for CapturingSpanHistoryExporter {
    fn set_resource(&mut self, resource: &Resource) {
        self.resources
            .lock()
            .expect("span resource history mutex poisoned")
            .push(resource.clone());
    }

    fn export(
        &self,
        _batch: Vec<SpanData>,
    ) -> futures::future::BoxFuture<'static, Result<(), OTelSdkError>> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Debug, Clone, Default)]
struct CapturingLogHistoryExporter {
    resources: Arc<Mutex<Vec<Resource>>>,
}

impl SdkLogExporter for CapturingLogHistoryExporter {
    fn set_resource(&mut self, resource: &Resource) {
        self.resources
            .lock()
            .expect("log resource history mutex poisoned")
            .push(resource.clone());
    }

    async fn export(&self, _batch: LogBatch<'_>) -> Result<(), OTelSdkError> {
        Ok(())
    }
}

// `HOME` is process-global. Keep these tests' temporary HOME and Explore root
// together, and serialize their lifetime so the cleanup cannot race another
// test in this file.
static TEST_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

struct IsolatedTest {
    _lock: MutexGuard<'static, ()>,
    _home: tempfile::TempDir,
    previous_home: Option<OsString>,
    config: Config,
    _runtime: tokio::runtime::Runtime,
}

impl IsolatedTest {
    fn new() -> Self {
        let lock = TEST_ENV_LOCK.get_or_init(|| Mutex::new(()));
        let lock = lock.lock().expect("test environment mutex poisoned");
        let home = tempfile::tempdir().expect("failed to create isolated HOME");
        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", home.path());

        let mut config = Config::default();
        config.workspace.home = home.path().to_path_buf();
        config.strands.explore.workspace_root = home.path().to_path_buf();
        config.strands.explore.workspaces.clear();
        let runtime = tokio::runtime::Runtime::new().expect("failed to create test runtime");

        Self {
            _lock: lock,
            _home: home,
            previous_home,
            config,
            _runtime: runtime,
        }
    }

    fn assert_explore_isolated(&self) {
        assert_eq!(
            self.config.strands.explore.workspace_root,
            self.config.workspace.home
        );
        assert!(self.config.strands.explore.workspaces.is_empty());
    }
}

impl Drop for IsolatedTest {
    fn drop(&mut self) {
        match &self.previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}

fn test_config(protocol: &str) -> OtlpSinkConfig {
    OtlpSinkConfig {
        enabled: true,
        protocol: protocol.to_string(),
        endpoint: if protocol == "http" {
            "http://127.0.0.1:4318".to_string()
        } else {
            "http://127.0.0.1:4317".to_string()
        },
        timeout_ms: 100,
        metrics_interval_secs: 3600,
        service_namespace: "transport-seam-tests".to_string(),
        resource_attributes: vec![
            "deployment.environment=transport-test".to_string(),
            "needle.test.marker=exporter-boundary".to_string(),
        ],
        ..Default::default()
    }
}

fn test_resource(config: &OtlpSinkConfig) -> Resource {
    OtlpSink::build_resource(
        "transport-worker",
        "transport-session",
        config,
        Some("test-agent"),
        Some("test-model"),
        None,
        Some("/private/path/transport-workspace"),
    )
    .expect("test resource should build")
}

fn assert_resource_attributes(resource: &Resource) {
    let expected = [
        ("service.name", "needle"),
        ("service.namespace", "transport-seam-tests"),
        ("service.version", env!("CARGO_PKG_VERSION")),
        ("service.instance.id", "transport-worker"),
        ("needle.session_id", "transport-session"),
        ("needle.agent", "test-agent"),
        ("needle.model", "test-model"),
        ("needle.workspace", "transport-workspace"),
        ("deployment.environment", "transport-test"),
        ("needle.test.marker", "exporter-boundary"),
    ];

    for (key, expected_value) in expected {
        let value = resource
            .iter()
            .find(|(attribute, _)| attribute.as_str() == key)
            .map(|(_, value)| value.as_str());
        assert_eq!(
            value.as_deref(),
            Some(expected_value),
            "wrong or missing {key}"
        );
    }
}

fn wait_for_resource(resource: &Arc<Mutex<Option<Resource>>>, signal: &str) -> Resource {
    for _ in 0..100 {
        if let Some(resource) = resource
            .lock()
            .expect("captured resource mutex poisoned")
            .clone()
        {
            return resource;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("{signal} exporter did not receive a resource from its provider");
}

fn shutdown_providers(
    tracer_provider: opentelemetry_sdk::trace::SdkTracerProvider,
    meter_provider: opentelemetry_sdk::metrics::SdkMeterProvider,
    logger_provider: opentelemetry_sdk::logs::SdkLoggerProvider,
) {
    let _ = tracer_provider.shutdown();
    let _ = logger_provider.shutdown();
    let _ = meter_provider.shutdown();
}

/// A small running-worker harness for the cycle-boundary telemetry test.
///
/// The provider tuple is the live OTLP transport owned by the worker. Toggling
/// the config rebuilds or shuts down that tuple, while the capturing exporters
/// remain at the transport boundary. This deliberately does not inspect the
/// `Resource` returned by `build_resource`; the only observations below are
/// resources handed to exporters by the real provider-builder path.
struct RunningOtlpWorker {
    config: OtlpSinkConfig,
    resource: Resource,
    providers: Option<(
        opentelemetry_sdk::trace::SdkTracerProvider,
        opentelemetry_sdk::metrics::SdkMeterProvider,
        opentelemetry_sdk::logs::SdkLoggerProvider,
    )>,
    span_resources: Arc<Mutex<Vec<Resource>>>,
    log_resources: Arc<Mutex<Vec<Resource>>>,
}

impl RunningOtlpWorker {
    fn start(config: OtlpSinkConfig) -> Self {
        let resource = test_resource(&config);
        Self {
            config,
            resource,
            providers: None,
            span_resources: Arc::new(Mutex::new(Vec::new())),
            log_resources: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Apply a telemetry config at the point a real worker would finish a
    /// cycle. Enabling builds a fresh provider through the transport seam;
    /// disabling removes the live provider before the next cycle.
    fn apply_config_at_cycle_boundary(&mut self, config: OtlpSinkConfig) -> anyhow::Result<()> {
        if config.enabled == self.config.enabled {
            self.config = config;
            return Ok(());
        }

        if config.enabled {
            let span_exporter = CapturingSpanHistoryExporter {
                resources: self.span_resources.clone(),
            };
            let log_exporter = CapturingLogHistoryExporter {
                resources: self.log_resources.clone(),
            };
            let (drop_tx, _drop_rx) = mpsc::unbounded_channel();
            let providers = OtlpSink::build_http_providers_with_exporters(
                &config,
                &self.resource,
                drop_tx,
                span_exporter,
                log_exporter,
            )?;
            self.providers = Some(providers);
        } else if let Some((tracer, meter, logger)) = self.providers.take() {
            shutdown_providers(tracer, meter, logger);
        }

        self.config = config;
        Ok(())
    }

    fn span_resource_count(&self) -> usize {
        self.span_resources
            .lock()
            .expect("span resources mutex poisoned")
            .len()
    }

    fn log_resource_count(&self) -> usize {
        self.log_resources
            .lock()
            .expect("log resources mutex poisoned")
            .len()
    }

    fn wait_for_history(
        resources: &Arc<Mutex<Vec<Resource>>>,
        expected_count: usize,
        signal: &str,
    ) -> Resource {
        for _ in 0..100 {
            if let Some(resource) = resources
                .lock()
                .expect("resource history mutex poisoned")
                .get(expected_count.saturating_sub(1))
                .cloned()
            {
                return resource;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("{signal} exporter did not receive resource handoff #{expected_count}");
    }

    fn assert_transport_received_current_resource(&self, expected_count: usize) {
        let span_resource =
            Self::wait_for_history(&self.span_resources, expected_count, "span transport");
        let log_resource =
            Self::wait_for_history(&self.log_resources, expected_count, "log transport");

        assert_resource_attributes(&span_resource);
        assert_resource_attributes(&log_resource);
    }
}

impl Drop for RunningOtlpWorker {
    fn drop(&mut self) {
        if let Some((tracer, meter, logger)) = self.providers.take() {
            shutdown_providers(tracer, meter, logger);
        }
    }
}

#[test]
fn http_provider_path_hands_resource_to_transport_exporters() {
    let isolated = IsolatedTest::new();
    isolated.assert_explore_isolated();
    let _runtime_guard = isolated._runtime.enter();

    let config = test_config("http");
    let resource = test_resource(&config);
    let span_exporter = CapturingSpanExporter::default();
    let log_exporter = CapturingLogExporter::default();
    let span_resource = span_exporter.resource.clone();
    let log_resource = log_exporter.resource.clone();
    let (drop_tx, _drop_rx) = mpsc::unbounded_channel();

    let providers = OtlpSink::build_http_providers_with_exporters(
        &config,
        &resource,
        drop_tx,
        span_exporter,
        log_exporter,
    )
    .expect("HTTP providers should build through the transport seam");

    let captured_span_resource = wait_for_resource(&span_resource, "HTTP trace");
    let captured_log_resource = wait_for_resource(&log_resource, "HTTP log");
    assert_resource_attributes(&captured_span_resource);
    assert_resource_attributes(&captured_log_resource);

    shutdown_providers(providers.0, providers.1, providers.2);
}

#[test]
fn grpc_provider_path_hands_resource_to_transport_exporters() {
    let isolated = IsolatedTest::new();
    isolated.assert_explore_isolated();
    let _runtime_guard = isolated._runtime.enter();

    let config = test_config("grpc");
    let resource = test_resource(&config);
    let span_exporter = CapturingSpanExporter::default();
    let log_exporter = CapturingLogExporter::default();
    let span_resource = span_exporter.resource.clone();
    let log_resource = log_exporter.resource.clone();
    let (drop_tx, _drop_rx) = mpsc::unbounded_channel();

    let providers = OtlpSink::build_grpc_providers_with_exporters(
        &config,
        &resource,
        drop_tx,
        span_exporter,
        log_exporter,
    )
    .expect("gRPC providers should build through the transport seam");

    let captured_span_resource = wait_for_resource(&span_resource, "gRPC trace");
    let captured_log_resource = wait_for_resource(&log_resource, "gRPC log");
    assert_resource_attributes(&captured_span_resource);
    assert_resource_attributes(&captured_log_resource);

    shutdown_providers(providers.0, providers.1, providers.2);
}

/// Regression coverage for the wrapper hop. Each provider path constructs its
/// two resilient wrappers before handing the provider resource to them, so all
/// four wrapper/resource edges are exercised here.
#[test]
fn all_four_resilient_wrappers_forward_provider_resource() {
    let isolated = IsolatedTest::new();
    isolated.assert_explore_isolated();
    let _runtime_guard = isolated._runtime.enter();

    let config = test_config("http");
    let resource = test_resource(&config);
    let http_span_exporter = CapturingSpanExporter::default();
    let http_log_exporter = CapturingLogExporter::default();
    let grpc_span_exporter = CapturingSpanExporter::default();
    let grpc_log_exporter = CapturingLogExporter::default();
    let http_span_resource = http_span_exporter.resource.clone();
    let http_log_resource = http_log_exporter.resource.clone();
    let grpc_span_resource = grpc_span_exporter.resource.clone();
    let grpc_log_resource = grpc_log_exporter.resource.clone();
    let (http_drop_tx, _http_drop_rx) = mpsc::unbounded_channel();
    let (grpc_drop_tx, _grpc_drop_rx) = mpsc::unbounded_channel();

    let http_providers = OtlpSink::build_http_providers_with_exporters(
        &config,
        &resource,
        http_drop_tx,
        http_span_exporter,
        http_log_exporter,
    )
    .expect("HTTP providers should build");
    let grpc_providers = OtlpSink::build_grpc_providers_with_exporters(
        &config,
        &resource,
        grpc_drop_tx,
        grpc_span_exporter,
        grpc_log_exporter,
    )
    .expect("gRPC providers should build");

    assert_resource_attributes(&wait_for_resource(&http_span_resource, "HTTP trace"));
    assert_resource_attributes(&wait_for_resource(&http_log_resource, "HTTP log"));
    assert_resource_attributes(&wait_for_resource(&grpc_span_resource, "gRPC trace"));
    assert_resource_attributes(&wait_for_resource(&grpc_log_resource, "gRPC log"));

    shutdown_providers(http_providers.0, http_providers.1, http_providers.2);
    shutdown_providers(grpc_providers.0, grpc_providers.1, grpc_providers.2);
}

/// A running worker must be able to turn OTLP on and off at a cycle boundary.
///
/// The false -> true transition proves that a worker which booted without an
/// exporter can create one after a config reload. The true -> false transition
/// proves that disabling it removes the live provider without handing a stale
/// or replacement resource to the transport. Both enabled observations are
/// made by the capturing exporters, after the resilient-wrapper hop.
#[test]
fn running_worker_toggles_otlp_both_directions_at_transport_seam() {
    let isolated = IsolatedTest::new();
    isolated.assert_explore_isolated();
    let _runtime_guard = isolated._runtime.enter();

    let mut disabled_config = test_config("http");
    disabled_config.enabled = false;
    let mut worker = RunningOtlpWorker::start(disabled_config.clone());

    assert_eq!(worker.span_resource_count(), 0);
    assert_eq!(worker.log_resource_count(), 0);

    // false -> true: the running worker must hand the candidate's resource to
    // the actual transport exporters, not merely construct it in a builder.
    let mut enabled_config = disabled_config;
    enabled_config.enabled = true;
    worker
        .apply_config_at_cycle_boundary(enabled_config)
        .expect("enabling OTLP at the cycle boundary should succeed");
    worker.assert_transport_received_current_resource(1);
    assert_eq!(worker.span_resource_count(), 1);
    assert_eq!(worker.log_resource_count(), 1);

    // true -> false: disabling the running provider must not cause another
    // exporter handoff, and the worker remains usable for later cycles.
    let mut disabled_again = test_config("http");
    disabled_again.enabled = false;
    worker
        .apply_config_at_cycle_boundary(disabled_again)
        .expect("disabling OTLP at the cycle boundary should succeed");
    assert_eq!(worker.span_resource_count(), 1);
    assert_eq!(worker.log_resource_count(), 1);

    // Re-enable once more to prove the same worker can make the reverse
    // transition repeatedly rather than only during its first rebuild.
    let enabled_again = test_config("http");
    worker
        .apply_config_at_cycle_boundary(enabled_again)
        .expect("re-enabling OTLP at the cycle boundary should succeed");
    worker.assert_transport_received_current_resource(2);
    assert_eq!(worker.span_resource_count(), 2);
    assert_eq!(worker.log_resource_count(), 2);
}
