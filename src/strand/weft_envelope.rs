//! Table-driven translation from LOOM's harness-neutral capabilities to NEEDLE adapters.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::dispatch::{self, AgentAdapter};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WeftHarness {
    ClaudePrint,
    Codex,
}

#[derive(Debug, Clone, Default)]
pub struct EnvelopeRequest {
    pub capabilities: Vec<String>,
    pub deny_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslatedEnvelope {
    pub harness: WeftHarness,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub sandbox_mode: Option<String>,
    pub approval_policy: Option<String>,
    pub writable_roots: Vec<String>,
    pub network_access: bool,
    pub settings: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvelopeError {
    pub error_code: &'static str,
    pub retryable: bool,
    pub capability: String,
    pub harness: WeftHarness,
    pub reason: String,
}

impl std::fmt::Display for EnvelopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "capability_unsupported: {} cannot express {} for {:?}",
            self.reason, self.capability, self.harness
        )
    }
}

impl std::error::Error for EnvelopeError {}

#[derive(Clone, Copy)]
struct CapabilityRule {
    name: &'static str,
    claude_tools: &'static [&'static str],
    codex_supported: bool,
}

const RULES: &[CapabilityRule] = &[
    CapabilityRule {
        name: "read",
        claude_tools: &["Read"],
        codex_supported: true,
    },
    CapabilityRule {
        name: "grep",
        claude_tools: &["Grep", "Glob"],
        codex_supported: true,
    },
    CapabilityRule {
        name: "git-readonly",
        claude_tools: &[
            "Bash(git log:*)",
            "Bash(git show:*)",
            "Bash(git diff:*)",
            "Bash(git status:*)",
        ],
        codex_supported: true,
    },
    CapabilityRule {
        name: "kubectl-readonly",
        claude_tools: &[
            "Bash(kubectl get:*)",
            "Bash(kubectl describe:*)",
            "Bash(kubectl logs:*)",
        ],
        codex_supported: true,
    },
    CapabilityRule {
        name: "bead-readonly",
        claude_tools: &["Bash(bead show:*)", "Bash(bead list:*)"],
        codex_supported: true,
    },
    CapabilityRule {
        name: "docs-readonly",
        claude_tools: &[],
        codex_supported: true,
    },
];

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

pub fn harness_for_adapter(adapter: &AgentAdapter) -> Result<WeftHarness, String> {
    let executable = Path::new(adapter.agent_cli.trim())
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(adapter.agent_cli.trim())
        .to_ascii_lowercase();
    if executable == "claude-print" {
        Ok(WeftHarness::ClaudePrint)
    } else if executable == "codex" || executable.starts_with("codex-") {
        Ok(WeftHarness::Codex)
    } else {
        Err(format!(
            "adapter '{}' uses unsupported LOOM harness '{}'",
            adapter.name, adapter.agent_cli
        ))
    }
}

pub fn translate(
    harness: WeftHarness,
    request: &EnvelopeRequest,
) -> Result<TranslatedEnvelope, EnvelopeError> {
    let mut allowed_tools = Vec::new();
    let mut disallowed_tools = Vec::new();

    for capability in &request.capabilities {
        let Some(rule) = RULES.iter().find(|rule| rule.name == capability) else {
            return Err(EnvelopeError {
                error_code: "capability_unsupported",
                retryable: false,
                capability: capability.clone(),
                harness,
                reason: "unknown capability".into(),
            });
        };
        match harness {
            WeftHarness::ClaudePrint => {
                for tool in rule.claude_tools {
                    push_unique(&mut allowed_tools, (*tool).to_string());
                }
            }
            WeftHarness::Codex if rule.codex_supported => {}
            WeftHarness::Codex => {
                return Err(EnvelopeError {
                    error_code: "capability_unsupported",
                    retryable: false,
                    capability: capability.clone(),
                    harness,
                    reason: "capability is not expressible in Codex read-only mode".into(),
                });
            }
        }
    }

    if harness == WeftHarness::Codex && !request.deny_paths.is_empty() {
        return Err(EnvelopeError {
            error_code: "capability_unsupported",
            retryable: false,
            capability: "deny_paths".into(),
            harness,
            reason: "Codex read-only mode cannot enforce per-path read denials".into(),
        });
    }

    if harness == WeftHarness::ClaudePrint {
        for path in &request.deny_paths {
            push_unique(&mut disallowed_tools, format!("Read({path})"));
        }
    }

    let settings = (harness == WeftHarness::ClaudePrint).then(|| {
        json!({
            "permissions": {
                "deny": request.deny_paths.iter().map(|path| format!("Read({path})")).collect::<Vec<_>>()
            }
        })
    });

    Ok(TranslatedEnvelope {
        harness,
        allowed_tools,
        disallowed_tools,
        sandbox_mode: (harness == WeftHarness::Codex).then(|| "read-only".into()),
        approval_policy: (harness == WeftHarness::Codex).then(|| "never".into()),
        writable_roots: Vec::new(),
        network_access: false,
        settings,
    })
}

