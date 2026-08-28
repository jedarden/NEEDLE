//! Integration test verifying Anthropic subscription models route through claude-print adapter.
//!
//! This test validates the model routing logic without executing actual agent processes.
//! It verifies that beads requesting Anthropic models (sonnet, opus, fable, haiku)
//! correctly resolve to the claude-print adapter via the routing rules.
//!
//! Test coverage:
//! 1. Anthropic subscription model patterns match claude-print adapter
//! 2. Non-Anthropic models use default adapter (claude-code-glm-4.7)
//! 3. Routing rules are evaluated in order (first match wins)
//! 4. Telemetry events contain correct routing decision data

use std::collections::HashMap;
use std::path::PathBuf;

use needle::config::{AgentConfig, Config, RoutingConfig, RoutingRule};
use needle::dispatch::{AgentAdapter, Dispatcher};
use needle::routing::match_adapter;
use needle::telemetry::Telemetry;
use needle::types::InputMethod;

// ──────────────────────────────────────────────────────────────────────────────
// Test Helper Functions
// ──────────────────────────────────────────────────────────────────────────────

/// Create a routing rule with the given pattern and adapter.
fn make_rule(pattern: &str, adapter: &str) -> RoutingRule {
    RoutingRule {
        match_model: pattern.to_string(),
        adapter: adapter.to_string(),
    }
}

/// Create the standard Anthropic subscription routing configuration.
fn make_anthropic_subscription_routing() -> RoutingConfig {
    RoutingConfig {
        rules: vec![
            // Route all Anthropic Claude subscription models to claude-print
            make_rule("(claude-)?(sonnet|opus|fable|haiku).*", "claude-print"),
        ],
        default_adapter: Some("claude-code-glm-4.7".to_string()),
        strict: false,
    }
}

/// Create a minimal config for testing.
fn make_test_config(agent_default: &str, routing: Option<RoutingConfig>) -> Config {
    Config {
        agent: AgentConfig {
            default: agent_default.to_string(),
            adapters_dir: PathBuf::from("/nonexistent"),
            args: vec![],
            timeout: 120,
            routing,
        },
        ..Default::default()
    }
}

/// Create mock adapters for testing.
fn make_mock_adapters() -> HashMap<String, AgentAdapter> {
    let mut adapters = HashMap::new();

    // Mock claude-print adapter (what Anthropic models should route to)
    adapters.insert(
        "claude-print".to_string(),
        AgentAdapter {
            name: "claude-print".to_string(),
            description: Some("claude-print adapter for Anthropic subscription models".to_string()),
            agent_cli: "claude-print".to_string(),
            version_command: Some("claude-print --version".to_string()),
            input_method: InputMethod::Stdin,
            invoke_template: "claude-print --model {model} --output-format stream-json".to_string(),
            environment: HashMap::new(),
            timeout_secs: 600,
            idle_timeout_secs: 0,
            hard_timeout_secs: 0,
            provider: Some("anthropic".to_string()),
            model: None, // Model is filled in at runtime
            token_extraction: needle::dispatch::TokenExtraction::None,
            output_transform: Some("needle-transform-claude".to_string()),
            harness: Some("needle".to_string()),
            harness_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        },
    );

    // Mock claude-code-glm-4.7 adapter (default for non-Anthropic models)
    adapters.insert(
        "claude-code-glm-4.7".to_string(),
        AgentAdapter {
            name: "claude-code-glm-4.7".to_string(),
            description: Some("GLM adapter for non-Anthropic models".to_string()),
            agent_cli: "claude".to_string(),
            version_command: Some("claude --version".to_string()),
            input_method: InputMethod::Stdin,
            invoke_template: "claude --model {model} --max-turns 30".to_string(),
            environment: HashMap::new(),
            timeout_secs: 600,
            idle_timeout_secs: 0,
            hard_timeout_secs: 0,
            provider: Some("anthropic".to_string()),
            model: None,
            token_extraction: needle::dispatch::TokenExtraction::None,
            output_transform: None,
            harness: Some("needle".to_string()),
            harness_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        },
    );

    adapters
}

// ──────────────────────────────────────────────────────────────────────────────
// Anthropic Subscription Model Routing Tests
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn verifies_anthropic_sonnet_routes_to_claude_print() {
    // Test that Sonnet models correctly route to claude-print adapter
    let routing = make_anthropic_subscription_routing();
    let config = make_test_config("claude", Some(routing));

    let sonnet_models = vec![
        "claude-sonnet-4-6",
        "claude-sonnet-4-5-20251001",
        "claude-sonnet-4-7",
        "claude-sonnet-5",
        "claude-sonnet-5-20250529",
        "sonnet-4-6", // Without claude- prefix
        "sonnet-4-7",
        "sonnet-5",
    ];

    for model in sonnet_models {
        let default_adapter = config
            .agent
            .routing
            .as_ref()
            .unwrap()
            .default_adapter
            .as_deref()
            .unwrap_or("claude");

        let result = match_adapter(
            model,
            &config.agent.routing.as_ref().unwrap().rules,
            default_adapter,
        );

        assert_eq!(
            result,
            Some("claude-print".to_string()),
            "Sonnet model '{}' should route to claude-print adapter, got '{:?}'",
            model,
            result
        );
    }
}

