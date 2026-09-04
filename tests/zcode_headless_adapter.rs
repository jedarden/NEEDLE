use std::path::PathBuf;

use needle::dispatch::{AgentAdapter, TimeoutPolicy};
use needle::types::InputMethod;

#[test]
fn shipped_zcode_adapter_matches_the_headless_contract() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("plugins/zcode-headless/zcode-headless.yaml");
    let yaml = std::fs::read_to_string(&path).expect("read shipped ZCode adapter");
    let adapter: AgentAdapter = serde_yaml::from_str(&yaml).expect("parse shipped ZCode adapter");

    assert_eq!(adapter.name, "zcode-headless");
    assert_eq!(adapter.agent_cli, "needle-zcode-headless");
    assert_eq!(adapter.provider.as_deref(), Some("zai"));
    assert_eq!(adapter.model.as_deref(), Some("glm-5.3-flash"));
    assert_eq!(adapter.harness.as_deref(), Some("zcode"));
    assert!(matches!(adapter.input_method, InputMethod::File { .. }));
    assert_eq!(
        adapter.timeout_policy(),
        TimeoutPolicy::New {
            idle_enabled: true,
            hard_enabled: true,
        }
    );

    for required in ["--workspace", "--prompt-file", "--mode yolo"] {
        assert!(
            adapter.invoke_template.contains(required),
            "invoke template omitted {required}"
        );
    }
    assert!(!adapter.invoke_template.contains("| cat"));
    assert!(!adapter.invoke_template.contains("--max-turns"));
    assert!(!adapter.invoke_template.contains("--settings"));
    assert!(adapter.output_transform.is_none());
}
