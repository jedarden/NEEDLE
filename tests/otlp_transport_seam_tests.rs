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

// `HOME` is process-global. Keep these tests' temporary HOME and Explore root
// together, and serialize their lifetime so the cleanup cannot race another
// test in this file.
static TEST_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

struct IsolatedTest {
    _lock: MutexGuard<'static, ()>,
    _home: tempfile::TempDir,
    previous_home: Option<OsString>,
    config: Config,
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

        Self {
            _lock: lock,
            _home: home,
            previous_home,
            config,
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

#[test]
fn http_provider_path_hands_resource_to_transport_exporters() {
    let isolated = IsolatedTest::new();
    isolated.assert_explore_isolated();

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