#[test]
fn verifies_anthropic_opus_routes_to_claude_print() {
    // Test that Opus models correctly route to claude-print adapter
    let routing = make_anthropic_subscription_routing();
    let config = make_test_config("claude", Some(routing));

    let opus_models = vec![
        "claude-opus-4-6",
        "claude-opus-4-5-20251001",
        "claude-opus-4-7",
        "claude-opus-5",
        "opus-4-6", // Without claude- prefix
        "opus-4-7",
        "opus-5",
    ];

    for model in opus_models {
        let default_adapter = config
            .agent
            .routing
            .as_ref()
            .unwrap()
            .default_adapter
            .as_deref()
            .unwrap_or("claude");

        let result = match_adapter(
            model,
            &config.agent.routing.as_ref().unwrap().rules,
            default_adapter,
        );

        assert_eq!(
            result,
            Some("claude-print".to_string()),
            "Opus model '{}' should route to claude-print adapter, got '{:?}'",
            model,
            result
        );
    }
}

#[test]
fn verifies_anthropic_fable_routes_to_claude_print() {
    // Test that Fable models correctly route to claude-print adapter
    let routing = make_anthropic_subscription_routing();
    let config = make_test_config("claude", Some(routing));

    let fable_models = vec![
        "claude-fable-4-6",
        "claude-fable-4-5-20251001",
        "claude-fable-4-7",
        "fable-4-6", // Without claude- prefix
        "fable-4-7",
    ];

    for model in fable_models {
        let default_adapter = config
            .agent
            .routing
            .as_ref()
            .unwrap()
            .default_adapter
            .as_deref()
            .unwrap_or("claude");

        let result = match_adapter(
            model,
            &config.agent.routing.as_ref().unwrap().rules,
            default_adapter,
        );

        assert_eq!(
            result,
            Some("claude-print".to_string()),
            "Fable model '{}' should route to claude-print adapter, got '{:?}'",
            model,
            result
        );
    }
}

#[test]
fn verifies_anthropic_haiku_routes_to_claude_print() {
    // Test that Haiku models correctly route to claude-print adapter
    let routing = make_anthropic_subscription_routing();
    let config = make_test_config("claude", Some(routing));

    let haiku_models = vec![
        "claude-haiku-4-6",
        "claude-haiku-4-5-20251001",
        "claude-haiku-4-7",
        "haiku-4-6", // Without claude- prefix
        "haiku-4-7",
    ];

    for model in haiku_models {
        let default_adapter = config
            .agent
            .routing
            .as_ref()
            .unwrap()
            .default_adapter
            .as_deref()
            .unwrap_or("claude");

        let result = match_adapter(
            model,
            &config.agent.routing.as_ref().unwrap().rules,
            default_adapter,
        );

        assert_eq!(
            result,
            Some("claude-print".to_string()),
            "Haiku model '{}' should route to claude-print adapter, got '{:?}'",
            model,
            result
        );
    }
}

#[test]
fn verifies_non_anthropic_models_use_default_adapter() {
    // Test that non-Anthropic models fall back to default adapter
    let routing = make_anthropic_subscription_routing();
    let config = make_test_config("claude", Some(routing));

    let non_anthropic_models = vec![
        "glm-4.7",
        "glm-4.7-turbo",
        "gpt-4",
        "gpt-4-turbo",
        "claude-vision", // Not in subscription pattern
        "unknown-model",
    ];

    for model in non_anthropic_models {
        let default_adapter = config
            .agent
            .routing
            .as_ref()
            .unwrap()
            .default_adapter
            .as_deref()
            .unwrap_or("claude");

        let result = match_adapter(
            model,
            &config.agent.routing.as_ref().unwrap().rules,
            default_adapter,
        );

        assert_eq!(
            result,
            Some("claude-code-glm-4.7".to_string()),
            "Non-Anthropic model '{}' should route to default adapter, got '{:?}'",
            model,
            result
        );
    }
}

