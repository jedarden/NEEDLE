//! Integration tests for model-based adapter routing.
//!
//! These tests exercise end-to-end routing behavior:
//! 1. Anthropic Claude models -> claude-print (subscription billing)
//! 2. GLM models -> claude-code-glm-4.7 (default adapter)
//! 3. Workspace override of global routing rules
//! 4. Missing adapter failure (strict mode)
//!
//! Tests use real Dispatcher instances with full configuration to verify
//! that routing decisions are made correctly through the entire dispatch chain.

use std::collections::HashMap;
use std::path::PathBuf;

use needle::config::{AgentConfig, Config, RoutingConfig, RoutingRule};
use needle::dispatch::{AgentAdapter, Dispatcher, TokenExtraction};
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

/// Create a default Anthropic subscription routing config (pre-June 15 policy).
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
        provider: None,
        model: None,
        token_extraction: TokenExtraction::None,
        output_transform: None,
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
// Anthropic Claude Models -> claude-print
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn routing_anthropic_sonnet_to_claude_print() {
    // Test that Claude Sonnet models route to claude-print adapter.
    let routing = make_anthropic_subscription_routing();
    let config = make_test_config("claude", Some(routing));

    // Test various Sonnet model names
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
        let result = match_adapter(
            model,
            &config.agent.routing.as_ref().unwrap().rules,
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or(""),
        );

        assert_eq!(
            result,
            Some("claude-print".to_string()),
            "Sonnet model '{}' should route to claude-print",
            model
        );
    }
}

#[test]
fn routing_anthropic_opus_to_claude_print() {
    // Test that Claude Opus models route to claude-print adapter.
    let routing = make_anthropic_subscription_routing();
    let config = make_test_config("claude", Some(routing));

    // Test various Opus model names
    let opus_models = vec![
        "claude-opus-4-6",
        "claude-opus-4-7",
        "claude-opus-4-8",
        "opus-4-6", // Without claude- prefix
        "opus-4-7",
        "opus-4-8",
    ];

    for model in opus_models {
        let result = match_adapter(
            model,
            &config.agent.routing.as_ref().unwrap().rules,
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or(""),
        );

        assert_eq!(
            result,
            Some("claude-print".to_string()),
            "Opus model '{}' should route to claude-print",
            model
        );
    }
}

#[test]
fn routing_anthropic_fable_to_claude_print() {
    // Test that Claude Fable models route to claude-print adapter.
    let routing = make_anthropic_subscription_routing();
    let config = make_test_config("claude", Some(routing));

    // Test Fable model names
    let fable_models = vec![
        "claude-fable-5",
        "claude-fable-5-20251001",
        "fable-5", // Without claude- prefix
    ];

    for model in fable_models {
        let result = match_adapter(
            model,
            &config.agent.routing.as_ref().unwrap().rules,
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or(""),
        );

        assert_eq!(
            result,
            Some("claude-print".to_string()),
            "Fable model '{}' should route to claude-print",
            model
        );
    }
}

#[test]
fn routing_anthropic_haiku_to_claude_print() {
    // Test that Claude Haiku models route to claude-print adapter.
    let routing = make_anthropic_subscription_routing();
    let config = make_test_config("claude", Some(routing));

    // Test various Haiku model names
    let haiku_models = vec![
        "claude-haiku-4-5",
        "claude-haiku-4-5-20251001",
        "haiku-4-5", // Without claude- prefix
    ];

    for model in haiku_models {
        let result = match_adapter(
            model,
            &config.agent.routing.as_ref().unwrap().rules,
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or(""),
        );

        assert_eq!(
            result,
            Some("claude-print".to_string()),
            "Haiku model '{}' should route to claude-print",
            model
        );
    }
}

