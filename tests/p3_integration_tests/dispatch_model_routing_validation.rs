//! Dispatch model routing validation tests.
//!
//! Tests the Dispatcher::resolve_adapter_name method to ensure model routing
//! works correctly with various routing configurations and edge cases.

use std::collections::HashMap;
use std::path::PathBuf;

use needle::config::{AgentConfig, Config, RoutingConfig, RoutingRule};
use needle::dispatch::{AgentAdapter, Dispatcher, TokenExtraction};
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

/// Create a minimal config with the given agent and routing settings.
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

/// Create a test adapter for use in dispatcher tests.
fn make_test_adapter(name: &str, cli: &str, template: &str) -> AgentAdapter {
    AgentAdapter {
        name: name.to_string(),
        description: Some(format!("Test adapter: {}", name)),
        agent_cli: cli.to_string(),
        version_command: None,
        input_method: InputMethod::Stdin,
        invoke_template: template.to_string(),
        environment: HashMap::new(),
        timeout_secs: 120,
        idle_timeout_secs: 0,
        hard_timeout_secs: 0,
        provider: None,
        model: None,
        token_extraction: TokenExtraction::None,
        output_transform: None,
        harness: None,
        harness_version: None,
    }
}

/// Create a dispatcher with the given adapters for testing.
fn make_test_dispatcher(adapters: Vec<AgentAdapter>) -> Dispatcher {
    let mut adapter_map = HashMap::new();
    for adapter in adapters {
        adapter_map.insert(adapter.name.clone(), adapter);
    }
    let telemetry = Telemetry::new("test-worker".to_string());
    Dispatcher::with_adapters(adapter_map, telemetry, 3600)
}

// ──────────────────────────────────────────────────────────────────────────────
// Core resolve_adapter_name Tests
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn resolve_adapter_with_no_routing_uses_default() {
    // When no routing is configured, resolve_adapter_name should return agent.default
    let config = make_test_config("claude-sonnet", None);
    let dispatcher = make_test_dispatcher(vec![make_test_adapter(
        "claude-sonnet",
        "claude",
        "claude --model {model}",
    )]);

    let adapter = dispatcher.resolve_adapter_name("any-model", &config);
    assert_eq!(adapter, "claude-sonnet");
}

#[test]
fn resolve_adapter_with_empty_routing_uses_default() {
    // When routing is configured but has no rules, should use agent.default
    let routing = RoutingConfig {
        rules: vec![],
        default_adapter: None,
        strict: false,
    };
    let config = make_test_config("claude-sonnet", Some(routing));
    let dispatcher = make_test_dispatcher(vec![make_test_adapter(
        "claude-sonnet",
        "claude",
        "claude --model {model}",
    )]);

    let adapter = dispatcher.resolve_adapter_name("any-model", &config);
    assert_eq!(adapter, "claude-sonnet");
}

#[test]
fn resolve_adapter_with_routing_default_adapter() {
    // When routing has a default_adapter but no matching rules, should use it
    let routing = RoutingConfig {
        rules: vec![make_rule("sonnet.*", "claude-print")],
        default_adapter: Some("claude-code-glm-4.7".to_string()),
        strict: false,
    };
    let config = make_test_config("fallback", Some(routing));
    let dispatcher = make_test_dispatcher(vec![
        make_test_adapter("claude-print", "claude", "claude --model {model}"),
        make_test_adapter("claude-code-glm-4.7", "claude", "claude --model {model}"),
    ]);

    let adapter = dispatcher.resolve_adapter_name("gpt-4", &config);
    assert_eq!(adapter, "claude-code-glm-4.7");
}

#[test]
fn resolve_adapter_first_match_wins() {
    // First matching rule should win, even if later rules also match
    let routing = RoutingConfig {
        rules: vec![
            make_rule("claude.*", "first-adapter"),
            make_rule("claude-sonnet.*", "second-adapter"), // Never matched
            make_rule("*", "catchall"),
        ],
        default_adapter: Some("default".to_string()),
        strict: false,
    };
    let config = make_test_config("default", Some(routing));
    let dispatcher = make_test_dispatcher(vec![
        make_test_adapter("first-adapter", "agent1", "agent1 --model {model}"),
        make_test_adapter("second-adapter", "agent2", "agent2 --model {model}"),
        make_test_adapter("catchall", "agent3", "agent3 --model {model}"),
    ]);

    // First rule matches
    let adapter = dispatcher.resolve_adapter_name("claude-sonnet-4-6", &config);
    assert_eq!(adapter, "first-adapter");
}

