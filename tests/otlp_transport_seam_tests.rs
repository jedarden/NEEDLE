//! Transport-seam OTLP attribute tests.
//!
//! These tests verify that OTLP resource attributes are correctly propagated
//! through the resilient exporter wrappers to the actual transport layer.
//!
//! Unlike the unit tests in src/telemetry/otlp.rs which assert on
//! OtlpSink::build_resource() return values (which were always correct),
//! these tests assert on what the EXPORTER is actually handed - testing the
//! full path from build_resource() through the wrapper hop to the transport.
//!
//! This catches the class of bug where build_resource() is correct but the
//! resilient wrappers fail to forward the resource to the inner exporter.

use std::sync::{Arc, Mutex};
use needle::config::OtlpSinkConfig;
use opentelemetry::KeyValue;
use opentelemetry_sdk::logs::{
    LogBatch, LogExporter as SdkLogExporter, SdkLoggerProvider,
};
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::resource::Resource;
use opentelemetry_sdk::trace::{
    BatchSpanProcessor, SpanData, SpanExporter as SdkSpanExporter, SdkTracerProvider,
};
use opentelemetry_sdk::error::OTelSdkError;
use std::time::Duration;
use tokio::sync::mpsc;

// ─────────────────────────────────────────────────────────────────────────────────
// Capturing Exporters
// ─────────────────────────────────────────────────────────────────────────────────

/// Span exporter that captures the resource set on it.
#[derive(Debug, Clone, Default)]
struct CapturingSpanExporter {
    resource: Arc<Mutex<Option<Resource>>>,
}

impl SdkSpanExporter for CapturingSpanExporter {
    fn set_resource(&mut self, resource: &Resource) {
        *self.resource.lock().unwrap() = Some(resource.clone());
    }

    fn export(
        &self,
        _batch: Vec<SpanData>,
    ) -> futures::future::BoxFuture<'static, Result<(), OTelSdkError>> {
        Box::pin(async { Ok(()) })
    }
}

/// Log exporter that captures the resource set on it.
#[derive(Debug, Clone, Default)]
struct CapturingLogExporter {
    resource: Arc<Mutex<Option<Resource>>>,
}

impl SdkLogExporter for CapturingLogExporter {
    fn set_resource(&mut self, resource: &Resource) {
        *self.resource.lock().unwrap() = Some(resource.clone());
    }

    fn export(
        &self,
        _batch: LogBatch<'_>,
    ) -> impl std::future::Future<Output = Result<(), OTelSdkError>> + Send {
        async move { Ok(()) }
    }
}

// ─────────────────────────────────────────────────────────────────────────────────
// gRPC Wrapper Tests
// ─────────────────────────────────────────────────────────────────────────────────

#[test]
fn test_grpc_span_exporter_forwards_resource_to_inner() {
    // Create a capturing exporter to record what resource is set
    let capturing = CapturingSpanExporter::default();
    let resource_clone = capturing.resource.clone();

    // Wrap it in a ResilientGrpcSpanExporter (simulating build_grpc_providers)
    let (drop_tx, _drop_rx) = mpsc::unbounded_channel();
    let wrapper = needle::telemetry::otlp::ResilientGrpcSpanExporter::new(
        capturing,
        drop_tx,
    );

    // Set a resource via the wrapper
    let test_resource = Resource::builder()
        .with_attributes([
            KeyValue::new("service.name", "needle"),
            KeyValue::new("service.instance.id", "test-worker"),
            KeyValue::new("needle.session_id", "test-session"),
            KeyValue::new("needle.agent", "claude-anthropic-sonnet"),
        ])
        .build();

    // This must forward to the inner exporter
    wrapper.set_resource(&test_resource);

    // Verify the inner exporter received the resource
    let received = resource_clone.lock().unwrap();
    let received = received.as_ref().expect("inner exporter should have resource set");

    // Verify key attributes made it through
    let attrs: Vec<_> = received.iter().map(|(k, _)| k.as_str().to_string()).collect();
    assert!(attrs.contains(&"service.name".to_string()), "missing service.name");
    assert!(attrs.contains(&"service.instance.id".to_string()), "missing service.instance.id");
    assert!(attrs.contains(&"needle.session_id".to_string()), "missing needle.session_id");
    assert!(attrs.contains(&"needle.agent".to_string()), "missing needle.agent");

    // Verify actual values match
    let service_name = received
        .iter()
        .find(|(k, _)| k.as_str() == "service.name")
        .map(|(_, v)| v.as_str());
    assert_eq!(service_name, Some("needle"), "service.name should match");
}