/// Enforce the subscription-billing rule for Weft Claude turns.
pub fn validate_loom_adapter(adapter: &AgentAdapter) -> Result<(), String> {
    let executable = Path::new(adapter.agent_cli.trim())
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(adapter.agent_cli.trim())
        .to_ascii_lowercase();
    let invoked = executable == "claude"
        || dispatch::extract_executables_from_template(&adapter.invoke_template)
            .iter()
            .any(|name| {
                Path::new(name)
                    .file_name()
                    .and_then(|part| part.to_str())
                    .is_some_and(|part| part.eq_ignore_ascii_case("claude"))
            });
    if !invoked {
        return Ok(());
    }

    let has_headless_flag = adapter
        .invoke_template
        .split(|c: char| c.is_whitespace() || matches!(c, ';' | '|' | '&' | '<' | '>'))
        .any(|token| matches!(token.trim_matches(['"', '\'']), "-p" | "--print"));
    if has_headless_flag {
        Err(format!(
            "adapter '{}' invokes claude with -p/--print; LOOM Claude turns must use claude-print for subscription billing",
            adapter.name
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(capabilities: &[&str]) -> EnvelopeRequest {
        EnvelopeRequest {
            capabilities: capabilities.iter().map(|s| (*s).into()).collect(),
            deny_paths: vec!["~/.ssh/**".into()],
        }
    }

    fn adapter(agent_cli: &str, invoke_template: &str) -> AgentAdapter {
        AgentAdapter {
            name: "test".into(),
            description: None,
            agent_cli: agent_cli.into(),
            version_command: None,
            input_method: crate::types::InputMethod::Stdin,
            invoke_template: invoke_template.into(),
            environment: Default::default(),
            timeout_secs: 0,
            idle_timeout_secs: 0,
            hard_timeout_secs: 0,
            provider: None,
            model: None,
            token_extraction: Default::default(),
            usage_format: None,
            output_transform: None,
            harness: None,
            harness_version: None,
            capabilities: Vec::new(),
        }
    }

    #[test]
    fn claude_print_maps_contract_capabilities_and_deny_settings() {
        let translated = translate(
            WeftHarness::ClaudePrint,
            &request(&[
                "read",
                "grep",
                "git-readonly",
                "kubectl-readonly",
                "bead-readonly",
            ]),
        )
        .unwrap();
        assert_eq!(
            translated.allowed_tools,
            vec![
                "Read",
                "Grep",
                "Glob",
                "Bash(git log:*)",
                "Bash(git show:*)",
                "Bash(git diff:*)",
                "Bash(git status:*)",
                "Bash(kubectl get:*)",
                "Bash(kubectl describe:*)",
                "Bash(kubectl logs:*)",
                "Bash(bead show:*)",
                "Bash(bead list:*)",
            ]
        );
        assert_eq!(translated.disallowed_tools, vec!["Read(~/.ssh/**)"]);
        assert_eq!(
            translated.settings.unwrap()["permissions"]["deny"][0],
            "Read(~/.ssh/**)"
        );
    }

    #[test]
    fn docs_readonly_is_an_explicit_noop_row() {
        let mut input = request(&["docs-readonly"]);
        input.deny_paths.clear();
        let translated = translate(WeftHarness::ClaudePrint, &input).unwrap();
        assert!(translated.allowed_tools.is_empty());
        assert!(translated.disallowed_tools.is_empty());
    }

    #[test]
    fn codex_uses_read_only_sandbox_approval_and_no_writable_network_scope() {
        let mut input = request(&[
            "read",
            "grep",
            "git-readonly",
            "bead-readonly",
            "docs-readonly",
        ]);
        input.deny_paths.clear();
        let translated = translate(WeftHarness::Codex, &input).unwrap();
        assert_eq!(translated.sandbox_mode.as_deref(), Some("read-only"));
        assert_eq!(translated.approval_policy.as_deref(), Some("never"));
        assert!(translated.writable_roots.is_empty());
        assert!(!translated.network_access);
        assert!(translated.allowed_tools.is_empty());
    }

    #[test]
    fn unsupported_capabilities_fail_closed() {
        let mut input = request(&["web-fetch"]);
        input.deny_paths.clear();
        let error = translate(WeftHarness::Codex, &input).unwrap_err();
        assert_eq!(error.capability, "web-fetch");
        assert_eq!(error.error_code, "capability_unsupported");
        assert!(!error.retryable);

        input.capabilities = vec!["shell-admin".into()];
        let error = translate(WeftHarness::Codex, &input).unwrap_err();
        assert_eq!(error.capability, "shell-admin");
    }

    #[test]
    fn codex_rejects_any_deny_path_it_cannot_express() {
        let input = request(&["read"]);
        let error = translate(WeftHarness::Codex, &input).unwrap_err();
        assert_eq!(error.capability, "deny_paths");
        assert_eq!(error.error_code, "capability_unsupported");
        assert!(!error.retryable);
    }

    #[test]
    fn loom_adapter_lint_rejects_claude_print_but_accepts_subscription_wrapper() {
        assert!(validate_loom_adapter(&adapter("claude", "claude -p --model {model}")).is_err());
        assert!(validate_loom_adapter(&adapter("claude", "claude --print")).is_err());
        assert!(
            validate_loom_adapter(&adapter("claude-print", "claude-print --model {model}")).is_ok()
        );
        assert!(validate_loom_adapter(&adapter("codex", "codex exec {prompt_file}")).is_ok());

        // Exercise the real serve_strands validator so an unsafe adapter cannot
        // bypass the lint through config loading.
        let directory = tempfile::tempdir().unwrap();
        let adapter_path = directory.path().join("unsafe.yaml");
        std::fs::write(
            &adapter_path,
            "name: unsafe\nagent_cli: claude\ninvoke_template: claude --print {prompt_file}\n",
        )
        .unwrap();
        let mut config = crate::config::Config::default();
        config.agent.adapters_dir = directory.path().to_path_buf();
        config.loom.serve_strands.insert(
            "advisor".into(),
            crate::config::LoomServeStrand {
                adapter: "unsafe".into(),
                model: Some("model".into()),
            },
        );
        assert!(crate::config::ConfigLoader::validate(&config)
            .iter()
            .any(
                |error| error.full_path == "loom.serve_strands.advisor.adapter"
                    && error.message.contains("claude with -p/--print")
            ));
    }
}