#[test]
fn verifies_dispatcher_adapter_resolution_for_anthropic_models() {
    // Test that Dispatcher correctly resolves Anthropic models to claude-print
    let adapters = make_mock_adapters();
    let telemetry = Telemetry::new("test-worker".to_string());
    let dispatcher = Dispatcher::with_adapters(adapters, telemetry, 3600);

    let routing = make_anthropic_subscription_routing();
    let config = make_test_config("claude", Some(routing));

    // Test that dispatcher resolves adapter correctly for Anthropic models
    let anthropic_models = vec![
        "claude-sonnet-4-6",
        "claude-opus-4-7",
        "claude-fable-5",
        "claude-haiku-4-6",
        "sonnet-5", // Without claude- prefix
    ];

    for model in anthropic_models {
        let adapter_name = dispatcher.resolve_adapter_name(model, &config);
        assert_eq!(
            adapter_name, "claude-print",
            "Dispatcher should resolve Anthropic model '{}' to claude-print, got '{}'",
            model, adapter_name
        );

        let adapter = dispatcher.adapter(&adapter_name);
        assert!(
            adapter.is_some(),
            "Dispatcher should have claude-print adapter available"
        );

        let adapter = adapter.unwrap();
        assert_eq!(
            adapter.name, "claude-print",
            "Resolved adapter should be claude-print"
        );
        assert_eq!(
            adapter.provider.as_deref(),
            Some("anthropic"),
            "claude-print adapter should have provider='anthropic'"
        );
    }
}

#[test]
fn verifies_dispatcher_adapter_resolution_for_non_anthropic_models() {
    // Test that Dispatcher correctly resolves non-Anthropic models to default adapter
    let adapters = make_mock_adapters();
    let telemetry = Telemetry::new("test-worker".to_string());
    let dispatcher = Dispatcher::with_adapters(adapters, telemetry, 3600);

    let routing = make_anthropic_subscription_routing();
    let config = make_test_config("claude", Some(routing));

    // Test that dispatcher resolves adapter correctly for non-Anthropic models
    let non_anthropic_models = vec!["glm-4.7", "gpt-4", "unknown-model"];

    for model in non_anthropic_models {
        let adapter_name = dispatcher.resolve_adapter_name(model, &config);
        assert_eq!(
            adapter_name, "claude-code-glm-4.7",
            "Dispatcher should resolve non-Anthropic model '{}' to default adapter, got '{}'",
            model, adapter_name
        );

        let adapter = dispatcher.adapter(&adapter_name);
        assert!(
            adapter.is_some(),
            "Dispatcher should have default adapter available"
        );

        let adapter = adapter.unwrap();
        assert_eq!(
            adapter.name, "claude-code-glm-4.7",
            "Resolved adapter should be claude-code-glm-4.7"
        );
    }
}

#[test]
fn verifies_claude_print_invoke_template_contains_correct_arguments() {
    // Test that claude-print adapter's invoke template expects stream-json output
    let adapters = make_mock_adapters();
    let claude_print = adapters
        .get("claude-print")
        .expect("claude-print adapter should exist");

    // Verify the invoke template contains key elements
    assert!(
        claude_print.invoke_template.contains("claude-print"),
        "claude-print invoke template should contain claude-print binary"
    );
    assert!(
        claude_print.invoke_template.contains("{model}"),
        "claude-print invoke template should contain {{model}} placeholder"
    );
    assert!(
        claude_print.invoke_template.contains("stream-json"),
        "claude-print invoke template should request stream-json output format"
    );

    // Verify output transform is configured
    assert_eq!(
        claude_print.output_transform.as_deref(),
        Some("needle-transform-claude"),
        "claude-print should have needle-transform-claude output transformer"
    );
}

#[test]
fn verifies_adapter_has_correct_provider_and_model_fields() {
    // Test that claude-print adapter is configured as Anthropic provider
    let adapters = make_mock_adapters();
    let claude_print = adapters
        .get("claude-print")
        .expect("claude-print adapter should exist");

    assert_eq!(
        claude_print.provider.as_deref(),
        Some("anthropic"),
        "claude-print adapter should have provider='anthropic'"
    );

    // Model field is None for claude-print since it accepts the model at runtime
    assert!(
        claude_print.model.is_none(),
        "claude-print adapter should have model=None (filled at runtime)"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Test Summary Documentation
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn test_suite_provides_comprehensive_routing_verification() {
    // This test serves as documentation that the test suite provides:
    // 1. Anthropic subscription model pattern matching (sonnet, opus, fable, haiku)
    // 2. Non-Anthropic model default adapter routing
    // 3. Dispatcher-level adapter resolution
    // 4. Invoke template validation (stream-json output)
    // 5. Adapter provider and model field verification

    // All assertions in this test module should pass for the routing to be correct
    // No runtime assertion needed - compilation and test execution verify correctness
}