#[test]
fn test_grpc_log_exporter_forwards_resource_to_inner() {
    let capturing = CapturingLogExporter::default();
    let resource_clone = capturing.resource.clone();

    let (drop_tx, _drop_rx) = mpsc::unbounded_channel();
    let wrapper = needle::telemetry::otlp::ResilientGrpcLogExporter::new(
        capturing,
        drop_tx,
    );

    let test_resource = Resource::builder()
        .with_attributes([
            KeyValue::new("service.name", "needle"),
            KeyValue::new("service.namespace", "test-namespace"),
            KeyValue::new("needle.model", "claude-sonnet-4-6"),
            KeyValue::new("needle.workspace", "test-workspace"),
        ])
        .build();

    wrapper.set_resource(&test_resource);

    let received = resource_clone.lock().unwrap();
    let received = received.as_ref().expect("inner exporter should have resource set");

    let attrs: Vec<_> = received.iter().map(|(k, _)| k.as_str().to_string()).collect();
    assert!(attrs.contains(&"service.namespace".to_string()), "missing service.namespace");
    assert!(attrs.contains(&"needle.model".to_string()), "missing needle.model");
    assert!(attrs.contains(&"needle.workspace".to_string()), "missing needle.workspace");
}

// ─────────────────────────────────────────────────────────────────────────────────
// HTTP Wrapper Tests
// ─────────────────────────────────────────────────────────────────────────────────

#[test]
fn test_http_span_exporter_forwards_resource_to_inner() {
    let capturing = CapturingSpanExporter::default();
    let resource_clone = capturing.resource.clone();

    let (drop_tx, _drop_rx) = mpsc::unbounded_channel();
    let wrapper = needle::telemetry::otlp::ResilientHttpSpanExporter::new(
        capturing,
        drop_tx,
    );

    let test_resource = Resource::builder()
        .with_attributes([
            KeyValue::new("service.name", "needle"),
            KeyValue::new("service.version", "0.1.0"),
            KeyValue::new("host.name", "test-host"),
            KeyValue::new("process.pid", "12345"),
        ])
        .build();

    wrapper.set_resource(&test_resource);

    let received = resource_clone.lock().unwrap();
    let received = received.as_ref().expect("inner exporter should have resource set");

    let attrs: Vec<_> = received.iter().map(|(k, _)| k.as_str().to_string()).collect();
    assert!(attrs.contains(&"service.version".to_string()), "missing service.version");
    assert!(attrs.contains(&"host.name".to_string()), "missing host.name");
    assert!(attrs.contains(&"process.pid".to_string()), "missing process.pid");
}

#[test]
fn test_http_log_exporter_forwards_resource_to_inner() {
    let capturing = CapturingLogExporter::default();
    let resource_clone = capturing.resource.clone();

    let (drop_tx, _drop_rx) = mpsc::unbounded_channel();
    let wrapper = needle::telemetry::otlp::ResilientHttpLogExporter::new(
        capturing,
        drop_tx,
    );

    let test_resource = Resource::builder()
        .with_attributes([
            KeyValue::new("service.name", "needle"),
            KeyValue::new("deployment.environment", "production"),
            KeyValue::new("custom.attribute", "custom-value"),
        ])
        .build();

    wrapper.set_resource(&test_resource);

    let received = resource_clone.lock().unwrap();
    let received = received.as_ref().expect("inner exporter should have resource set");

    let attrs: Vec<_> = received.iter().map(|(k, _)| k.as_str().to_string()).collect();
    assert!(attrs.contains(&"deployment.environment".to_string()), "missing deployment.environment");
    assert!(attrs.contains(&"custom.attribute".to_string()), "missing custom.attribute");

    let custom_attr = received
        .iter()
        .find(|(k, _)| k.as_str() == "custom.attribute")
        .map(|(_, v)| v.as_str());
    assert_eq!(custom_attr, Some("custom-value"), "custom.attribute should match");
}

// ─────────────────────────────────────────────────────────────────────────────────
// Full Pipeline Tests
// ─────────────────────────────────────────────────────────────────────────────────

/// Test that the full gRPC provider pipeline correctly propagates resources.
///
/// This test simulates what happens in build_grpc_providers() and verifies
/// that the resource makes it through the BatchSpanProcessor → ResilientGrpcSpanExporter → inner exporter path.
#[test]
fn test_grpc_span_pipeline_propagates_resource() {
    let capturing = CapturingSpanExporter::default();
    let resource_clone = capturing.resource.clone();

    let (drop_tx, _drop_rx) = mpsc::unbounded_channel();
    let resilient = needle::telemetry::otlp::ResilientGrpcSpanExporter::new(
        capturing.clone(),
        drop_tx,
    );

    // Build a BatchSpanProcessor (as done in build_grpc_providers)
    let batch_processor = BatchSpanProcessor::builder(resilient).build();

    // Build a SdkTracerProvider with the resource (as done in build_grpc_providers)
    let test_resource = Resource::builder()
        .with_attributes([
            KeyValue::new("service.name", "needle"),
            KeyValue::new("service.instance.id", "test-worker-grpc-pipeline"),
            KeyValue::new("needle.session_id", "pipeline-test-session"),
        ])
        .build();

    let _tracer_provider = SdkTracerProvider::builder()
        .with_span_processor(batch_processor)
        .with_resource(test_resource.clone())
        .build();

    // The resource should have been propagated through the pipeline
    // Give it a moment to process
    std::thread::sleep(Duration::from_millis(100));

    let received = resource_clone.lock().unwrap();
    let received = received.as_ref().expect("resource should propagate through pipeline");

    // Verify the resource made it through
    let instance_id = received
        .iter()
        .find(|(k, _)| k.as_str() == "service.instance.id")
        .map(|(_, v)| v.as_str());
    assert_eq!(
        instance_id,
        Some("test-worker-grpc-pipeline"),
        "service.instance.id should propagate through pipeline"
    );
}

