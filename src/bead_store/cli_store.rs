//! Descriptor-driven bead CLI command engine.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::types::Bead;

use super::{spawn_with_etxtbsy_retry_child, BeadBackend, BeadOperationSpec, ParseShape};

const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// One descriptor-bound CLI store. The descriptor and binary are inseparable.
pub struct CliBeadStore {
    backend: BeadBackend,
    binary: PathBuf,
    workspace: PathBuf,
    model: Option<String>,
    harness: Option<String>,
    harness_version: Option<String>,
}

impl CliBeadStore {
    pub fn new(
        backend: BeadBackend,
        binary: PathBuf,
        workspace: PathBuf,
        model: Option<String>,
        harness: Option<String>,
        harness_version: Option<String>,
    ) -> Result<Self> {
        backend.validate(Path::new("<resolved-backend>"))?;
        if !binary.is_file() {
            bail!("bead backend binary not found at {}", binary.display());
        }
        Ok(Self {
            backend,
            binary,
            workspace,
            model,
            harness,
            harness_version,
        })
    }

    pub fn backend(&self) -> &BeadBackend {
        &self.backend
    }

    pub fn binary(&self) -> &Path {
        &self.binary
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    fn operation(&self, name: &str) -> Result<&BeadOperationSpec> {
        self.backend.operations.get(name).ok_or_else(|| {
            anyhow::anyhow!(
                "backend '{}' does not define operation '{}'",
                self.backend.name,
                name
            )
        })
    }

    /// Render argv, removing an optional flag plus placeholder when its value
    /// is absent. Required embedded placeholders fail explicitly.
    pub fn render_operation(
        &self,
        name: &str,
        values: &HashMap<&str, String>,
    ) -> Result<Vec<String>> {
        let spec = self.operation(name)?;
        let mut argv = Vec::with_capacity(spec.argv.len());
        for template in &spec.argv {
            let names = placeholders(template)?;
            if names.is_empty() {
                argv.push(template.clone());
                continue;
            }
            let mut rendered = template.clone();
            let mut omit = false;
            for placeholder in names {
                let value = self
                    .implicit_value(&placeholder)
                    .or_else(|| values.get(placeholder.as_str()).cloned());
                match value {
                    Some(value) if !value.is_empty() => {
                        rendered = rendered.replace(&format!("{{{placeholder}}}"), &value);
                    }
                    _ if template == &format!("{{{placeholder}}}")
                        && is_optional_placeholder(&placeholder) =>
                    {
                        omit = true;
                    }
                    _ => bail!(
                        "backend '{}' operation '{}' requires placeholder '{{{}}}'",
                        self.backend.name,
                        name,
                        placeholder
                    ),
                }
            }
            if omit {
                if argv
                    .last()
                    .is_some_and(|argument| argument.starts_with('-'))
                {
                    argv.pop();
                }
            } else {
                argv.push(rendered);
            }
        }
        Ok(argv)
    }

    fn implicit_value(&self, name: &str) -> Option<String> {
        match name {
            "model" => self.model.clone(),
            "harness" => self.harness.clone(),
            "harness_version" => self.harness_version.clone(),
            _ => None,
        }
    }

    pub async fn run_operation(
        &self,
        name: &str,
        values: &HashMap<&str, String>,
    ) -> Result<String> {
        let args = self.render_operation(name, values)?;
        let timeout_secs = self
            .operation(name)?
            .timeout_secs
            .unwrap_or(DEFAULT_TIMEOUT_SECS);
        self.run_argv(name, &args, timeout_secs).await
    }

    async fn run_argv(&self, name: &str, args: &[String], timeout_secs: u64) -> Result<String> {
        let binary = self.binary.clone();
        let workspace = self.workspace.clone();
        let owned_args = args.to_vec();
        let child = spawn_with_etxtbsy_retry_child(
            || async {
                let mut command = tokio::process::Command::new(&binary);
                command
                    .args(&owned_args)
                    .current_dir(&workspace)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true);
                command.spawn()
            },
            5,
            20,
        )
        .await
        .with_context(|| {
            format!(
                "failed to spawn backend '{}' operation '{}' using {}",
                self.backend.name,
                name,
                self.binary.display()
            )
        })?;
        let output =
            tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output())
                .await
                .with_context(|| {
                    format!(
                        "backend '{}' operation '{}' timed out after {}s",
                        self.backend.name, name, timeout_secs
                    )
                })?
                .with_context(|| {
                    format!(
                        "backend '{}' operation '{}' failed",
                        self.backend.name, name
                    )
                })?;
        let stdout = String::from_utf8(output.stdout).with_context(|| {
            format!(
                "backend '{}' operation '{}' stdout was not UTF-8",
                self.backend.name, name
            )
        })?;
        if !output.status.success() {
            bail!(
                "backend '{}' operation '{}' exited with code {}: {}",
                self.backend.name,
                name,
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(stdout)
    }

    pub fn parse_beads(&self, name: &str, output: &str) -> Result<Vec<Bead>> {
        let shape = self.operation(name)?.parse.ok_or_else(|| {
            anyhow::anyhow!(
                "backend '{}' operation '{}' has no declared parse shape",
                self.backend.name,
                name
            )
        })?;
        parse_beads(shape, output).with_context(|| {
            format!(
                "failed to parse backend '{}' operation '{}' as {:?}",
                self.backend.name, name, shape
            )
        })
    }
}

fn is_optional_placeholder(name: &str) -> bool {
    matches!(name, "model" | "harness" | "harness_version")
}

fn placeholders(template: &str) -> Result<Vec<String>> {
    let mut names = Vec::new();
    let mut remainder = template;
    while let Some(open) = remainder.find('{') {
        let after_open = &remainder[open + 1..];
        let close = after_open
            .find('}')
            .ok_or_else(|| anyhow::anyhow!("malformed placeholder in {template:?}"))?;
        names.push(after_open[..close].to_string());
        remainder = &after_open[close + 1..];
    }
    if remainder.contains('}') {
        bail!("malformed placeholder in {template:?}");
    }
    Ok(names)
}

fn parse_beads(shape: ParseShape, output: &str) -> Result<Vec<Bead>> {
    if output.trim().is_empty() {
        return Ok(Vec::new());
    }
    match shape {
        ParseShape::JsonArray => serde_json::from_str(output).context("invalid JSON array"),
        ParseShape::JsonObject => {
            if let Ok(bead) = serde_json::from_str::<Bead>(output) {
                return Ok(vec![bead]);
            }
            let beads: Vec<Bead> = serde_json::from_str(output).context("invalid JSON object")?;
            if beads.len() != 1 {
                bail!("expected one JSON object, found {}", beads.len());
            }
            Ok(beads)
        }
        ParseShape::JsonLines => output
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).context("invalid JSON line"))
            .collect(),
        ParseShape::BareId | ParseShape::None => {
            bail!("parse shape {shape:?} cannot produce bead records")
        }
    }
}
