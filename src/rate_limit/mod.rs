//! Provider/model concurrency limits and RPM rate limiting.
//!
//! Before dispatching an agent, the worker checks:
//! 1. **Concurrency limits**: Are fewer than `max_concurrent` workers using
//!    this provider/model? Checked via the worker registry.
//! 2. **RPM limits**: Has the provider's requests-per-minute budget been
//!    exceeded? Checked via a file-based token bucket.
//!
//! If either check fails, the caller should back off and retry.
//!
//! Depends on: `config`, `registry`, `types`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::config::LimitsConfig;
use crate::registry::Registry;

// ──────────────────────────────────────────────────────────────────────────────
// RateLimitDecision
// ──────────────────────────────────────────────────────────────────────────────

/// Result of a rate limit check.
#[derive(Debug, Clone, PartialEq)]
pub enum RateLimitDecision {
    /// Dispatch is allowed.
    Allowed,
    /// Provider concurrency limit reached.
    ProviderConcurrencyExceeded {
        provider: String,
        current: u32,
        limit: u32,
    },
    /// Model concurrency limit reached.
    ModelConcurrencyExceeded {
        model: String,
        current: u32,
        limit: u32,
    },
    /// RPM limit reached for this provider.
    RpmExceeded {
        provider: String,
        requests_per_minute: u32,
    },
}

impl RateLimitDecision {
    /// Whether dispatch is allowed.
    pub fn is_allowed(&self) -> bool {
        matches!(self, RateLimitDecision::Allowed)
    }
}

impl std::fmt::Display for RateLimitDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RateLimitDecision::Allowed => write!(f, "allowed"),
            RateLimitDecision::ProviderConcurrencyExceeded {
                provider,
                current,
                limit,
            } => write!(
                f,
                "provider '{}' concurrency exceeded ({}/{})",
                provider, current, limit
            ),
            RateLimitDecision::ModelConcurrencyExceeded {
                model,
                current,
                limit,
            } => write!(
                f,
                "model '{}' concurrency exceeded ({}/{})",
                model, current, limit
            ),
            RateLimitDecision::RpmExceeded {
                provider,
                requests_per_minute,
            } => write!(
                f,
                "provider '{}' RPM limit exceeded ({}/min)",
                provider, requests_per_minute
            ),
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Token Bucket (file-based)
// ──────────────────────────────────────────────────────────────────────────────

/// On-disk token bucket state for RPM limiting.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TokenBucket {
    /// Current available tokens (fractional for smooth refill).
    tokens: f64,
    /// Maximum tokens (= requests_per_minute).
    capacity: u32,
    /// Last time tokens were refilled.
    last_refill: DateTime<Utc>,
}

impl TokenBucket {
    fn new(capacity: u32) -> Self {
        TokenBucket {
            tokens: capacity as f64,
            capacity,
            last_refill: Utc::now(),
        }
    }

    /// Refill tokens based on elapsed time since last refill.
    fn refill(&mut self) {
        let now = Utc::now();
        let elapsed_secs = (now - self.last_refill).num_milliseconds().max(0) as f64 / 1000.0;
        let refill_rate = self.capacity as f64 / 60.0; // tokens per second
        self.tokens = (self.tokens + elapsed_secs * refill_rate).min(self.capacity as f64);
        self.last_refill = now;
    }