/// Test that the full HTTP provider pipeline correctly propagates resources for logs.
#[test]
fn test_http_log_pipeline_propagates_resource() {
    let capturing = CapturingLogExporter::default();
    let resource_clone = capturing.resource.clone();

    let (drop_tx, _drop_rx) = mpsc::unbounded_channel();
    let resilient = needle::telemetry::otlp::ResilientHttpLogExporter::new(
        capturing.clone(),
        drop_tx,
    );

    // Build a BatchLogProcessor (as done in build_http_providers)
    use opentelemetry_sdk::logs::BatchLogProcessor;
    let batch_processor = BatchLogProcessor::builder(resilient).build();

    // Build a SdkLoggerProvider with the resource
    let test_resource = Resource::builder()
        .with_attributes([
            KeyValue::new("service.name", "needle"),
            KeyValue::new("service.namespace", "http-pipeline-test"),
            KeyValue::new("needle.agent", "test-agent"),
        ])
        .build();

    let _logger_provider = SdkLoggerProvider::builder()
        .with_log_processor(batch_processor)
        .with_resource(test_resource.clone())
        .build();

    // The resource should have been propagated
    std::thread::sleep(Duration::from_millis(100));

    let received = resource_clone.lock().unwrap();
    let received = received.as_ref().expect("resource should propagate through log pipeline");

    let namespace = received
        .iter()
        .find(|(k, _)| k.as_str() == "service.namespace")
        .map(|(_, v)| v.as_str());
    assert_eq!(
        namespace,
        Some("http-pipeline-test"),
        "service.namespace should propagate through HTTP log pipeline"
    );
}

// ─────────────────────────────────────────────────────────────────────────────────
// Regression Test: All Four Wrappers Forward Resources
// ─────────────────────────────────────────────────────────────────────────────────

/// Regression test that all four resilient wrappers forward set_resource.
///
/// This test must fail if any wrapper stops forwarding the resource to its inner exporter.
/// It covers all four wrappers: gRPC span, gRPC log, HTTP span, HTTP log.
#[test]
fn test_all_four_wrappers_forward_set_resource() {
    let test_resource = Resource::builder()
        .with_attributes([
            KeyValue::new("regression.test", "all-wrappers"),
            KeyValue::new("service.instance.id", "regression-test"),
        ])
        .build();

    // Test ResilientGrpcSpanExporter
    {
        let capturing = CapturingSpanExporter::default();
        let resource_clone = capturing.resource.clone();
        let (drop_tx, _drop_rx) = mpsc::unbounded_channel();
        let wrapper = needle::telemetry::otlp::ResilientGrpcSpanExporter::new(
            capturing,
            drop_tx,
        );
        wrapper.set_resource(&test_resource);
        let received = resource_clone.lock().unwrap();
        assert!(
            received.is_some(),
            "ResilientGrpcSpanExporter must forward resource"
        );
    }

    // Test ResilientGrpcLogExporter
    {
        let capturing = CapturingLogExporter::default();
        let resource_clone = capturing.resource.clone();
        let (drop_tx, _drop_rx) = mpsc::unbounded_channel();
        let wrapper = needle::telemetry::otlp::ResilientGrpcLogExporter::new(
            capturing,
            drop_tx,
        );
        wrapper.set_resource(&test_resource);
        let received = resource_clone.lock().unwrap();
        assert!(
            received.is_some(),
            "ResilientGrpcLogExporter must forward resource"
        );
    }

    // Test ResilientHttpSpanExporter
    {
        let capturing = CapturingSpanExporter::default();
        let resource_clone = capturing.resource.clone();
        let (drop_tx, _drop_rx) = mpsc::unbounded_channel();
        let wrapper = needle::telemetry::otlp::ResilientHttpSpanExporter::new(
            capturing,
            drop_tx,
        );
        wrapper.set_resource(&test_resource);
        let received = resource_clone.lock().unwrap();
        assert!(
            received.is_some(),
            "ResilientHttpSpanExporter must forward resource"
        );
    }

    // Test ResilientHttpLogExporter
    {
        let capturing = CapturingLogExporter::default();
        let resource_clone = capturing.resource.clone();
        let (drop_tx, _drop_rx) = mpsc::unbounded_channel();
        let wrapper = needle::telemetry::otlp::ResilientHttpLogExporter::new(
            capturing,
            drop_tx,
        );
        wrapper.set_resource(&test_resource);
        let received = resource_clone.lock().unwrap();
        assert!(
            received.is_some(),
            "ResilientHttpLogExporter must forward resource"
        );
    }
}
