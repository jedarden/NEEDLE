//! Test that default routing configuration uses only built-in adapters.
//!
//! This test verifies that when a fresh HOME (no custom config) loads
//! Config::default(), the routing configuration references only adapter
//! names that exist in the builtin_adapters() list.
//!
//! Run with: cargo test --test default_routing_uses_builtin_adapters

use needle::config::AgentConfig;
use needle::dispatch::builtin_adapters;

#[test]
fn default_routing_uses_only_builtin_adapters() {
    // Get the default agent configuration (fresh HOME, no custom config)
    let default_agent = AgentConfig::default();

    // Collect all built-in adapter names
    let builtin_names: std::collections::HashSet<String> = builtin_adapters()
        .into_iter()
        .map(|adapter| adapter.name)
        .collect();

    // Verify default routing rules reference only built-in adapters
    if let Some(routing) = &default_agent.routing {
        for rule in &routing.rules {
            assert!(
                builtin_names.contains(&rule.adapter),
                "Default routing rule references non-built-in adapter: {}",
                rule.adapter
            );
        }

        // Verify default_adapter references only built-in adapters
        if let Some(ref default_adapter) = routing.default_adapter {
            assert!(
                builtin_names.contains(default_adapter),
                "Default adapter references non-built-in adapter: {}",
                default_adapter
            );
        }
    }
}