    /// Try to consume one token. Returns true if successful.
    fn try_consume(&mut self) -> bool {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// RateLimiter
// ──────────────────────────────────────────────────────────────────────────────

/// Enforces provider/model concurrency limits and RPM rate limiting.
pub struct RateLimiter {
    config: LimitsConfig,
    /// Directory for token bucket state files (`~/.needle/state/rate_limits/`).
    state_dir: PathBuf,
}

impl RateLimiter {
    /// Create a rate limiter with the given config and state directory.
    pub fn new(config: LimitsConfig, state_dir: &Path) -> Self {
        RateLimiter {
            config,
            state_dir: state_dir.join("rate_limits"),
        }
    }

    /// Check all rate limits before dispatching.
    ///
    /// Returns `Allowed` if dispatch can proceed, or a specific reason why not.
    /// The caller should back off and retry if not allowed.
    pub fn check(
        &self,
        provider: Option<&str>,
        model: Option<&str>,
        registry: &Registry,
    ) -> Result<RateLimitDecision> {
        // Check provider concurrency.
        if let Some(provider_name) = provider {
            if let Some(provider_limits) = self.config.providers.get(provider_name) {
                if let Some(max_concurrent) = provider_limits.max_concurrent {
                    let decision =
                        self.check_provider_concurrency(provider_name, max_concurrent, registry)?;
                    if !decision.is_allowed() {
                        return Ok(decision);
                    }
                }
            }
        }

        // Check model concurrency.
        if let Some(model_name) = model {
            if let Some(model_limits) = self.config.models.get(model_name) {
                if let Some(max_concurrent) = model_limits.max_concurrent {
                    let decision =
                        self.check_model_concurrency(model_name, max_concurrent, registry)?;
                    if !decision.is_allowed() {
                        return Ok(decision);
                    }
                }
            }
        }

        // Check RPM.
        if let Some(provider_name) = provider {
            if let Some(provider_limits) = self.config.providers.get(provider_name) {
                if let Some(rpm) = provider_limits.requests_per_minute {
                    let decision = self.check_rpm(provider_name, rpm)?;
                    if !decision.is_allowed() {
                        return Ok(decision);
                    }
                }
            }
        }

        Ok(RateLimitDecision::Allowed)
    }

    /// Check provider concurrency against the worker registry.
    fn check_provider_concurrency(
        &self,
        provider: &str,
        max_concurrent: u32,
        registry: &Registry,
    ) -> Result<RateLimitDecision> {
        let workers = registry.list()?;
        let active_count = workers
            .iter()
            .filter(|w| w.provider.as_deref() == Some(provider))
            .count() as u32;

        if active_count >= max_concurrent {
            Ok(RateLimitDecision::ProviderConcurrencyExceeded {
                provider: provider.to_string(),
                current: active_count,
                limit: max_concurrent,
            })
        } else {
            Ok(RateLimitDecision::Allowed)
        }
    }

    /// Check model concurrency against the worker registry.
    fn check_model_concurrency(
        &self,
        model: &str,
        max_concurrent: u32,
        registry: &Registry,
    ) -> Result<RateLimitDecision> {
        let workers = registry.list()?;
        let active_count = workers
            .iter()
            .filter(|w| w.model.as_deref() == Some(model))
            .count() as u32;

        if active_count >= max_concurrent {
            Ok(RateLimitDecision::ModelConcurrencyExceeded {
                model: model.to_string(),
                current: active_count,
                limit: max_concurrent,
            })
        } else {
            Ok(RateLimitDecision::Allowed)
        }
    }

    /// Check RPM via a file-based token bucket.
    fn check_rpm(&self, provider: &str, rpm: u32) -> Result<RateLimitDecision> {
        if rpm == 0 {
            return Ok(RateLimitDecision::Allowed);
        }

        let allowed = self.try_acquire_rpm_token(provider, rpm)?;
        if allowed {
            Ok(RateLimitDecision::Allowed)
        } else {
            Ok(RateLimitDecision::RpmExceeded {
                provider: provider.to_string(),
                requests_per_minute: rpm,
            })
        }
    }

    /// Try to acquire an RPM token from the file-based token bucket.
    ///
    /// Uses flock for cross-process safety.
    fn try_acquire_rpm_token(&self, provider: &str, rpm: u32) -> Result<bool> {
        // Ensure the rate_limits directory exists.
        std::fs::create_dir_all(&self.state_dir).with_context(|| {
            format!(
                "failed to create rate_limits directory: {}",
                self.state_dir.display()
            )
        })?;

        let bucket_path = self.state_dir.join(format!("{provider}.json"));

        // Open or create the bucket file.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&bucket_path)
            .with_context(|| {
                format!(
                    "failed to open rate limit bucket: {}",
                    bucket_path.display()
                )
            })?;

        // Exclusive lock for atomic read-modify-write.
        FileExt::lock_exclusive(&file).with_context(|| {
            format!(
                "failed to acquire lock on rate limit bucket: {}",
                bucket_path.display()
            )
        })?;

        // Read current state.
        let content = std::fs::read_to_string(&bucket_path).unwrap_or_default();
        let mut bucket: TokenBucket = if content.trim().is_empty() {
            TokenBucket::new(rpm)
        } else {
            serde_json::from_str(&content).unwrap_or_else(|_| TokenBucket::new(rpm))
        };

        // Update capacity if config changed.
        bucket.capacity = rpm;

        // Try to consume a token.
        let allowed = bucket.try_consume();

        // Write updated state.
        let json = serde_json::to_string_pretty(&bucket)
            .context("failed to serialize token bucket state")?;
        std::fs::write(&bucket_path, &json).with_context(|| {
            format!(
                "failed to write rate limit bucket: {}",
                bucket_path.display()
            )
        })?;

        // Release lock.
        FileExt::unlock(&file).with_context(|| {
            format!(
                "failed to release lock on rate limit bucket: {}",
                bucket_path.display()
            )
        })?;

        Ok(allowed)
    }

    /// Check system resource health (CPU load and memory).
    ///
    /// Returns warnings as log messages; does not block dispatch.
    pub fn check_system_resources(cpu_load_warn: f64, memory_free_warn_mb: u64) {
        // CPU load: read /proc/loadavg on Linux.
        if let Ok(loadavg) = std::fs::read_to_string("/proc/loadavg") {
            if let Some(load_str) = loadavg.split_whitespace().next() {
                if let Ok(load) = load_str.parse::<f64>() {
                    // Normalize by number of CPUs.
                    let num_cpus = std::thread::available_parallelism()
                        .map(|n| n.get() as f64)
                        .unwrap_or(1.0);
                    let normalized = load / num_cpus;
                    if normalized > cpu_load_warn {
                        tracing::warn!(
                            load_1min = %load_str,
                            normalized = %format!("{:.2}", normalized),
                            threshold = %format!("{:.2}", cpu_load_warn),
                            "CPU load exceeds warning threshold"
                        );
                    }
                }
            }
        }

        // Memory: read /proc/meminfo on Linux.
        if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
            let mut mem_available_kb: Option<u64> = None;
            for line in meminfo.lines() {
                if line.starts_with("MemAvailable:") {
                    if let Some(val) = line.split_whitespace().nth(1) {
                        mem_available_kb = val.parse().ok();
                    }
                    break;
                }
            }
            if let Some(avail_kb) = mem_available_kb {
                let avail_mb = avail_kb / 1024;
                if avail_mb < memory_free_warn_mb {
                    tracing::warn!(
                        available_mb = avail_mb,
                        threshold_mb = memory_free_warn_mb,
                        "available memory below warning threshold"
                    );
                }
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{LimitsConfig, ModelLimits, ProviderLimits};
    use crate::registry::{Registry, WorkerEntry};
    use chrono::Utc;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn make_entry(id: &str, provider: Option<&str>, model: Option<&str>) -> WorkerEntry {
        WorkerEntry {
            id: id.to_string(),
            pid: std::process::id(),
            workspace: PathBuf::from("/tmp/test"),
            agent: "claude".to_string(),
            model: model.map(|s| s.to_string()),
            provider: provider.map(|s| s.to_string()),
            started_at: Utc::now(),
            beads_processed: 0,
        }
    }

    fn make_limits(
        providers: Vec<(&str, Option<u32>, Option<u32>)>,
        models: Vec<(&str, Option<u32>)>,
    ) -> LimitsConfig {
        let mut provider_map = BTreeMap::new();
        for (name, max_concurrent, rpm) in providers {
            provider_map.insert(
                name.to_string(),
                ProviderLimits {
                    max_concurrent,
                    requests_per_minute: rpm,
                },
            );
        }
        let mut model_map = BTreeMap::new();
        for (name, max_concurrent) in models {
            model_map.insert(name.to_string(), ModelLimits { max_concurrent });
        }
        LimitsConfig {
            providers: provider_map,
            models: model_map,
        }
    }

    #[test]
    fn no_limits_configured_allows_all() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new(dir.path());
        let limiter = RateLimiter::new(LimitsConfig::default(), dir.path());

        let decision = limiter
            .check(Some("anthropic"), Some("claude-sonnet"), &registry)
            .unwrap();
        assert_eq!(decision, RateLimitDecision::Allowed);
    }

    #[test]
    fn provider_concurrency_below_limit_allows() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new(dir.path());
        registry
            .register(make_entry("w1", Some("anthropic"), Some("sonnet")))
            .unwrap();

        let limits = make_limits(vec![("anthropic", Some(2), None)], vec![]);
        let limiter = RateLimiter::new(limits, dir.path());

        let decision = limiter.check(Some("anthropic"), None, &registry).unwrap();
        assert_eq!(decision, RateLimitDecision::Allowed);
    }

    #[test]
    fn provider_concurrency_at_limit_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new(dir.path());
        registry
            .register(make_entry("w1", Some("anthropic"), Some("sonnet")))
            .unwrap();
        registry
            .register(make_entry("w2", Some("anthropic"), Some("opus")))
            .unwrap();

        let limits = make_limits(vec![("anthropic", Some(2), None)], vec![]);
        let limiter = RateLimiter::new(limits, dir.path());

        let decision = limiter.check(Some("anthropic"), None, &registry).unwrap();
        assert!(matches!(
            decision,
            RateLimitDecision::ProviderConcurrencyExceeded {
                current: 2,
                limit: 2,
                ..
            }
        ));
    }

    #[test]
    fn different_provider_not_counted() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new(dir.path());
        registry
            .register(make_entry("w1", Some("openai"), Some("gpt4")))
            .unwrap();
        registry
            .register(make_entry("w2", Some("openai"), Some("gpt4")))
            .unwrap();

        let limits = make_limits(vec![("anthropic", Some(2), None)], vec![]);
        let limiter = RateLimiter::new(limits, dir.path());

        let decision = limiter.check(Some("anthropic"), None, &registry).unwrap();
        assert_eq!(decision, RateLimitDecision::Allowed);
    }