#[test]
fn resolve_adapter_regex_patterns() {
    // Test that regex patterns work correctly
    let routing = RoutingConfig {
        rules: vec![
            make_rule("(claude-)?(sonnet|opus).*", "claude-print"),
            make_rule("gpt-.*", "openai-adapter"),
        ],
        default_adapter: Some("default".to_string()),
        strict: false,
    };
    let config = make_test_config("default", Some(routing));
    let dispatcher = make_test_dispatcher(vec![
        make_test_adapter("claude-print", "claude", "claude --model {model}"),
        make_test_adapter("openai-adapter", "openai", "openai --model {model}"),
    ]);

    // Test various model names
    assert_eq!(
        dispatcher.resolve_adapter_name("sonnet-4-6", &config),
        "claude-print"
    );
    assert_eq!(
        dispatcher.resolve_adapter_name("claude-sonnet-4-6", &config),
        "claude-print"
    );
    assert_eq!(
        dispatcher.resolve_adapter_name("opus-4-6", &config),
        "claude-print"
    );
    assert_eq!(
        dispatcher.resolve_adapter_name("claude-opus-4-6", &config),
        "claude-print"
    );
    assert_eq!(
        dispatcher.resolve_adapter_name("gpt-4", &config),
        "openai-adapter"
    );
    assert_eq!(
        dispatcher.resolve_adapter_name("gpt-3.5-turbo", &config),
        "openai-adapter"
    );
}

#[test]
fn resolve_adapter_glob_patterns() {
    // Test that glob-style patterns work correctly
    let routing = RoutingConfig {
        rules: vec![
            make_rule("claude-*", "claude-adapter"),
            make_rule("gpt-*", "openai-adapter"),
        ],
        default_adapter: Some("default".to_string()),
        strict: false,
    };
    let config = make_test_config("default", Some(routing));
    let dispatcher = make_test_dispatcher(vec![
        make_test_adapter("claude-adapter", "claude", "claude --model {model}"),
        make_test_adapter("openai-adapter", "openai", "openai --model {model}"),
    ]);

    assert_eq!(
        dispatcher.resolve_adapter_name("claude-sonnet-4-6", &config),
        "claude-adapter"
    );
    assert_eq!(
        dispatcher.resolve_adapter_name("claude-opus-4-6", &config),
        "claude-adapter"
    );
    assert_eq!(
        dispatcher.resolve_adapter_name("gpt-4", &config),
        "openai-adapter"
    );
}

#[test]
fn resolve_adapter_catchall_pattern() {
    // Test catch-all pattern ".*" (regex for "match anything")
    let routing = RoutingConfig {
        rules: vec![
            make_rule("claude-sonnet.*", "sonnet-specific"),
            make_rule(".*", "catchall"),
        ],
        default_adapter: Some("default".to_string()),
        strict: false,
    };
    let config = make_test_config("default", Some(routing));
    let dispatcher = make_test_dispatcher(vec![
        make_test_adapter("sonnet-specific", "agent1", "agent1 --model {model}"),
        make_test_adapter("catchall", "agent2", "agent2 --model {model}"),
    ]);

    assert_eq!(
        dispatcher.resolve_adapter_name("claude-sonnet-4-6", &config),
        "sonnet-specific"
    );
    assert_eq!(
        dispatcher.resolve_adapter_name("anything-else", &config),
        "catchall"
    );
}