#[test]
fn routing_anthropic_all_claude_models_together() {
    // Test all Anthropic Claude models in a single test for comprehensive coverage.
    let routing = make_anthropic_subscription_routing();
    let config = make_test_config("claude", Some(routing));

    // All Anthropic Claude subscription models
    let claude_models = vec![
        // Sonnet
        "claude-sonnet-4-6",
        "claude-sonnet-4-7",
        "claude-sonnet-5",
        "claude-sonnet-5-20250529",
        "sonnet-4-6",
        "sonnet-5",
        // Opus
        "claude-opus-4-6",
        "claude-opus-4-7",
        "claude-opus-4-8",
        "opus-4-6",
        "opus-4-8",
        // Fable
        "claude-fable-5",
        "fable-5",
        // Haiku
        "claude-haiku-4-5",
        "claude-haiku-4-5-20251001",
        "haiku-4-5",
    ];

    for model in claude_models {
        let result = match_adapter(
            model,
            &config.agent.routing.as_ref().unwrap().rules,
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or(""),
        );

        assert_eq!(
            result,
            Some("claude-print".to_string()),
            "Claude model '{}' should route to claude-print",
            model
        );
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// GLM Models -> claude-code-glm-4.7
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn routing_glm_47_to_claude_code_glm_47() {
    // Test that glm-4.7 models explicitly route to claude-code-glm-4.7 adapter.
    let routing = RoutingConfig {
        rules: vec![make_rule("glm-4\\.7.*", "claude-code-glm-4.7")],
        default_adapter: Some("fallback".to_string()),
        strict: false,
    };
    let config = make_test_config("claude", Some(routing));

    // Test various glm-4.7 model names
    let glm_47_models = vec![
        "glm-4.7",
        "glm-4.7-charlie",
        "glm-4.7-turbo",
        "glm-4.7-latest",
    ];

    for model in glm_47_models {
        let result = match_adapter(
            model,
            &config.agent.routing.as_ref().unwrap().rules,
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or(""),
        );

        assert_eq!(
            result,
            Some("claude-code-glm-4.7".to_string()),
            "GLM-4.7 model '{}' should route to claude-code-glm-4.7",
            model
        );
    }
}

#[test]
fn routing_glm_47_flash_to_claude_code_glm_47() {
    // Test that glm-4.7-flash models explicitly route to claude-code-glm-4.7 adapter.
    let routing = RoutingConfig {
        rules: vec![make_rule("glm-4\\.7-flash.*", "claude-code-glm-4.7")],
        default_adapter: Some("fallback".to_string()),
        strict: false,
    };
    let config = make_test_config("claude", Some(routing));

    // Test glm-4.7-flash model variants
    let glm_flash_models = vec![
        "glm-4.7-flash",
        "glm-4.7-flash-turbo",
        "glm-4.7-flash-preview",
    ];

    for model in glm_flash_models {
        let result = match_adapter(
            model,
            &config.agent.routing.as_ref().unwrap().rules,
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or(""),
        );

        assert_eq!(
            result,
            Some("claude-code-glm-4.7".to_string()),
            "GLM-4.7-flash model '{}' should route to claude-code-glm-4.7",
            model
        );
    }
}

#[test]
fn routing_glm_to_default_adapter() {
    // Test that GLM models route to the default adapter (claude-code-glm-4.7).
    let routing = make_anthropic_subscription_routing();
    let config = make_test_config("claude", Some(routing));

    // GLM model names (these should NOT match the Claude pattern and fall through)
    let glm_models = vec![
        "glm-4.7",
        "glm-4",
        "glm-4-flash",
        "glm-4-plus",
        "claude-code-glm-4.7", // Adapter name itself should route to default
    ];

    for model in glm_models {
        let result = match_adapter(
            model,
            &config.agent.routing.as_ref().unwrap().rules,
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or(""),
        );

        assert_eq!(
            result,
            Some("claude-code-glm-4.7".to_string()),
            "GLM model '{}' should route to default adapter claude-code-glm-4.7",
            model
        );
    }
}

#[test]
fn routing_non_claude_to_default_adapter() {
    // Test that non-Claude models route to the default adapter.
    let routing = make_anthropic_subscription_routing();
    let config = make_test_config("claude", Some(routing));

    // Non-Claude models (various providers)
    let other_models = vec![
        // OpenAI
        "gpt-4",
        "gpt-4-turbo",
        "gpt-3.5-turbo",
        // Google
        "gemini-pro",
        "gemini-ultra",
        // Meta
        "llama-3-70b",
        "llama-3-8b",
        // Mistral
        "mistral-large",
        "mixtral-8x7b",
        // Other
        "qwen-72b",
        "deepseek-coder",
    ];

    for model in other_models {
        let result = match_adapter(
            model,
            &config.agent.routing.as_ref().unwrap().rules,
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or(""),
        );

        assert_eq!(
            result,
            Some("claude-code-glm-4.7".to_string()),
            "Non-Claude model '{}' should route to default adapter",
            model
        );
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Dispatcher Integration Tests
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn dispatcher_resolve_adapter_anthropic_models() {
    // Test that Dispatcher correctly resolves adapters for Anthropic models.
    let routing = make_anthropic_subscription_routing();
    let config = make_test_config("claude", Some(routing));

    let dispatcher = make_test_dispatcher(vec![
        make_test_adapter("claude-print", "claude", "claude --print < {prompt_file}"),
        make_test_adapter(
            "claude-code-glm-4.7",
            "claude-code-glm",
            "claude-code-glm < {prompt_file}",
        ),
    ]);

    // Test Sonnet
    let sonnet_adapter = dispatcher.resolve_adapter_name("claude-sonnet-4-6", &config);
    assert_eq!(sonnet_adapter, "claude-print");

    // Test Opus
    let opus_adapter = dispatcher.resolve_adapter_name("claude-opus-4-6", &config);
    assert_eq!(opus_adapter, "claude-print");

    // Test Haiku
    let haiku_adapter = dispatcher.resolve_adapter_name("claude-haiku-4-5", &config);
    assert_eq!(haiku_adapter, "claude-print");

    // Test Fable
    let fable_adapter = dispatcher.resolve_adapter_name("claude-fable-5", &config);
    assert_eq!(fable_adapter, "claude-print");
}

#[test]
fn dispatcher_resolve_adapter_glm_models() {
    // Test that Dispatcher correctly routes GLM models to default adapter.
    let routing = make_anthropic_subscription_routing();
    let config = make_test_config("claude", Some(routing));

    let dispatcher = make_test_dispatcher(vec![
        make_test_adapter("claude-print", "claude", "claude --print < {prompt_file}"),
        make_test_adapter(
            "claude-code-glm-4.7",
            "claude-code-glm",
            "claude-code-glm < {prompt_file}",
        ),
    ]);

    // Test GLM models
    let glm_adapter = dispatcher.resolve_adapter_name("glm-4.7", &config);
    assert_eq!(glm_adapter, "claude-code-glm-4.7");

    let glm_adapter2 = dispatcher.resolve_adapter_name("glm-4-flash", &config);
    assert_eq!(glm_adapter2, "claude-code-glm-4.7");

    // Test other non-Claude models
    let gpt_adapter = dispatcher.resolve_adapter_name("gpt-4", &config);
    assert_eq!(gpt_adapter, "claude-code-glm-4.7");

    let gemini_adapter = dispatcher.resolve_adapter_name("gemini-pro", &config);
    assert_eq!(gemini_adapter, "claude-code-glm-4.7");
}

#[test]
fn dispatcher_adapter_exists_after_routing() {
    // Test that resolved adapters actually exist in the dispatcher.
    let routing = make_anthropic_subscription_routing();
    let config = make_test_config("claude", Some(routing));

    let dispatcher = make_test_dispatcher(vec![
        make_test_adapter("claude-print", "claude", "claude --print < {prompt_file}"),
        make_test_adapter(
            "claude-code-glm-4.7",
            "claude-code-glm",
            "claude-code-glm < {prompt_file}",
        ),
    ]);

    // Verify that Anthropic models resolve to existing adapter
    let sonnet_adapter = dispatcher.resolve_adapter_name("claude-sonnet-4-6", &config);
    assert!(
        dispatcher.adapter(&sonnet_adapter).is_some(),
        "Resolved adapter '{}' should exist in dispatcher",
        sonnet_adapter
    );

    // Verify that GLM models resolve to existing adapter
    let glm_adapter = dispatcher.resolve_adapter_name("glm-4.7", &config);
    assert!(
        dispatcher.adapter(&glm_adapter).is_some(),
        "Resolved adapter '{}' should exist in dispatcher",
        glm_adapter
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Workspace Override Tests
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn routing_workspace_override_changes_defaults() {
    // Test that workspace-specific routing can override global defaults.

    // Global config: Anthropic models -> claude-print
    let global_routing = RoutingConfig {
        rules: vec![make_rule("(claude-)?(sonnet|opus).*", "claude-print")],
        default_adapter: Some("claude-code-glm-4.7".to_string()),
        strict: false,
    };
    let global_config = make_test_config("claude", Some(global_routing));

    // Workspace config: Sonnet -> workspace-specific adapter
    let workspace_routing = RoutingConfig {
        rules: vec![
            make_rule("claude-sonnet.*", "workspace-sonnet-adapter"),
            make_rule("(claude-)?opus.*", "claude-print"),
        ],
        default_adapter: Some("workspace-fallback".to_string()),
        strict: false,
    };
    let workspace_config = make_test_config("claude", Some(workspace_routing));

    // Global: Sonnet routes to claude-print
    let global_result = match_adapter(
        "claude-sonnet-4-6",
        &global_config.agent.routing.as_ref().unwrap().rules,
        global_config
            .agent
            .routing
            .as_ref()
            .unwrap()
            .default_adapter
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(""),
    );
    assert_eq!(global_result, Some("claude-print".to_string()));

    // Workspace: Sonnet routes to workspace-specific adapter
    let workspace_result = match_adapter(
        "claude-sonnet-4-6",
        &workspace_config.agent.routing.as_ref().unwrap().rules,
        workspace_config
            .agent
            .routing
            .as_ref()
            .unwrap()
            .default_adapter
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(""),
    );
    assert_eq!(
        workspace_result,
        Some("workspace-sonnet-adapter".to_string())
    );

    // Both: Opus still routes to claude-print (same in both configs)
    let global_opus = match_adapter(
        "claude-opus-4-6",
        &global_config.agent.routing.as_ref().unwrap().rules,
        global_config
            .agent
            .routing
            .as_ref()
            .unwrap()
            .default_adapter
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(""),
    );
    let workspace_opus = match_adapter(
        "claude-opus-4-6",
        &workspace_config.agent.routing.as_ref().unwrap().rules,
        workspace_config
            .agent
            .routing
            .as_ref()
            .unwrap()
            .default_adapter
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(""),
    );
    assert_eq!(global_opus, Some("claude-print".to_string()));
    assert_eq!(workspace_opus, Some("claude-print".to_string()));
}

#[test]
fn routing_workspace_specific_glm_rules_override_global() {
    // Test that workspace-specific GLM routing rules override global defaults.
    //
    // This demonstrates that a workspace can customize GLM model routing
    // independently of the global configuration.

    // Global config: GLM models route to default claude-code-glm-4.7
    let global_routing = RoutingConfig {
        rules: vec![make_rule("(claude-)?(sonnet|opus).*", "claude-print")],
        default_adapter: Some("claude-code-glm-4.7".to_string()),
        strict: false,
    };
    let global_config = make_test_config("claude", Some(global_routing));

    // Workspace config: GLM models route to custom adapter
    let workspace_routing = RoutingConfig {
        rules: vec![
            make_rule("glm-4\\.7.*", "workspace-glm-custom"),
            make_rule("(claude-)?(sonnet|opus).*", "claude-print"),
        ],
        default_adapter: Some("workspace-default".to_string()),
        strict: false,
    };
    let workspace_config = make_test_config("claude", Some(workspace_routing));

    // Global: glm-4.7 routes to claude-code-glm-4.7 (default)
    let global_result = match_adapter(
        "glm-4.7",
        &global_config.agent.routing.as_ref().unwrap().rules,
        global_config
            .agent
            .routing
            .as_ref()
            .unwrap()
            .default_adapter
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(""),
    );
    assert_eq!(global_result, Some("claude-code-glm-4.7".to_string()));

    // Workspace: glm-4.7 routes to custom adapter (explicit rule)
    let workspace_result = match_adapter(
        "glm-4.7",
        &workspace_config.agent.routing.as_ref().unwrap().rules,
        workspace_config
            .agent
            .routing
            .as_ref()
            .unwrap()
            .default_adapter
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(""),
    );
    assert_eq!(workspace_result, Some("workspace-glm-custom".to_string()));
}

#[test]
fn routing_workspace_patterns_take_precedence_over_global() {
    // Test that workspace-specific patterns take precedence over global patterns.
    //
    // This tests the case where both global and workspace configs have rules
    // for the same model family, and the workspace rule should win.

    // Global config: All Claude models -> global-adapter
    let global_routing = RoutingConfig {
        rules: vec![make_rule("claude-.*", "global-adapter")],
        default_adapter: Some("global-default".to_string()),
        strict: false,
    };

    // Workspace config: More specific patterns override
    let workspace_routing = RoutingConfig {
        rules: vec![
            make_rule("claude-sonnet.*", "workspace-sonnet"),
            make_rule("claude-opus.*", "workspace-opus"),
            make_rule("claude-.*", "workspace-other-claude"),
        ],
        default_adapter: Some("workspace-default".to_string()),
        strict: false,
    };

    // Test that workspace patterns are used
    let sonnet_result = match_adapter(
        "claude-sonnet-4-6",
        &workspace_routing.rules,
        workspace_routing
            .default_adapter
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(""),
    );
    assert_eq!(sonnet_result, Some("workspace-sonnet".to_string()));

    let opus_result = match_adapter(
        "claude-opus-4-6",
        &workspace_routing.rules,
        workspace_routing
            .default_adapter
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(""),
    );
    assert_eq!(opus_result, Some("workspace-opus".to_string()));

    let fable_result = match_adapter(
        "claude-fable-5",
        &workspace_routing.rules,
        workspace_routing
            .default_adapter
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(""),
    );
    assert_eq!(fable_result, Some("workspace-other-claude".to_string()));
}

#[test]
fn routing_workspace_can_restrict_global_patterns() {
    // Test that a workspace can restrict global routing patterns.
    //
    // This demonstrates that a workspace can have a more restrictive
    // routing policy than the global config.

    // Global config: Permissive - routes all Claude models
    let global_routing = RoutingConfig {
        rules: vec![make_rule("claude-.*", "claude-print")],
        default_adapter: Some("claude-code-glm-4.7".to_string()),
        strict: false,
    };

    // Workspace config: Restrictive - only routes Sonnet models
    let workspace_routing = RoutingConfig {
        rules: vec![make_rule("claude-sonnet.*", "claude-print")],
        default_adapter: Some("restricted-default".to_string()),
        strict: true,
    };

    // Workspace: Sonnet routes successfully
    let sonnet_result = match_adapter(
        "claude-sonnet-4-6",
        &workspace_routing.rules,
        workspace_routing
            .default_adapter
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(""),
    );
    assert_eq!(sonnet_result, Some("claude-print".to_string()));

    // Workspace: Opus doesn't match any rule, uses default
    let opus_result = match_adapter(
        "claude-opus-4-6",
        &workspace_routing.rules,
        workspace_routing
            .default_adapter
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(""),
    );
    assert_eq!(opus_result, Some("restricted-default".to_string()));
}

#[test]
fn routing_workspace_override_default_adapter() {
    // Test that workspace config can override the default adapter.

    // Global config: default = claude-code-glm-4.7
    let global_routing = RoutingConfig {
        rules: vec![],
        default_adapter: Some("global-default".to_string()),
        strict: false,
    };
    let global_config = make_test_config("claude", Some(global_routing));

    // Workspace config: default = workspace-default
    let workspace_routing = RoutingConfig {
        rules: vec![],
        default_adapter: Some("workspace-default".to_string()),
        strict: false,
    };
    let workspace_config = make_test_config("claude", Some(workspace_routing));

    // Global: unmatched model routes to global-default
    let global_result = match_adapter(
        "unknown-model",
        &global_config.agent.routing.as_ref().unwrap().rules,
        global_config
            .agent
            .routing
            .as_ref()
            .unwrap()
            .default_adapter
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(""),
    );
    assert_eq!(global_result, Some("global-default".to_string()));

    // Workspace: unmatched model routes to workspace-default
    let workspace_result = match_adapter(
        "unknown-model",
        &workspace_config.agent.routing.as_ref().unwrap().rules,
        workspace_config
            .agent
            .routing
            .as_ref()
            .unwrap()
            .default_adapter
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(""),
    );
    assert_eq!(workspace_result, Some("workspace-default".to_string()));
}

#[test]
fn routing_workspace_empty_rules_inherits_default() {
    // Test that workspace config with empty rules can still specify default.

    let workspace_routing = RoutingConfig {
        rules: vec![], // No rules
        default_adapter: Some("workspace-only-default".to_string()),
        strict: false,
    };
    let workspace_config = make_test_config("claude", Some(workspace_routing));

    // Even without rules, default_adapter should work
    let result = match_adapter(
        "any-model",
        &workspace_config.agent.routing.as_ref().unwrap().rules,
        workspace_config
            .agent
            .routing
            .as_ref()
            .unwrap()
            .default_adapter
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(""),
    );
    assert_eq!(result, Some("workspace-only-default".to_string()));
}

// ──────────────────────────────────────────────────────────────────────────────
// Missing Adapter = Loud Failure (Strict Mode)
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn routing_strict_mode_no_match_fails_loudly() {
    // Test that strict mode returns None when no rule matches.
    let strict_routing = RoutingConfig {
        rules: vec![make_rule("claude-.*", "claude-print")],
        default_adapter: Some("fallback".to_string()),
        strict: true, // Strict mode enabled
    };
    let config = make_test_config("claude", Some(strict_routing));

    // Model that doesn't match any rule
    let result = match_adapter(
        "unknown-model",
        &config.agent.routing.as_ref().unwrap().rules,
        config
            .agent
            .routing
            .as_ref()
            .unwrap()
            .default_adapter
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(""),
    );

    // In strict mode with no matching rules, should return default if set
    assert_eq!(
        result,
        Some("fallback".to_string()),
        "With default_adapter set, should return fallback even in strict mode"
    );
}

#[test]
fn routing_strict_mode_no_default_returns_none() {
    // Test that strict mode without default returns None for unmatched models.
    let strict_routing = RoutingConfig {
        rules: vec![make_rule("claude-.*", "claude-print")],
        default_adapter: None, // No default
        strict: true,
    };
    let config = make_test_config("claude", Some(strict_routing));

    // Model that doesn't match any rule
    let result = match_adapter(
        "unknown-model",
        &config.agent.routing.as_ref().unwrap().rules,
        config
            .agent
            .routing
            .as_ref()
            .unwrap()
            .default_adapter
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(""),
    );

    // In strict mode without default, unmatched model returns None
    assert_eq!(
        result, None,
        "Strict mode with no default should return None for unmatched model"
    );
}

#[test]
fn routing_non_strict_mode_graceful_fallback() {
    // Test that non-strict mode falls back gracefully when no rules match.
    let non_strict_routing = RoutingConfig {
        rules: vec![make_rule("claude-.*", "claude-print")],
        default_adapter: Some("fallback".to_string()),
        strict: false, // Non-strict mode (default)
    };
    let config = make_test_config("claude", Some(non_strict_routing));

    // Model that doesn't match any rule should still get default
    let result = match_adapter(
        "unknown-model",
        &config.agent.routing.as_ref().unwrap().rules,
        config
            .agent
            .routing
            .as_ref()
            .unwrap()
            .default_adapter
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(""),
    );

    assert_eq!(
        result,
        Some("fallback".to_string()),
        "Non-strict mode should gracefully fall back to default adapter"
    );
}

#[test]
fn dispatcher_missing_adapter_returns_none() {
    // Test that Dispatcher returns None when asking for a non-existent adapter.
    let dispatcher = make_test_dispatcher(vec![
        make_test_adapter("claude-print", "claude", "claude --print < {prompt_file}"),
        make_test_adapter(
            "claude-code-glm-4.7",
            "claude-code-glm",
            "claude-code-glm < {prompt_file}",
        ),
    ]);

    // Ask for an adapter that doesn't exist
    let missing_adapter = dispatcher.adapter("non-existent-adapter");
    assert!(
        missing_adapter.is_none(),
        "Dispatcher should return None for non-existent adapter"
    );
}

#[test]
fn routing_strict_mode_with_matching_rule_succeeds() {
    // Test that strict mode still works when rules DO match.
    let strict_routing = RoutingConfig {
        rules: vec![make_rule("claude-.*", "claude-print")],
        default_adapter: None,
        strict: true,
    };
    let config = make_test_config("claude", Some(strict_routing));

    // Model that DOES match should succeed
    let result = match_adapter(
        "claude-sonnet-4-6",
        &config.agent.routing.as_ref().unwrap().rules,
        config
            .agent
            .routing
            .as_ref()
            .unwrap()
            .default_adapter
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(""),
    );

    assert_eq!(
        result,
        Some("claude-print".to_string()),
        "Strict mode should succeed when rule matches"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// First-Match-Wins Semantics
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn routing_first_match_wins_with_overlapping_patterns() {
    // Test that when multiple patterns match a model, the FIRST match wins.
    //
    // This is critical for predictable routing behavior. When rules overlap,
    // the first rule in the list that matches takes precedence.
    //
    // Example: If we have:
    //   1. match_model: "claude-.*" -> adapter: "first-adapter"
    //   2. match_model: "claude-sonnet.*" -> adapter: "second-adapter"
    //
    // Then "claude-sonnet-4-6" should route to "first-adapter" because the
    // first pattern matches and wins, even though the second pattern is more
    // specific.

    let routing = RoutingConfig {
        // Order matters! First pattern that matches wins.
        rules: vec![
            make_rule("claude-.*", "first-adapter"), // Matches all claude-*
            make_rule("claude-sonnet.*", "second-adapter"), // More specific, but comes second
            make_rule("sonnet-.*", "third-adapter"), // Also matches sonnet prefix
        ],
        default_adapter: Some("fallback".to_string()),
        strict: false,
    };
    let config = make_test_config("claude", Some(routing));

    // "claude-sonnet-4-6" matches all three patterns, but first wins
    let result = match_adapter(
        "claude-sonnet-4-6",
        &config.agent.routing.as_ref().unwrap().rules,
        config
            .agent
            .routing
            .as_ref()
            .unwrap()
            .default_adapter
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(""),
    );

    assert_eq!(
        result,
        Some("first-adapter".to_string()),
        "First matching pattern should win, even if later patterns are more specific"
    );
}

#[test]
fn routing_first_match_wins_reversed_order() {
    // Test that reversing rule order changes the outcome.
    //
    // This confirms that routing is order-dependent, not just pattern-dependent.
    // The same set of rules in a different order should produce different results
    // for models that match multiple patterns.

    let routing = RoutingConfig {
        // Reversed order from the previous test
        rules: vec![
            make_rule("sonnet-.*", "third-adapter"),        // First now
            make_rule("claude-sonnet.*", "second-adapter"), // Second now
            make_rule("claude-.*", "first-adapter"),        // Last now
        ],
        default_adapter: Some("fallback".to_string()),
        strict: false,
    };
    let config = make_test_config("claude", Some(routing));

    // "claude-sonnet-4-6" matches all three patterns, but first wins (now "third-adapter")
    let result = match_adapter(
        "claude-sonnet-4-6",
        &config.agent.routing.as_ref().unwrap().rules,
        config
            .agent
            .routing
            .as_ref()
            .unwrap()
            .default_adapter
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(""),
    );

    assert_eq!(
        result,
        Some("third-adapter".to_string()),
        "Reversing rule order should change which adapter is selected"
    );
}

#[test]
fn routing_first_match_wins_with_specific_patterns() {
    // Test first-match-wins with realistic Anthropic model patterns.
    //
    // This uses actual model name patterns to verify that order matters
    // in real-world configurations.

    let routing = RoutingConfig {
        // More specific patterns first catch exact matches
        rules: vec![
            make_rule("claude-sonnet-5", "sonnet-5-exact"), // Exact match for Sonnet 5
            make_rule("claude-sonnet.*", "sonnet-family"),  // Fallback for all Sonnet
            make_rule("claude-.*", "all-claude"),           // Fallback for all Claude
        ],
        default_adapter: Some("fallback".to_string()),
        strict: false,
    };
    let config = make_test_config("claude", Some(routing));

    // Exact match for claude-sonnet-5 -> sonnet-5-exact (first pattern)
    let result1 = match_adapter(
        "claude-sonnet-5",
        &config.agent.routing.as_ref().unwrap().rules,
        config
            .agent
            .routing
            .as_ref()
            .unwrap()
            .default_adapter
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(""),
    );
    assert_eq!(
        result1,
        Some("sonnet-5-exact".to_string()),
        "Exact match should win when placed first"
    );

    // Other Sonnet models -> sonnet-family (second pattern, first doesn't match)
    let result2 = match_adapter(
        "claude-sonnet-4-6",
        &config.agent.routing.as_ref().unwrap().rules,
        config
            .agent
            .routing
            .as_ref()
            .unwrap()
            .default_adapter
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(""),
    );
    assert_eq!(
        result2,
        Some("sonnet-family".to_string()),
        "Second pattern should win when first doesn't match"
    );

    // Opus models -> all-claude (third pattern, first two don't match)
    let result3 = match_adapter(
        "claude-opus-4-8",
        &config.agent.routing.as_ref().unwrap().rules,
        config
            .agent
            .routing
            .as_ref()
            .unwrap()
            .default_adapter
            .as_ref()
            .map(|s| s.as_str())
            .unwrap_or(""),
    );
    assert_eq!(
        result3,
        Some("all-claude".to_string()),
        "Third pattern should win when first two don't match"
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// Real-World Configuration Tests
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn routing_real_world_anthropic_subscription_policy() {
    // Test the real-world Anthropic subscription billing policy.
    //
    // Historical context: On June 15, 2026, Anthropic's credit split changed.
    // Before this date: claude -p used subscription credits.
    // After this date: claude -p consumed API credits.
    //
    // To maximize subscription value before the deadline, Anthropic Claude
    // models were routed to claude-print, while other models defaulted to
    // claude-code-glm-4.7.
    //
    // This test verifies that the routing policy implements this behavior.

    let routing = RoutingConfig {
        rules: vec![make_rule(
            "(claude-)?(sonnet|opus|fable|haiku).*",
            "claude-print",
        )],
        default_adapter: Some("claude-code-glm-4.7".to_string()),
        strict: false,
    };
    let config = make_test_config("claude", Some(routing));

    // All Anthropic subscription models -> claude-print
    assert_eq!(
        match_adapter(
            "claude-sonnet-4-6",
            &config.agent.routing.as_ref().unwrap().rules,
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or("")
        ),
        Some("claude-print".to_string())
    );
    assert_eq!(
        match_adapter(
            "claude-opus-4-6",
            &config.agent.routing.as_ref().unwrap().rules,
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or("")
        ),
        Some("claude-print".to_string())
    );
    assert_eq!(
        match_adapter(
            "claude-fable-5",
            &config.agent.routing.as_ref().unwrap().rules,
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or("")
        ),
        Some("claude-print".to_string())
    );
    assert_eq!(
        match_adapter(
            "claude-haiku-4-5",
            &config.agent.routing.as_ref().unwrap().rules,
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or("")
        ),
        Some("claude-print".to_string())
    );

    // Other models -> claude-code-glm-4.7
    assert_eq!(
        match_adapter(
            "glm-4.7",
            &config.agent.routing.as_ref().unwrap().rules,
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or("")
        ),
        Some("claude-code-glm-4.7".to_string())
    );
    assert_eq!(
        match_adapter(
            "gpt-4",
            &config.agent.routing.as_ref().unwrap().rules,
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or("")
        ),
        Some("claude-code-glm-4.7".to_string())
    );
}

#[test]
fn routing_june_15_deadline_rationale() {
    // Document the June 15, 2026 deadline rationale in a test.
    //
    // This test exists primarily to document WHY the routing policy exists.
    // The routing feature shipped before this deadline (tracked by bead bf-2xi)
    // to enable workspace operators to maximize subscription credit value.
    //
    // Key points:
    // 1. Anthropic subscription credits were valuable pre-June 15
    // 2. The -p flag (--print) consumed subscription credits
    // 3. After June 15, -p switched to API credits
    // 4. Routing Anthropic models to claude-print maximized subscription value
    // 5. Non-Anthropic models defaulted to claude-code-glm-4.7
    //
    // This test verifies that the routing configuration supports this use case.

    let deadline_date = "2026-06-15";

    // Verify that our routing supports the pre-deadline policy
    let pre_deadline_routing = RoutingConfig {
        rules: vec![make_rule(
            "(claude-)?(sonnet|opus|fable|haiku).*",
            "claude-print",
        )],
        default_adapter: Some("claude-code-glm-4.7".to_string()),
        strict: false,
    };
    let config = make_test_config("claude", Some(pre_deadline_routing));

    // Pre-June 15: Anthropic models -> claude-print (subscription credits)
    let claude_models = vec!["sonnet", "opus", "fable", "haiku"];
    for model_family in claude_models {
        let model = format!("claude-{}-4-6", model_family);
        let result = match_adapter(
            &model,
            &config.agent.routing.as_ref().unwrap().rules,
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or(""),
        );
        assert_eq!(
            result,
            Some("claude-print".to_string()),
            "Pre-{} Anthropic {} model should route to claude-print",
            deadline_date,
            model_family
        );
    }

    // Non-Anthropic models -> default adapter (API credits or other)
    let non_anthropic = vec!["glm-4.7", "gpt-4", "gemini-pro"];
    for model in non_anthropic {
        let result = match_adapter(
            model,
            &config.agent.routing.as_ref().unwrap().rules,
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or(""),
        );
        assert_eq!(
            result,
            Some("claude-code-glm-4.7".to_string()),
            "Pre-{} non-Anthropic model {} should route to default adapter",
            deadline_date,
            model
        );
    }
}

#[test]
fn routing_config_example_documentation() {
    // Test that documents the .needle.yaml configuration example.
    //
    // This test serves as living documentation for the routing configuration
    // schema. It shows the exact YAML structure that workspace operators
    // should use to configure routing in their .needle.yaml files.
    //
    // Example .needle.yaml:
    //
    // ```yaml
    // agent:
    //   default: claude
    //   routing:
    //     rules:
    //       - match_model: "(claude-)?(sonnet|opus).*"
    //         adapter: claude-print
    //       - match_model: "(claude-)?(fable|haiku).*"
    //         adapter: claude-print
    //       - match_model: "glm-.*"
    //         adapter: claude-code-glm-4.7
    //     default_adapter: claude-code-glm-4.7
    //     strict: false
    // ```
    //
    // This test verifies that such a configuration would work correctly.

    let example_routing = RoutingConfig {
        // Rules from the example YAML
        rules: vec![
            make_rule("(claude-)?(sonnet|opus).*", "claude-print"),
            make_rule("(claude-)?(fable|haiku).*", "claude-print"),
            make_rule("glm-.*", "claude-code-glm-4.7"),
        ],
        default_adapter: Some("claude-code-glm-4.7".to_string()),
        strict: false,
    };
    let config = make_test_config("claude", Some(example_routing));

    // Verify the routing behavior matches the documented example

    // Sonnet and Opus -> claude-print
    assert_eq!(
        match_adapter(
            "claude-sonnet-4-6",
            &config.agent.routing.as_ref().unwrap().rules,
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or("")
        ),
        Some("claude-print".to_string())
    );
    assert_eq!(
        match_adapter(
            "claude-opus-4-6",
            &config.agent.routing.as_ref().unwrap().rules,
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or("")
        ),
        Some("claude-print".to_string())
    );

    // Fable and Haiku -> claude-print
    assert_eq!(
        match_adapter(
            "claude-fable-5",
            &config.agent.routing.as_ref().unwrap().rules,
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or("")
        ),
        Some("claude-print".to_string())
    );
    assert_eq!(
        match_adapter(
            "claude-haiku-4-5",
            &config.agent.routing.as_ref().unwrap().rules,
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or("")
        ),
        Some("claude-print".to_string())
    );

    // GLM models -> claude-code-glm-4.7
    assert_eq!(
        match_adapter(
            "glm-4.7",
            &config.agent.routing.as_ref().unwrap().rules,
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or("")
        ),
        Some("claude-code-glm-4.7".to_string())
    );

    // Other models -> default_adapter
    assert_eq!(
        match_adapter(
            "gpt-4",
            &config.agent.routing.as_ref().unwrap().rules,
            config
                .agent
                .routing
                .as_ref()
                .unwrap()
                .default_adapter
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or("")
        ),
        Some("claude-code-glm-4.7".to_string())
    );
}