    #[test]
    fn model_concurrency_at_limit_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new(dir.path());
        registry
            .register(make_entry("w1", Some("anthropic"), Some("claude-opus")))
            .unwrap();
        registry
            .register(make_entry("w2", Some("anthropic"), Some("claude-opus")))
            .unwrap();
        registry
            .register(make_entry("w3", Some("anthropic"), Some("claude-opus")))
            .unwrap();

        let limits = make_limits(vec![], vec![("claude-opus", Some(3))]);
        let limiter = RateLimiter::new(limits, dir.path());

        let decision = limiter
            .check(Some("anthropic"), Some("claude-opus"), &registry)
            .unwrap();
        assert!(matches!(
            decision,
            RateLimitDecision::ModelConcurrencyExceeded {
                current: 3,
                limit: 3,
                ..
            }
        ));
    }

    #[test]
    fn model_concurrency_below_limit_allows() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new(dir.path());
        registry
            .register(make_entry("w1", Some("anthropic"), Some("claude-opus")))
            .unwrap();

        let limits = make_limits(vec![], vec![("claude-opus", Some(3))]);
        let limiter = RateLimiter::new(limits, dir.path());

        let decision = limiter
            .check(Some("anthropic"), Some("claude-opus"), &registry)
            .unwrap();
        assert_eq!(decision, RateLimitDecision::Allowed);
    }

    #[test]
    fn rpm_limit_allows_first_request() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new(dir.path());
        let limits = make_limits(vec![("anthropic", None, Some(60))], vec![]);
        let limiter = RateLimiter::new(limits, dir.path());

        let decision = limiter.check(Some("anthropic"), None, &registry).unwrap();
        assert_eq!(decision, RateLimitDecision::Allowed);
    }

    #[test]
    fn rpm_limit_exhausts_bucket() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new(dir.path());
        // Very low RPM to exhaust quickly.
        let limits = make_limits(vec![("anthropic", None, Some(2))], vec![]);
        let limiter = RateLimiter::new(limits, dir.path());

        // First two should succeed (bucket starts full at capacity=2).
        let d1 = limiter.check(Some("anthropic"), None, &registry).unwrap();
        assert_eq!(d1, RateLimitDecision::Allowed);

        let d2 = limiter.check(Some("anthropic"), None, &registry).unwrap();
        assert_eq!(d2, RateLimitDecision::Allowed);

        // Third should be rate limited.
        let d3 = limiter.check(Some("anthropic"), None, &registry).unwrap();
        assert!(matches!(d3, RateLimitDecision::RpmExceeded { .. }));
    }

    #[test]
    fn provider_checked_before_model() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new(dir.path());
        registry
            .register(make_entry("w1", Some("anthropic"), Some("claude-opus")))
            .unwrap();
        registry
            .register(make_entry("w2", Some("anthropic"), Some("claude-sonnet")))
            .unwrap();

        // Provider limit of 2 exceeded, model limit of 3 would allow.
        let limits = make_limits(
            vec![("anthropic", Some(2), None)],
            vec![("claude-opus", Some(3))],
        );
        let limiter = RateLimiter::new(limits, dir.path());

        let decision = limiter
            .check(Some("anthropic"), Some("claude-opus"), &registry)
            .unwrap();
        // Provider check fails first.
        assert!(matches!(
            decision,
            RateLimitDecision::ProviderConcurrencyExceeded { .. }
        ));
    }

    #[test]
    fn no_provider_or_model_allows() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new(dir.path());
        let limits = make_limits(vec![("anthropic", Some(1), Some(10))], vec![]);
        let limiter = RateLimiter::new(limits, dir.path());

        // No provider/model info → no limits to check.
        let decision = limiter.check(None, None, &registry).unwrap();
        assert_eq!(decision, RateLimitDecision::Allowed);
    }

    #[test]
    fn decision_display() {
        assert_eq!(format!("{}", RateLimitDecision::Allowed), "allowed");
        assert!(format!(
            "{}",
            RateLimitDecision::ProviderConcurrencyExceeded {
                provider: "anthropic".to_string(),
                current: 5,
                limit: 3,
            }
        )
        .contains("anthropic"));
    }

    #[test]
    fn token_bucket_refill() {
        let mut bucket = TokenBucket::new(60);
        bucket.tokens = 0.0;
        // Simulate 1 second elapsed by manually setting last_refill.
        bucket.last_refill = Utc::now() - chrono::Duration::seconds(1);
        bucket.refill();
        // 60 RPM = 1 token/sec, so after 1 second we should have ~1 token.
        assert!(
            bucket.tokens >= 0.9,
            "tokens should have refilled: {}",
            bucket.tokens
        );
        assert!(
            bucket.tokens <= 1.1,
            "tokens should be ~1: {}",
            bucket.tokens
        );
    }

    #[test]
    fn token_bucket_does_not_exceed_capacity() {
        let mut bucket = TokenBucket::new(10);
        bucket.tokens = 10.0;
        // Even after refill, should not exceed capacity.
        bucket.last_refill = Utc::now() - chrono::Duration::seconds(120);
        bucket.refill();
        assert_eq!(bucket.tokens, 10.0);
    }

    #[test]
    fn system_resource_check_does_not_panic() {
        // Smoke test: should not panic on any platform.
        RateLimiter::check_system_resources(0.8, 512);
    }
}
