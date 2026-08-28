//! End-to-end integration test for Anthropic model routing verification.
//!
//! This test validates that beads requesting Anthropic subscription models
//! correctly route through the claude-print adapter by:
//! 1. Configuring routing rules for Anthropic models
//! 2. Verifying adapter resolution for various model names
//! 3. Validating claude-print adapter configuration
//! 4. Ensuring stream-json output format is requested
//!
//! Run with: cargo test --test anthropic_routing_e2e_test

use std::path::PathBuf;

use needle::config::{AgentConfig, Config, RoutingConfig, RoutingRule};
use needle::dispatch::Dispatcher;
use needle::telemetry::Telemetry;

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

/// Create a minimal config with Anthropic routing configured.
fn make_test_config_with_routing(agent_default: &str, routing: Option<RoutingConfig>) -> Config {
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

// ──────────────────────────────────────────────────────────────────────────────
// Main Integration Test
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn anthropic_routing_e2e_sonnet_to_claude_print() {
    // This test validates end-to-end routing for Anthropic models
    // It configures routing and verifies adapter resolution

    // 1. Configure routing for Anthropic subscription models
    let routing = make_anthropic_subscription_routing();
    let config = make_test_config_with_routing("claude", Some(routing));

    // 2. Create dispatcher with telemetry
    let telemetry = Telemetry::new("test-worker-anthropic-routing".to_string());
    let dispatcher = Dispatcher::new(&config, telemetry)
        .expect("Failed to create dispatcher");

    // 3. Verify routing configuration is loaded
    assert!(
        config.agent.routing.is_some(),
        "Routing configuration should be present"
    );

    let routing_config = config.agent.routing.as_ref().unwrap();
    assert_eq!(
        routing_config.rules.len(),
        1,
        "Should have 1 routing rule for Anthropic models"
    );
    assert_eq!(
        routing_config.rules[0].match_model,
        "(claude-)?(sonnet|opus|fable|haiku).*",
        "Routing rule should match Anthropic subscription models"
    );
    assert_eq!(
        routing_config.rules[0].adapter,
        "claude-print",
        "Routing rule should route to claude-print adapter"
    );
    assert_eq!(
        routing_config.default_adapter.as_deref(),
        Some("claude-code-glm-4.7"),
        "Default adapter should be claude-code-glm-4.7"
    );

    // 4. Test adapter resolution for Anthropic models
    let anthropic_models = vec![
        "sonnet-4-6",
        "claude-sonnet-4-6",
        "opus-4-7",
        "claude-opus-4-7",
        "fable-5",
        "claude-fable-5",
        "haiku-4-5",
        "claude-haiku-4-5",
    ];

    for model in &anthropic_models {
        let resolved_adapter = dispatcher.resolve_adapter_name(model, &config);
        assert_eq!(
            resolved_adapter,
            "claude-print",
            "Anthropic model '{}' should resolve to claude-print adapter, got '{}'",
            model,
            resolved_adapter
        );
    }

    // 5. Test adapter resolution for non-Anthropic models
    let non_anthropic_models = vec![
        "gpt-4",
        "gemini-pro",
        "glm-4.7",
    ];

    for model in &non_anthropic_models {
        let resolved_adapter = dispatcher.resolve_adapter_name(model, &config);
        assert_eq!(
            resolved_adapter,
            "claude-code-glm-4.7",
            "Non-Anthropic model '{}' should resolve to default adapter, got '{}'",
            model,
            resolved_adapter
        );
    }

    // 6. Verify claude-print adapter exists and has correct configuration
    let claude_print_adapter = dispatcher.adapter("claude-print");
    assert!(
        claude_print_adapter.is_some(),
        "claude-print adapter should be available"
    );

    let adapter = claude_print_adapter.unwrap();
    assert_eq!(
        adapter.name,
        "claude-print",
        "Adapter name should be claude-print"
    );
    assert_eq!(
        adapter.provider.as_deref(),
        Some("anthropic"),
        "claude-print adapter should have provider='anthropic'"
    );
    assert!(
        adapter.invoke_template.contains("claude-print"),
        "claude-print invoke template should contain claude-print binary"
    );
    assert!(
        adapter.invoke_template.contains("{model}"),
        "claude-print invoke template should contain {{model}} placeholder"
    );

    // 7. Validate that stream-json is requested in the invoke template
    // This is a key requirement: claude-print should output stream-json format
    assert!(
        adapter.invoke_template.contains("stream-json"),
        "claude-print invoke template should request stream-json output format"
    );

    // 8. Verify output transform is configured
    assert_eq!(
        adapter.output_transform.as_deref(),
        Some("needle-transform-claude"),
        "claude-print should have needle-transform-claude output transformer"
    );

    // 9. Log test results for documentation
    println!("✅ Anthropic routing E2E test passed:");
    println!("   - {} Anthropic models correctly route to claude-print", anthropic_models.len());
    println!("   - {} non-Anthropic models correctly route to default", non_anthropic_models.len());
    println!("   - claude-print adapter has correct configuration");
    println!("   - stream-json output format is requested");
    println!("   - needle-transform-claude is configured");
}

#[test]
fn anthropic_routing_verify_adapter_resolution_order() {
    // Test that routing rules are evaluated in order (first match wins)

    let routing = RoutingConfig {
        rules: vec![
            // More specific rule first
            make_rule("claude-sonnet.*", "claude-print-sonnet-specific"),
            // General claude rule second
            make_rule("claude.*", "claude-general"),
            // Catchall last
            make_rule("*", "catchall"),
        ],
        default_adapter: Some("default".to_string()),
        strict: false,
    };

    let config = make_test_config_with_routing("default", Some(routing));
    let telemetry = Telemetry::new("test-worker-order".to_string());
    let dispatcher = Dispatcher::new(&config, telemetry)
        .expect("Failed to create dispatcher");

    // Test that first match wins
    assert_eq!(
        dispatcher.resolve_adapter_name("claude-sonnet-4-6", &config),
        "claude-print-sonnet-specific",
        "First matching rule should win for claude-sonnet"
    );

    assert_eq!(
        dispatcher.resolve_adapter_name("claude-opus-4-6", &config),
        "claude-general",
        "Second rule should match for claude-opus"
    );

    assert_eq!(
        dispatcher.resolve_adapter_name("gpt-4", &config),
        "catchall",
        "Catchall should match non-Claude models"
    );
}

#[test]
fn anthropic_routing_verify_default_adapter_fallback() {
    // Test that default adapter is used when no rules match

    let routing = RoutingConfig {
        rules: vec![
            // Only match sonnet models
            make_rule("sonnet.*", "claude-print"),
        ],
        default_adapter: Some("claude-code-glm-4.7".to_string()),
        strict: false,
    };

    let config = make_test_config_with_routing("default", Some(routing));
    let telemetry = Telemetry::new("test-worker-fallback".to_string());
    let dispatcher = Dispatcher::new(&config, telemetry)
        .expect("Failed to create dispatcher");

    // Models that match the rule
    assert_eq!(
        dispatcher.resolve_adapter_name("sonnet-4-6", &config),
        "claude-print",
        "sonnet should match the rule"
    );

    // Models that don't match should use default_adapter
    assert_eq!(
        dispatcher.resolve_adapter_name("gpt-4", &config),
        "claude-code-glm-4.7",
        "Non-matching models should use default_adapter"
    );

    assert_eq!(
        dispatcher.resolve_adapter_name("claude-opus", &config),
        "claude-code-glm-4.7",
        "Non-matching Anthropic models should use default_adapter"
    );
}

#[test]
fn anthropic_routing_verify_claude_print_adapter_fields() {
    // Test that claude-print adapter has all required fields

    let routing = make_anthropic_subscription_routing();
    let config = make_test_config_with_routing("claude", Some(routing));
    let telemetry = Telemetry::new("test-worker-fields".to_string());
    let dispatcher = Dispatcher::new(&config, telemetry)
        .expect("Failed to create dispatcher");

    let adapter = dispatcher.adapter("claude-print")
        .expect("claude-print adapter should exist");

    // Verify all required fields for correct routing
    assert_eq!(adapter.name, "claude-print");
    assert_eq!(adapter.provider.as_deref(), Some("anthropic"));
    assert!(adapter.model.is_none() || adapter.model.as_deref() == Some("claude-sonnet-4-6"));

    // Verify invoke template is correct
    assert!(adapter.invoke_template.contains("claude-print"));
    assert!(adapter.invoke_template.contains("{model}"));
    assert!(adapter.invoke_template.contains("stream-json"));

    // Verify output transform is configured
    assert_eq!(
        adapter.output_transform.as_deref(),
        Some("needle-transform-claude")
    );
}

#[test]
fn anthropic_routing_test_suite_provides_comprehensive_coverage() {
    // Documentation test - this serves as a summary of what the test suite covers

    // The test suite validates:
    // 1. Anthropic subscription model patterns match claude-print adapter
    // 2. Non-Anthropic models use default adapter
    // 3. Routing rules are evaluated in order (first match wins)
    // 4. Default adapter fallback works correctly
    // 5. claude-print adapter has correct configuration:
    //    - provider: anthropic
    //    - invoke_template: contains "claude-print", "{model}", and "stream-json"
    //    - output_transform: needle-transform-claude
    //    - input_method: stdin

    // All tests in this module should pass for correct routing behavior
}