#[test]
fn resolve_adapter_case_sensitivity() {
    // Test that matching is case-sensitive
    let routing = RoutingConfig {
        rules: vec![make_rule("Claude-Sonnet", "exact-case")],
        default_adapter: Some("default".to_string()),
        strict: false,
    };
    let config = make_test_config("default", Some(routing));
    let dispatcher = make_test_dispatcher(vec![make_test_adapter(
        "exact-case",
        "agent",
        "agent --model {model}",
    )]);

    // Exact case match
    assert_eq!(
        dispatcher.resolve_adapter_name("Claude-Sonnet", &config),
        "exact-case"
    );

    // Different case should not match
    assert_eq!(
        dispatcher.resolve_adapter_name("claude-sonnet", &config),
        "default"
    );
}

#[test]
fn resolve_adapter_invalid_regex_skipped() {
    // Test that invalid regex patterns are skipped with a warning
    let routing = RoutingConfig {
        rules: vec![
            make_rule("[invalid(regex", "bad-adapter"),
            make_rule("sonnet.*", "good-adapter"),
        ],
        default_adapter: Some("default".to_string()),
        strict: false,
    };
    let config = make_test_config("default", Some(routing));
    let dispatcher = make_test_dispatcher(vec![make_test_adapter(
        "good-adapter",
        "agent",
        "agent --model {model}",
    )]);

    // Invalid pattern should be skipped, second pattern should match
    assert_eq!(
        dispatcher.resolve_adapter_name("sonnet-4-6", &config),
        "good-adapter"
    );
}

#[test]
fn resolve_adapter_all_invalid_patterns_uses_default() {
    // Test that when all patterns are invalid, default_adapter is used
    let routing = RoutingConfig {
        rules: vec![
            make_rule("[invalid1", "bad1"),
            make_rule("(unclosed", "bad2"),
        ],
        default_adapter: Some("default".to_string()),
        strict: false,
    };
    let config = make_test_config("fallback", Some(routing));
    let dispatcher = make_test_dispatcher(vec![]);

    assert_eq!(
        dispatcher.resolve_adapter_name("any-model", &config),
        "default"
    );
}

#[test]
fn resolve_adapter_empty_model_name() {
    // Test behavior with empty model name
    let routing = RoutingConfig {
        rules: vec![make_rule(".*", "catchall")],
        default_adapter: Some("default".to_string()),
        strict: false,
    };
    let config = make_test_config("default", Some(routing));
    let dispatcher = make_test_dispatcher(vec![make_test_adapter(
        "catchall",
        "agent",
        "agent --model {model}",
    )]);

    // Empty model should match catchall (.* matches empty string)
    assert_eq!(dispatcher.resolve_adapter_name("", &config), "catchall");
}

#[test]
fn resolve_adapter_real_world_anthropic_subscription() {
    // Real-world test: Anthropic subscription billing routing
    let routing = RoutingConfig {
        rules: vec![make_rule(
            "(claude-)?(sonnet|opus|fable|haiku).*",
            "claude-print",
        )],
        default_adapter: Some("claude-code-glm-4.7".to_string()),
        strict: false,
    };
    let config = make_test_config("claude-sonnet", Some(routing));
    let dispatcher = make_test_dispatcher(vec![
        make_test_adapter("claude-print", "claude", "claude --model {model}"),
        make_test_adapter("claude-code-glm-4.7", "claude", "claude --model {model}"),
    ]);

    // Anthropic models -> subscription billing (claude-print)
    assert_eq!(
        dispatcher.resolve_adapter_name("claude-sonnet-4-6", &config),
        "claude-print"
    );
    assert_eq!(
        dispatcher.resolve_adapter_name("claude-opus-4-6", &config),
        "claude-print"
    );
    assert_eq!(
        dispatcher.resolve_adapter_name("claude-fable-5", &config),
        "claude-print"
    );
    assert_eq!(
        dispatcher.resolve_adapter_name("claude-haiku-4-5-20251001", &config),
        "claude-print"
    );
    assert_eq!(
        dispatcher.resolve_adapter_name("sonnet-4-6", &config),
        "claude-print"
    );

    // Other models -> API billing (claude-code-glm-4.7)
    assert_eq!(
        dispatcher.resolve_adapter_name("gpt-4", &config),
        "claude-code-glm-4.7"
    );
    assert_eq!(
        dispatcher.resolve_adapter_name("gemini-pro", &config),
        "claude-code-glm-4.7"
    );
}

