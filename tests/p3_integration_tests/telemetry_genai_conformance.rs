//! Exported OpenTelemetry semantic-convention contract tests.
//!
//! These tests observe completed spans at the SDK exporter boundary. That is
//! important here: `tracing::Span::record` can appear to succeed while a field
//! that was not declared when the span was created is silently omitted by the
//! tracing/OpenTelemetry bridge.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use needle::dispatch::{AgentAdapter, Dispatcher, TokenExtraction};
use needle::prompt::BuiltPrompt;
use needle::telemetry::Telemetry;
use needle::types::{BeadId, InputMethod};
use opentelemetry::Value;
use opentelemetry_sdk::error::OTelSdkError;
use opentelemetry_sdk::resource::Resource;
use opentelemetry_sdk::trace::{SdkTracerProvider, SpanData, SpanExporter as SdkSpanExporter};
use tracing::Instrument;
use tracing_subscriber::layer::SubscriberExt;

const BEAD_ID: &str = "needle-7f53ca33-genai-probe";
const WORKER_ID: &str = "genai-conformance-worker";
const USAGE_JSON: &str = r#"{"usage":{"input_tokens":123,"output_tokens":456}}"#;

#[derive(Debug, Clone, Default)]
struct CapturingSpanExporter {
    spans: Arc<Mutex<Vec<SpanData>>>,
}

#[allow(refining_impl_trait_internal, refining_impl_trait_reachable)]
impl SdkSpanExporter for CapturingSpanExporter {
    fn set_resource(&mut self, _resource: &Resource) {}

    fn export(
        &self,
        batch: Vec<SpanData>,
    ) -> futures::future::BoxFuture<'static, Result<(), OTelSdkError>> {
        let spans = self.spans.clone();
        Box::pin(async move {
            spans
                .lock()
                .expect("captured span mutex poisoned")
                .extend(batch);
            Ok(())
        })
    }
}

fn stub_adapter(command: &str, provider: Option<&str>, model: Option<&str>) -> AgentAdapter {
    AgentAdapter {
        name: "genai-probe".to_string(),
        description: None,
        agent_cli: "bash".to_string(),
        version_command: None,
        input_method: InputMethod::Stdin,
        invoke_template: command.to_string(),
        environment: HashMap::new(),
        timeout_secs: 30,
        idle_timeout_secs: 0,
        hard_timeout_secs: 0,
        provider: provider.map(str::to_string),
        model: model.map(str::to_string),
        token_extraction: TokenExtraction::JsonField {
            input_path: "usage.input_tokens".to_string(),
            output_path: "usage.output_tokens".to_string(),
        },
        usage_format: None,
        output_transform: None,
        harness: None,
        harness_version: None,
    }
}

fn probe_prompt() -> BuiltPrompt {
    BuiltPrompt {
        content: "gen ai conformance probe".to_string(),
        hash: "genai-conformance-probe".to_string(),
        token_estimate: 2,
        template_name: "pluck".to_string(),
        template_version: "pluck-default".to_string(),
    }
}

fn dispatch_and_capture(adapter: AgentAdapter) -> Vec<SpanData> {
    let telemetry_dir = tempfile::tempdir().expect("telemetry tempdir");
    let workspace = tempfile::tempdir().expect("workspace tempdir");
    let telemetry = Telemetry::with_log_dir(WORKER_ID.to_string(), telemetry_dir.path());
    telemetry.start();

    let adapter_name = adapter.name.clone();
    let mut adapters = HashMap::new();
    adapters.insert(adapter_name.clone(), adapter);
    let dispatcher = Dispatcher::with_adapters(adapters, telemetry, 3600);
    let adapter = dispatcher
        .adapter(&adapter_name)
        .expect("stub adapter should be registered")
        .clone();

    let exporter = CapturingSpanExporter::default();
    let captured = exporter.spans.clone();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter)
        .build();
    let subscriber =
        tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(
            opentelemetry::trace::TracerProvider::tracer(&provider, "genai-conformance"),
        ));

    let prompt = probe_prompt();
    let bead_id = BeadId::from(BEAD_ID);
    let workspace_path = workspace.path().to_path_buf();
    tracing::subscriber::with_default(subscriber, || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        // The ordinary `dispatch` entry point intentionally refuses a
        // context-free spawn. Analysis dispatches are claim-free by design,
        // so provide the same contract span around that supported path and
        // observe the values at the exporter boundary.
        let dispatch_span = tracing::info_span!(
            "agent.dispatch",
            needle.bead.id = %bead_id.as_ref(),
            gen_ai.system = %adapter.gen_ai_system(),
            gen_ai.operation.name = "chat",
            gen_ai.request.id = %bead_id.as_ref(),
            gen_ai.request.model = tracing::field::Empty,
            gen_ai.usage.input_tokens = tracing::field::Empty,
            gen_ai.usage.output_tokens = tracing::field::Empty,
            needle.agent.pid = tracing::field::Empty,
            needle.agent.exit_code = tracing::field::Empty,
        );
        runtime.block_on(async {
            dispatcher
                .dispatch_unclaimed_analysis(&bead_id, &prompt, &adapter, &workspace_path)
                .instrument(dispatch_span)
                .await
                .expect("stub dispatch should succeed");
        });
    });

    provider.shutdown().expect("span provider should shut down");
    let spans = captured
        .lock()
        .expect("captured span mutex poisoned")
        .iter()
        .filter(|span| span.name == "agent.dispatch")
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(spans.len(), 1, "one dispatch should export one agent span");
    spans
}

fn attribute<'a>(span: &'a SpanData, key: &str) -> Option<&'a Value> {
    span.attributes
        .iter()
        .find(|attribute| attribute.key.as_str() == key)
        .map(|attribute| &attribute.value)
}

fn string_attribute(span: &SpanData, key: &str) -> Option<String> {
    attribute(span, key).map(|value| value.as_str().into_owned())
}

fn integer_attribute(span: &SpanData, key: &str) -> Option<i64> {
    match attribute(span, key) {
        Some(Value::I64(value)) => Some(*value),
        _ => None,
    }
}

#[test]
fn exported_agent_dispatch_span_conforms_to_gen_ai_contract() {
    let spans = dispatch_and_capture(stub_adapter(
        &format!("printf '%s' '{USAGE_JSON}'"),
        Some("acme"),
        Some("acme-test-model"),
    ));
    let span = &spans[0];

    for (key, expected) in [
        ("gen_ai.system", "acme"),
        ("gen_ai.operation.name", "chat"),
        ("gen_ai.request.id", BEAD_ID),
        ("gen_ai.request.model", "acme-test-model"),
        ("needle.bead.id", BEAD_ID),
    ] {
        assert_eq!(
            string_attribute(span, key),
            Some(expected.to_string()),
            "exported agent.dispatch must carry {key}={expected}"
        );
    }

    assert_eq!(
        integer_attribute(span, "gen_ai.usage.input_tokens"),
        Some(123),
        "exported input token usage must survive the exporter boundary"
    );
    assert_eq!(
        integer_attribute(span, "gen_ai.usage.output_tokens"),
        Some(456),
        "exported output token usage must survive the exporter boundary"
    );
    assert_eq!(
        integer_attribute(span, "needle.agent.exit_code"),
        Some(0),
        "exported agent.dispatch must carry the process exit code"
    );
    let pid = integer_attribute(span, "needle.agent.pid")
        .expect("exported agent.dispatch must carry the agent pid");
    assert!(
        pid > 0,
        "the recorded agent pid must be positive, got {pid}"
    );
}

#[test]
fn exported_agent_dispatch_uses_documented_placeholders_without_optional_identity() {
    let spans = dispatch_and_capture(stub_adapter("echo 'no usage reported'", None, None));
    let span = &spans[0];

    assert_eq!(
        string_attribute(span, "gen_ai.system"),
        Some("local".to_string())
    );
    assert_eq!(
        string_attribute(span, "gen_ai.request.model"),
        Some("unknown".to_string())
    );
    assert!(
        attribute(span, "gen_ai.usage.input_tokens").is_none()
            && attribute(span, "gen_ai.usage.output_tokens").is_none(),
        "usage attributes must be absent when the agent reports no usage"
    );
}