#[test]
fn resolve_adapter_glm_4_7_routing_negative_control() {
    // Test GLM-4.7 routing as a negative control: verifies GLM-4.7 models
    // route through claude-code-glm-4.7 adapter (NOT claude-print)
    //
    // This serves as a control test to ensure not all models route to claude-print,
    // confirming that the routing logic correctly distinguishes between Anthropic
    // subscription models and other models like GLM-4.7.

    let routing = RoutingConfig {
        rules: vec![make_rule(
            "(claude-)?(sonnet|opus|fable|haiku).*",
            "claude-print",
        )],
        default_adapter: Some("claude-code-glm-4.7".to_string()),
        strict: false,
    };
    let config = make_test_config("claude-sonnet", Some(routing));
    let dispatcher = make_test_dispatcher(vec![
        make_test_adapter("claude-print", "claude", "claude --model {model}"),
        make_test_adapter("claude-code-glm-4.7", "claude", "claude --model {model}"),
    ]);

    // GLM-4.7 models should route to claude-code-glm-4.7 (NOT claude-print)
    // This is the negative control: proving non-Anthropic models don't route to claude-print
    assert_eq!(
        dispatcher.resolve_adapter_name("glm-4.7", &config),
        "claude-code-glm-4.7",
        "GLM-4.7 should route to claude-code-glm-4.7 adapter, not claude-print"
    );
    assert_eq!(
        dispatcher.resolve_adapter_name("glm-4.7-turbo", &config),
        "claude-code-glm-4.7",
        "GLM-4.7 variants should route to claude-code-glm-4.7 adapter"
    );
    assert_eq!(
        dispatcher.resolve_adapter_name("glm-4.7-vision", &config),
        "claude-code-glm-4.7",
        "GLM-4.7 vision models should route to claude-code-glm-4.7 adapter"
    );

    // Verify claude-print is NOT selected for any GLM-4.7 model
    let glm_adapter = dispatcher.resolve_adapter_name("glm-4.7", &config);
    assert_ne!(
        glm_adapter, "claude-print",
        "GLM-4.7 must NOT route through claude-print (negative control verification)"
    );
}

#[test]
fn resolve_adapter_with_special_characters() {
    // Test that adapter names with special characters are preserved
    let routing = RoutingConfig {
        rules: vec![make_rule("sonnet.*", "Claude-Print-v2.0")],
        default_adapter: None,
        strict: false,
    };
    let config = make_test_config("default", Some(routing));
    let dispatcher = make_test_dispatcher(vec![make_test_adapter(
        "Claude-Print-v2.0",
        "claude",
        "claude --model {model}",
    )]);

    let adapter = dispatcher.resolve_adapter_name("sonnet-4-6", &config);
    assert_eq!(adapter, "Claude-Print-v2.0");
}

#[test]
fn resolve_adapter_multiple_specific_rules() {
    // Test multiple specific patterns in correct order
    let routing = RoutingConfig {
        rules: vec![
            make_rule("^claude-sonnet-4-6$", "exact-sonnet-46"),
            make_rule("^claude-sonnet-4-5$", "exact-sonnet-45"),
            make_rule("claude-sonnet.*", "any-sonnet"),
        ],
        default_adapter: Some("default".to_string()),
        strict: false,
    };
    let config = make_test_config("default", Some(routing));
    let dispatcher = make_test_dispatcher(vec![
        make_test_adapter("exact-sonnet-46", "agent1", "agent1 --model {model}"),
        make_test_adapter("exact-sonnet-45", "agent2", "agent2 --model {model}"),
        make_test_adapter("any-sonnet", "agent3", "agent3 --model {model}"),
    ]);

    // Exact matches
    assert_eq!(
        dispatcher.resolve_adapter_name("claude-sonnet-4-6", &config),
        "exact-sonnet-46"
    );
    assert_eq!(
        dispatcher.resolve_adapter_name("claude-sonnet-4-5", &config),
        "exact-sonnet-45"
    );

    // Pattern match
    assert_eq!(
        dispatcher.resolve_adapter_name("claude-sonnet-4-7", &config),
        "any-sonnet"
    );
}
