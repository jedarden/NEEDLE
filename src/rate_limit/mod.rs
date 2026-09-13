//! Provider/model concurrency limits, RPM rate limiting, and host admission.
//!
//! Before dispatching an agent, the worker checks:
//! 1. **Concurrency limits**: Are fewer than `max_concurrent` executing
//!    workers using this provider/model? Checked via the worker registry.
//! 2. **RPM limits**: Has the provider's requests-per-minute budget been
//!    exceeded? Checked via a file-based token bucket.
//! 3. **Host admission**: Are CPU load and available memory inside the launch
//!    policy? A saturated host produces a typed [`AdmissionDecision::Blocked`]
//!    rather than an opaque error, so the caller can enter a first-class
//!    `ADMISSION_BLOCKED` state and hold there instead of exiting.
//!
//! If either limit check fails, the caller should back off and retry.
//!
//! Depends on: `config`, `registry`, `types`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::config::LimitsConfig;
use crate::registry::Registry;
use crate::telemetry::{EventKind, Telemetry};
use crate::types::WorkerState;

// ──────────────────────────────────────────────────────────────────────────────
// Host admission — typed launch deferral
// ──────────────────────────────────────────────────────────────────────────────

/// Where host resource readings come from.
///
/// Production reads `/proc`. Tests and the saturated-host regression fixture
/// point `NEEDLE_LAUNCH_RESOURCE_PROBE` at a directory containing `loadavg`
/// and `meminfo` files they can rewrite mid-run, which is how a fixture flips
/// a live worker from blocked to admitted without killing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceProbe {
    loadavg_path: PathBuf,
    meminfo_path: PathBuf,
}

impl ResourceProbe {
    /// Probe the real host.
    pub fn system() -> Self {
        ResourceProbe {
            loadavg_path: PathBuf::from("/proc/loadavg"),
            meminfo_path: PathBuf::from("/proc/meminfo"),
        }
    }

    /// Probe a directory of mock `loadavg`/`meminfo` files.
    pub fn from_dir(dir: &Path) -> Self {
        ResourceProbe {
            loadavg_path: dir.join("loadavg"),
            meminfo_path: dir.join("meminfo"),
        }
    }

    /// Resolve the probe: the `NEEDLE_LAUNCH_RESOURCE_PROBE` directory when
    /// set, the real host otherwise.
    pub fn from_env() -> Self {
        match std::env::var("NEEDLE_LAUNCH_RESOURCE_PROBE") {
            Ok(dir) if !dir.is_empty() => ResourceProbe::from_dir(Path::new(&dir)),
            _ => ResourceProbe::system(),
        }
    }

    fn loadavg_path(&self) -> &Path {
        &self.loadavg_path
    }

    fn meminfo_path(&self) -> &Path {
        &self.meminfo_path
    }
}

/// One reading of the host's CPU load and available memory.
///
/// A field is `None` when its source file is unreadable or unparsable — a
/// missing reading must not look like a zero (which would read as "no memory
/// available" and block forever) and must not fail admission either.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResourceSnapshot {
    /// 1-minute load average.
    pub load_1min: Option<f64>,
    /// Usable core count (`available_parallelism`).
    pub cores: usize,
    /// Available memory in MB.
    pub available_mb: Option<u64>,
}

impl ResourceSnapshot {
    /// Normalized CPU load (load / cores).
    pub fn normalized_load(&self) -> Option<f64> {
        self.load_1min.map(|load| load / self.cores.max(1) as f64)
    }
}

/// Launch admission thresholds (mirrors `worker.cpu_load_warn` and
/// `worker.memory_free_warn_mb`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdmissionThresholds {
    /// Normalized CPU load above which launch is deferred.
    pub cpu_load_warn: f64,
    /// Available memory (MB) below which launch is deferred.
    pub memory_free_warn_mb: u64,
}

impl AdmissionThresholds {
    pub fn new(cpu_load_warn: f64, memory_free_warn_mb: u64) -> Self {
        AdmissionThresholds {
            cpu_load_warn,
            memory_free_warn_mb,
        }
    }
}

/// The resource whose policy blocked launch.
#[derive(Debug, Clone, PartialEq)]
pub enum AdmissionResource {
    /// Normalized CPU load over threshold.
    Cpu {
        /// 1-minute load average, raw.
        load: f64,
        /// Core count used for normalization.
        cores: usize,
    },
    /// Available memory under threshold.
    Memory {
        /// Available memory in MB.
        available_mb: u64,
    },
}

impl std::fmt::Display for AdmissionResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdmissionResource::Cpu { .. } => write!(f, "cpu"),
            AdmissionResource::Memory { .. } => write!(f, "memory"),
        }
    }
}

/// A typed launch block: what resource, what was observed, what the policy is.
///
/// `actual` and `threshold` are carried as formatted strings so telemetry and
/// heartbeat consumers can render the pair without re-deriving normalization
/// rules. The structured fields stay on [`AdmissionResource`] for callers that
/// want numbers.
#[derive(Debug, Clone, PartialEq)]
pub struct AdmissionBlock {
    /// The resource whose policy was violated.
    pub resource: AdmissionResource,
    /// Observed value (normalized load, or available MB).
    pub actual: String,
    /// The configured threshold the observed value crossed.
    pub threshold: String,
    /// One-line human-readable reason.
    pub reason: String,
}

/// Result of a launch admission check.
#[derive(Debug, Clone, PartialEq)]
pub enum AdmissionDecision {
    /// Host is inside policy — launch may proceed.
    Admitted,
    /// Host is saturated — the caller must hold, not exit.
    Blocked(AdmissionBlock),
}

impl AdmissionDecision {
    /// Whether launch may proceed.
    pub fn is_admitted(&self) -> bool {
        matches!(self, AdmissionDecision::Admitted)
    }

    /// The block, when admission was denied.
    pub fn block(&self) -> Option<&AdmissionBlock> {
        match self {
            AdmissionDecision::Admitted => None,
            AdmissionDecision::Blocked(block) => Some(block),
        }
    }
}

/// Read the host's resource snapshot from `probe`.
///
/// Unreadable or unparsable probe files yield `None` readings rather than
/// errors: admission is a policy gate over *known* values, and a probe that
/// cannot be read (a mock file a fixture deleted mid-run, a transient `/proc`
/// read failure) must not by itself block a worker forever.
pub fn read_resource_snapshot(probe: &ResourceProbe) -> ResourceSnapshot {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    let load_1min = std::fs::read_to_string(probe.loadavg_path())
        .ok()
        .and_then(|loadavg| {
            loadavg
                .split_whitespace()
                .next()
                .and_then(|s| s.parse().ok())
        });

    let available_mb = std::fs::read_to_string(probe.meminfo_path())
        .ok()
        .and_then(|meminfo| {
            meminfo.lines().find_map(|line| {
                line.starts_with("MemAvailable:")
                    .then(|| line.split_whitespace().nth(1))
                    .flatten()
                    .and_then(|val| val.parse::<u64>().ok())
                    .map(|kb| kb / 1024)
            })
        });

    ResourceSnapshot {
        load_1min,
        cores,
        available_mb,
    }
}

/// Decide whether launch is admitted under the current host snapshot.
///
/// CPU is checked first (it is the threshold the 2026-09-03 lab incident
/// tripped), then memory. Readings the probe could not produce admit rather
/// than block — see [`read_resource_snapshot`].
pub fn check_launch_admission(
    thresholds: AdmissionThresholds,
    snapshot: ResourceSnapshot,
) -> AdmissionDecision {
    // Explicit opt-out for callers that must launch regardless of host load.
    //
    // `cpu_load_warn` is a normalized ratio capped at 1.0 by config validation, so
    // it cannot express "never defer": any box with load above its core count trips
    // the gate. That is correct for a fleet worker and wrong for a test harness
    // spawning one -- the test's own timeout expires inside the wait, reporting only
    // a hang. It is also a reasonable deliberate operator override when launching
    // onto a busy box.
    if std::env::var("NEEDLE_SKIP_LAUNCH_RESOURCE_CHECK").as_deref() == Ok("1") {
        tracing::debug!("NEEDLE_SKIP_LAUNCH_RESOURCE_CHECK=1: skipping pre-launch resource gate");
        return AdmissionDecision::Admitted;
    }

    if let Some(normalized) = snapshot.normalized_load() {
        if normalized > thresholds.cpu_load_warn {
            let load = snapshot.load_1min.unwrap_or_default();
            return AdmissionDecision::Blocked(AdmissionBlock {
                resource: AdmissionResource::Cpu {
                    load,
                    cores: snapshot.cores,
                },
                actual: format!("{normalized:.2}"),
                threshold: format!("{:.2}", thresholds.cpu_load_warn),
                reason: format!(
                    "CPU load saturated: {load:.2} (1-minute average) / {} cores = {normalized:.2} > threshold {:.2}",
                    snapshot.cores, thresholds.cpu_load_warn
                ),
            });
        }
    }

    if let Some(available_mb) = snapshot.available_mb {
        if available_mb < thresholds.memory_free_warn_mb {
            return AdmissionDecision::Blocked(AdmissionBlock {
                resource: AdmissionResource::Memory { available_mb },
                actual: format!("{available_mb} MB"),
                threshold: format!("{} MB", thresholds.memory_free_warn_mb),
                reason: format!(
                    "Memory saturated: {available_mb} MB available < {} MB threshold",
                    thresholds.memory_free_warn_mb
                ),
            });
        }
    }

    AdmissionDecision::Admitted
}

// ──────────────────────────────────────────────────────────────────────────────
// Admission hold — resident blocked state (plan revision 24 §4.6, N-T33)
// ──────────────────────────────────────────────────────────────────────────────

/// Backoff and bounded-emission policy for an admission hold.
///
/// Defaults put a retry every 5–30 s and a `worker.admission_blocked` event at
/// most every heartbeat interval, so a saturated host produces a handful of
/// events per hour instead of the 2026-09-03 pattern of four deferral events
/// per 30-second restart cycle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdmissionHoldConfig {
    /// First retry delay. Doubles per attempt up to [`max_backoff`].
    pub base_backoff: Duration,
    /// Retry-delay ceiling. The cap is what keeps a blocked worker resident
    /// instead of letting its retry cadence grow without bound.
    pub max_backoff: Duration,
    /// Minimum spacing between `worker.admission_blocked` emissions.
    pub heartbeat_interval: Duration,
}

impl Default for AdmissionHoldConfig {
    fn default() -> Self {
        AdmissionHoldConfig {
            base_backoff: Duration::from_secs(5),
            max_backoff: Duration::from_secs(30),
            heartbeat_interval: Duration::from_secs(30),
        }
    }
}

impl AdmissionHoldConfig {
    /// Resolve the hold policy from the environment, falling back to the
    /// defaults.
    ///
    /// `NEEDLE_ADMISSION_BACKOFF_BASE_MS` and
    /// `NEEDLE_ADMISSION_BACKOFF_CAP_MS` size the retry backoff;
    /// `NEEDLE_ADMISSION_HEARTBEAT_SECS` sizes the bounded blocked-event
    /// spacing. These are operator/test seams (like
    /// `NEEDLE_LAUNCH_RESOURCE_PROBE`): production should run on the defaults.
    pub fn from_env() -> Self {
        let mut config = AdmissionHoldConfig::default();

        if let Ok(ms) = std::env::var("NEEDLE_ADMISSION_BACKOFF_BASE_MS") {
            if let Ok(ms) = ms.parse::<u64>() {
                config.base_backoff = Duration::from_millis(ms.max(1));
            }
        }
        if let Ok(ms) = std::env::var("NEEDLE_ADMISSION_BACKOFF_CAP_MS") {
            if let Ok(ms) = ms.parse::<u64>() {
                config.max_backoff =
                    Duration::from_millis(ms.max(config.base_backoff.as_millis() as u64));
            }
        }
        if let Ok(secs) = std::env::var("NEEDLE_ADMISSION_HEARTBEAT_SECS") {
            if let Ok(secs) = secs.parse::<u64>() {
                config.heartbeat_interval = Duration::from_secs(secs.max(1));
            }
        }

        config
    }
}

/// Capped, jittered retry delay for the given attempt (1-based).
///
/// Exponential growth from `base` capped at `max`, with ±25% jitter so a fleet
/// of blocked workers does not retry in lockstep and stampede the host the
/// moment load dips.
pub fn admission_backoff(attempt: u64, base: Duration, max: Duration) -> Duration {
    let base_ms = base.as_millis() as u64;
    let max_ms = max.as_millis() as u64;
    let (lo, hi) = (base_ms.min(max_ms), base_ms.max(max_ms));
    let exp = attempt.saturating_sub(1).min(30);
    let shifted = lo.saturating_mul(1u64 << exp);
    let capped = shifted.clamp(lo, hi);

    // ±25% jitter from a cheap, non-cryptographic source: enough to break
    // alignment between workers, deterministic enough to stay inside bounds.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0) as u64;
    let fraction = (nanos % 1000) as f64 / 1000.0; // 0.0..1.0
    let jitter = 0.75 + 0.5 * fraction;
    let jittered = (capped as f64 * jitter) as u64;

    Duration::from_millis(jittered.clamp(lo, hi))
}

/// What a caller should do after observing a blocked admission check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockedObservation {
    /// Emit a `worker.admission_blocked` event: this is the first blocked
    /// check of a hold, or the heartbeat interval has elapsed since the last
    /// emission.
    Emit,
    /// Stay quiet: a blocked event was emitted recently enough.
    HoldQuiet,
}

/// State machine for one continuous admission hold.
///
/// Holds the "when do I emit" decisions so the three callers (worker run loop,
/// boot gate, supervisor spawn gate) cannot drift: the first blocked check
/// emits, subsequent checks emit only per [`AdmissionHoldConfig::
/// heartbeat_interval`], and the transition back to admitted emits exactly one
/// restore event — before selection resumes.
///
/// The hold carries no thresholds and no config: it cannot weaken policy. The
/// thresholds live with the caller's next [`check_launch_admission`] call.
#[derive(Debug)]
pub struct AdmissionHold {
    config: AdmissionHoldConfig,
    attempt: u64,
    blocked_since: Option<Instant>,
    last_emit: Option<Instant>,
}

impl AdmissionHold {
    pub fn new(config: AdmissionHoldConfig) -> Self {
        AdmissionHold {
            config,
            attempt: 0,
            blocked_since: None,
            last_emit: None,
        }
    }

    /// Whether a hold is currently in progress.
    pub fn is_blocked(&self) -> bool {
        self.blocked_since.is_some()
    }

    /// How long the current (or most recent) hold has lasted.
    pub fn blocked_for(&self) -> Duration {
        self.blocked_since
            .map(|since| since.elapsed())
            .unwrap_or_default()
    }

    /// How many admission checks have run during the hold.
    pub fn attempts(&self) -> u64 {
        self.attempt
    }

    /// Record a blocked check. Returns whether an event should be emitted.
    pub fn observe_blocked(&mut self) -> BlockedObservation {
        self.attempt = self.attempt.saturating_add(1);
        if self.blocked_since.is_none() {
            self.blocked_since = Some(Instant::now());
            self.last_emit = Some(Instant::now());
            return BlockedObservation::Emit;
        }
        let emit_due = self
            .last_emit
            .map(|last| last.elapsed() >= self.config.heartbeat_interval)
            .unwrap_or(true);
        if emit_due {
            self.last_emit = Some(Instant::now());
            BlockedObservation::Emit
        } else {
            BlockedObservation::HoldQuiet
        }
    }

    /// Record an admitted check that ends a hold. Returns whether a
    /// `worker.admission_restored` event should be emitted — true whenever a
    /// hold was in progress, so restoration is never announced for a worker
    /// that was never blocked.
    pub fn observe_admitted(&mut self) -> bool {
        if !self.is_blocked() {
            self.attempt = 0;
            return false;
        }
        self.blocked_since = None;
        self.last_emit = None;
        true
    }

    /// The retry delay to sleep before the next admission check.
    pub fn backoff(&self) -> Duration {
        admission_backoff(
            self.attempt.max(1),
            self.config.base_backoff,
            self.config.max_backoff,
        )
    }
}

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
pub struct TokenBucket {
    /// Current available tokens (fractional for smooth refill).
    pub tokens: f64,
    /// Maximum tokens (= requests_per_minute).
    pub capacity: u32,
    /// Last time tokens were refilled.
    pub last_refill: DateTime<Utc>,
}

impl TokenBucket {
    pub(crate) fn new(capacity: u32) -> Self {
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

    /// Check executing provider concurrency against the worker registry.
    fn check_provider_concurrency(
        &self,
        provider: &str,
        max_concurrent: u32,
        registry: &Registry,
    ) -> Result<RateLimitDecision> {
        let workers = registry.list()?;
        let active_count = workers
            .iter()
            .filter(|w| {
                w.provider.as_deref() == Some(provider)
                    && w.state.as_ref() == Some(&WorkerState::Executing)
            })
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

    /// Check executing model concurrency against the worker registry.
    fn check_model_concurrency(
        &self,
        model: &str,
        max_concurrent: u32,
        registry: &Registry,
    ) -> Result<RateLimitDecision> {
        let workers = registry.list()?;
        let active_count = workers
            .iter()
            .filter(|w| {
                w.model.as_deref() == Some(model)
                    && w.state.as_ref() == Some(&WorkerState::Executing)
            })
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
    /// Emits structured telemetry events when thresholds are exceeded; does not
    /// block dispatch. Logs tracing warnings in addition to telemetry for
    /// operator visibility.
    pub fn check_system_resources(
        cpu_load_warn: f64,
        memory_free_warn_mb: u64,
        telemetry: &Telemetry,
    ) {
        // CPU load: read /proc/loadavg on Linux.
        if let Ok(loadavg) = std::fs::read_to_string("/proc/loadavg") {
            if let Some(load_str) = loadavg.split_whitespace().next() {
                if let Ok(load) = load_str.parse::<f64>() {
                    // Normalize by number of CPUs.
                    let num_cpus = std::thread::available_parallelism()
                        .map(|n| n.get())
                        .unwrap_or(1);
                    let normalized = load / num_cpus as f64;
                    if normalized > cpu_load_warn {
                        tracing::warn!(
                            load_1min = %load_str,
                            normalized = %format!("{:.2}", normalized),
                            threshold = %format!("{:.2}", cpu_load_warn),
                            "CPU load exceeds warning threshold"
                        );
                        // Emit structured telemetry event.
                        let _ = telemetry.emit(
                            EventKind::FleetCpuSaturated {
                                load_average: load,
                                threshold: cpu_load_warn,
                                core_count: num_cpus,
                            },
                            chrono::Utc::now(),
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
                    // Emit structured telemetry event.
                    let _ = telemetry.emit(
                        EventKind::FleetMemoryLow {
                            free_mb: avail_mb,
                            threshold_mb: memory_free_warn_mb,
                        },
                        chrono::Utc::now(),
                    );
                }
            }
        }
    }

    /// Check system resources before worker launch.
    ///
    /// Returns an error if CPU or memory resources are saturated, blocking
    /// launch with a clear actionable reason. Unlike `check_system_resources`,
    /// which only emits telemetry warnings, this function returns a `Result`
    /// so the caller can defer/retry launch instead of proceeding into a step
    /// known to be slow (~5s) and vulnerable to being killed mid-step by the OS.
    ///
    /// # Errors
    ///
    /// Returns `Err` if:
    /// - Normalized CPU load (1-minute average / core count) exceeds `cpu_load_warn`
    /// - Available memory falls below `memory_free_warn_mb`
    pub fn check_system_resources_for_launch(
        cpu_load_warn: f64,
        memory_free_warn_mb: u64,
        telemetry: &Telemetry,
    ) -> Result<()> {
        // Explicit opt-out for callers that must launch regardless of host load.
        //
        // `cpu_load_warn` is a normalized ratio capped at 1.0 by config validation, so
        // it cannot express "never defer": any box with load above its core count trips
        // the gate, and the caller then retries for up to 120s. That is correct for a
        // fleet worker and wrong for a test harness spawning one -- the test's own
        // timeout expires inside the wait, reporting only a hang. It is also a
        // reasonable deliberate operator override when launching onto a busy box.
        if std::env::var("NEEDLE_SKIP_LAUNCH_RESOURCE_CHECK").as_deref() == Ok("1") {
            tracing::debug!(
                "NEEDLE_SKIP_LAUNCH_RESOURCE_CHECK=1: skipping pre-launch resource gate"
            );
            return Ok(());
        }

        // CPU load: read /proc/loadavg on Linux.
        if let Ok(loadavg) = std::fs::read_to_string("/proc/loadavg") {
            if let Some(load_str) = loadavg.split_whitespace().next() {
                if let Ok(load) = load_str.parse::<f64>() {
                    // Normalize by number of CPUs.
                    let num_cpus = std::thread::available_parallelism()
                        .map(|n| n.get())
                        .unwrap_or(1);
                    let normalized = load / num_cpus as f64;
                    if normalized > cpu_load_warn {
                        // Emit structured telemetry event.
                        let _ = telemetry.emit(
                            EventKind::FleetCpuSaturated {
                                load_average: load,
                                threshold: cpu_load_warn,
                                core_count: num_cpus,
                            },
                            chrono::Utc::now(),
                        );
                        return Err(anyhow::anyhow!(
                            "CPU load saturated: {:.2} (1-minute average) / {} cores = {:.2} > threshold {:.2}",
                            load,
                            num_cpus,
                            normalized,
                            cpu_load_warn
                        ));
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
                    // Emit structured telemetry event.
                    let _ = telemetry.emit(
                        EventKind::FleetMemoryLow {
                            free_mb: avail_mb,
                            threshold_mb: memory_free_warn_mb,
                        },
                        chrono::Utc::now(),
                    );
                    return Err(anyhow::anyhow!(
                        "Memory saturated: {} MB available < {} MB threshold",
                        avail_mb,
                        memory_free_warn_mb
                    ));
                }
            }
        }

        Ok(())
    }

    /// Load-adaptive stagger for batch worker launches.
    ///
    /// Replaces fixed-interval sleep with a load-aware delay: uses the short
    /// default `base_stagger_secs` when system load is comfortable (CPU and
    /// memory within thresholds), extends the wait (bounded by `max_wait_secs`)
    /// when saturated. This prevents batch launches from blindly pushing past
    /// the saturation threshold their own later-launched members would then be
    /// killed by.
    ///
    /// # Parameters
    ///
    /// * `cpu_load_warn` - Normalized CPU load threshold (0.0–1.0). Above this,
    ///   stagger is extended.
    /// * `memory_free_warn_mb` - Available memory threshold (MB). Below this,
    ///   stagger is extended.
    /// * `base_stagger_secs` - Default stagger delay when load is comfortable.
    /// * `max_wait_secs` - Maximum additional wait time when load is high.
    /// * `check_interval_secs` - How often to recheck load during extended wait.
    /// * `telemetry` - Telemetry emitter for structured events.
    ///
    /// # Behavior
    ///
    /// 1. Check current system load (CPU and memory).
    /// 2. If comfortable: sleep for `base_stagger_secs`, return.
    /// 3. If saturated: loop and recheck every `check_interval_secs` until either:
    ///    - Load drops below threshold → proceed immediately.
    ///    - `max_wait_secs` total wait time reached → proceed anyway (bounded).
    /// 4. Emit `WorkerLaunchDeferred` telemetry when extended wait occurs.
    ///
    /// # Panics
    ///
    /// Panics if `check_interval_secs` is zero (would loop forever).
    pub fn load_adaptive_stagger(
        cpu_load_warn: f64,
        memory_free_warn_mb: u64,
        base_stagger_secs: u64,
        max_wait_secs: u64,
        check_interval_secs: u64,
        telemetry: &Telemetry,
    ) {
        assert!(
            check_interval_secs > 0,
            "check_interval_secs must be positive (got {})",
            check_interval_secs
        );

        // Helper to check if system is saturated (returns true if CPU or memory is high).
        let is_saturated = || -> bool {
            let mut saturated = false;

            // CPU load: read /proc/loadavg on Linux.
            if let Ok(loadavg) = std::fs::read_to_string("/proc/loadavg") {
                if let Some(load_str) = loadavg.split_whitespace().next() {
                    if let Ok(load) = load_str.parse::<f64>() {
                        let num_cpus = std::thread::available_parallelism()
                            .map(|n| n.get())
                            .unwrap_or(1);
                        let normalized = load / num_cpus as f64;
                        if normalized > cpu_load_warn {
                            saturated = true;
                            tracing::debug!(
                                load_1min = %load_str,
                                normalized = %format!("{:.2}", normalized),
                                threshold = %format!("{:.2}", cpu_load_warn),
                                "CPU load exceeds threshold - extending stagger"
                            );
                        }
                    }
                }
            }

            // Memory: read /proc/meminfo on Linux.
            if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
                for line in meminfo.lines() {
                    if line.starts_with("MemAvailable:") {
                        if let Some(val) = line.split_whitespace().nth(1) {
                            if let Ok(avail_kb) = val.parse::<u64>() {
                                let avail_mb = avail_kb / 1024;
                                if avail_mb < memory_free_warn_mb {
                                    saturated = true;
                                    tracing::debug!(
                                        available_mb = avail_mb,
                                        threshold_mb = memory_free_warn_mb,
                                        "available memory below threshold - extending stagger"
                                    );
                                }
                            }
                        }
                        break;
                    }
                }
            }

            saturated
        };

        // First check: if comfortable, use the short default stagger.
        if !is_saturated() {
            tracing::debug!(
                stagger_secs = base_stagger_secs,
                "system load comfortable - using default stagger"
            );
            std::thread::sleep(std::time::Duration::from_secs(base_stagger_secs));
            return;
        }

        // System is saturated: enter extended wait loop with periodic rechecks.
        tracing::info!(
            max_wait_secs = max_wait_secs,
            check_interval_secs = check_interval_secs,
            "system load saturated - entering extended stagger wait"
        );

        let mut total_waited = 0u64;
        let mut deferred_count = 0u64;

        while total_waited < max_wait_secs {
            // Sleep for the check interval.
            let sleep_time = std::cmp::min(check_interval_secs, max_wait_secs - total_waited);
            std::thread::sleep(std::time::Duration::from_secs(sleep_time));
            total_waited += sleep_time;
            deferred_count += 1;

            // Recheck: if load dropped, proceed immediately.
            if !is_saturated() {
                tracing::info!(
                    total_waited_secs = total_waited,
                    deferred_count = deferred_count,
                    "load recovered - proceeding with launch"
                );
                let _ = telemetry.emit(
                    EventKind::WorkerLaunchDeferred {
                        deferred_count,
                        total_wait_secs: total_waited,
                        reason: "load-adaptive stagger: load recovered within wait window"
                            .to_string(),
                    },
                    chrono::Utc::now(),
                );
                return;
            }
        }

        // Max wait reached without recovery - proceed anyway (bounded).
        tracing::warn!(
            total_waited_secs = total_waited,
            deferred_count = deferred_count,
            "max stagger wait reached without load recovery - proceeding anyway"
        );
        let _ = telemetry.emit(
            EventKind::WorkerLaunchDeferred {
                deferred_count,
                total_wait_secs: total_waited,
                reason: "load-adaptive stagger: max wait reached, proceeding despite saturation"
                    .to_string(),
            },
            chrono::Utc::now(),
        );
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
        make_entry_in_state(id, provider, model, WorkerState::Executing)
    }

    fn make_entry_in_state(
        id: &str,
        provider: Option<&str>,
        model: Option<&str>,
        state: WorkerState,
    ) -> WorkerEntry {
        WorkerEntry {
            id: id.to_string(),
            pid: std::process::id(),
            workspace: PathBuf::from("/tmp/test"),
            agent: "claude".to_string(),
            model: model.map(|s| s.to_string()),
            provider: provider.map(|s| s.to_string()),
            started_at: Utc::now(),
            beads_processed: 0,
            config_reload_generation: 0,
            state: Some(state),
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
    fn dispatching_workers_do_not_consume_provider_or_model_concurrency() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new(dir.path());
        for i in 0..18 {
            registry
                .register(make_entry_in_state(
                    &format!("waiting-{i}"),
                    Some("zai-proxy"),
                    Some("glm-5.3"),
                    WorkerState::Dispatching,
                ))
                .unwrap();
        }

        let limits = make_limits(
            vec![("zai-proxy", Some(10), None)],
            vec![("glm-5.3", Some(10))],
        );
        let limiter = RateLimiter::new(limits, dir.path());

        let decision = limiter
            .check(Some("zai-proxy"), Some("glm-5.3"), &registry)
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
        let telemetry = Telemetry::new("test-worker".to_string());
        RateLimiter::check_system_resources(0.8, 512, &telemetry);
    }

    #[test]
    fn provider_concurrency_ignores_dead_pids() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new(dir.path());

        // Register a live worker.
        registry
            .register(make_entry("live-worker", Some("anthropic"), Some("sonnet")))
            .unwrap();

        // Create an entry with a fake (dead) PID and write it directly to the file.
        let mut dead_entry = make_entry("dead-worker", Some("anthropic"), Some("sonnet"));
        dead_entry.pid = 9999999; // Nonexistent PID (well beyond typical ranges).

        use crate::registry::RegistryFile;
        let file_content = serde_json::to_string_pretty(&RegistryFile {
            workers: vec![
                make_entry("live-worker", Some("anthropic"), Some("sonnet")),
                dead_entry,
            ],
            updated_at: chrono::Utc::now(),
        })
        .unwrap();
        std::fs::write(registry.path(), file_content).unwrap();

        // Concurrency limit of 2: only the live worker should count.
        let limits = make_limits(vec![("anthropic", Some(2), None)], vec![]);
        let limiter = RateLimiter::new(limits, dir.path());

        // Should be allowed because dead PIDs are filtered out.
        let decision = limiter.check(Some("anthropic"), None, &registry).unwrap();
        assert_eq!(decision, RateLimitDecision::Allowed);
    }

    // ── Launch admission (plan revision 24 §4.6 / N-T33) ──

    /// Serializes tests that mutate process-global env vars.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn write_probe_files(dir: &Path, loadavg: &str, mem_available_kb: &str) {
        std::fs::write(dir.join("loadavg"), format!("{loadavg}\n")).unwrap();
        std::fs::write(
            dir.join("meminfo"),
            format!("MemTotal: 8388608 kB\nMemAvailable: {mem_available_kb} kB\n"),
        )
        .unwrap();
    }

    #[test]
    fn admission_gate_blocks_cpu_over_threshold_with_typed_details() {
        let dir = tempfile::tempdir().unwrap();
        // 8.0 load on 4 cores = 2.0 normalized > 0.8.
        write_probe_files(dir.path(), "8.00 7.00 6.00 1/123 45678", "4194304");

        let snapshot = read_resource_snapshot(&ResourceProbe::from_dir(dir.path()));
        assert_eq!(snapshot.load_1min, Some(8.0), "mock loadavg should parse");

        let decision = check_launch_admission(
            AdmissionThresholds::new(0.8, 512),
            ResourceSnapshot {
                load_1min: Some(8.0),
                cores: 4,
                available_mb: Some(4096),
            },
        );
        let block = decision.block().expect("expected a typed block");
        assert_eq!(
            block.resource,
            AdmissionResource::Cpu {
                load: 8.0,
                cores: 4
            }
        );
        assert_eq!(block.actual, "2.00");
        assert_eq!(block.threshold, "0.80");
        assert!(
            block.reason.contains("CPU load saturated"),
            "{}",
            block.reason
        );
        assert_eq!(block.resource.to_string(), "cpu");
    }

    #[test]
    fn admission_gate_blocks_memory_under_threshold() {
        let decision = check_launch_admission(
            AdmissionThresholds::new(0.8, 512),
            ResourceSnapshot {
                load_1min: Some(0.5),
                cores: 4,
                available_mb: Some(128),
            },
        );
        let block = decision.block().expect("expected a typed block");
        assert_eq!(
            block.resource,
            AdmissionResource::Memory { available_mb: 128 }
        );
        assert_eq!(block.actual, "128 MB");
        assert_eq!(block.threshold, "512 MB");
        assert!(
            block.reason.contains("Memory saturated"),
            "{}",
            block.reason
        );
        assert_eq!(block.resource.to_string(), "memory");
    }

    #[test]
    fn admission_gate_admits_inside_policy() {
        let decision = check_launch_admission(
            AdmissionThresholds::new(0.8, 512),
            ResourceSnapshot {
                load_1min: Some(3.1),
                cores: 4,
                available_mb: Some(2048),
            },
        );
        assert!(decision.is_admitted());
    }

    #[test]
    fn admission_gate_admits_when_probe_readings_are_missing() {
        // A probe that cannot produce a reading must not block forever, and a
        // missing file must not read as a zero (which would look like "no
        // memory available").
        let dir = tempfile::tempdir().unwrap();
        let snapshot = read_resource_snapshot(&ResourceProbe::from_dir(dir.path()));
        assert_eq!(snapshot.load_1min, None);
        assert_eq!(snapshot.available_mb, None);

        let decision = check_launch_admission(AdmissionThresholds::new(0.8, 512), snapshot);
        assert!(decision.is_admitted());
    }

    #[test]
    fn probe_env_override_selects_mock_directory() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        write_probe_files(dir.path(), "1.00 1.00 1.00 1/123 45678", "2097152");

        std::env::set_var("NEEDLE_LAUNCH_RESOURCE_PROBE", dir.path());
        let probe = ResourceProbe::from_env();
        std::env::remove_var("NEEDLE_LAUNCH_RESOURCE_PROBE");

        let snapshot = read_resource_snapshot(&probe);
        assert_eq!(snapshot.load_1min, Some(1.0));
        assert_eq!(snapshot.available_mb, Some(2048));
    }

    #[test]
    fn admission_backoff_grows_to_cap_and_stays_in_bounds() {
        let base = Duration::from_secs(5);
        let max = Duration::from_secs(30);
        for attempt in 1..=20u64 {
            let delay = admission_backoff(attempt, base, max);
            assert!(
                delay >= Duration::from_secs(1) && delay <= max,
                "attempt {attempt} produced out-of-bounds delay {delay:?}"
            );
        }
        // Late attempts must sit at (or under) the cap, never grow past it.
        for _ in 0..50 {
            let delay = admission_backoff(25, base, max);
            assert!(delay <= max, "cap exceeded: {delay:?}");
            assert!(
                delay >= Duration::from_secs(15),
                "jitter floor too low: {delay:?}"
            );
        }
    }

    #[test]
    fn admission_hold_emits_first_blocked_then_rate_bounds() {
        let config = AdmissionHoldConfig {
            base_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(20),
            heartbeat_interval: Duration::from_millis(50),
        };
        let mut hold = AdmissionHold::new(config);
        assert!(!hold.is_blocked());

        assert_eq!(hold.observe_blocked(), BlockedObservation::Emit);
        assert_eq!(hold.attempts(), 1);

        // Retries inside the heartbeat window stay quiet — that is the bound.
        for expected_attempt in 2..=6 {
            assert_eq!(
                hold.observe_blocked(),
                BlockedObservation::HoldQuiet,
                "attempt {expected_attempt} should not have emitted"
            );
            assert_eq!(hold.attempts(), expected_attempt);
        }

        // After the heartbeat window, exactly one more emission.
        std::thread::sleep(Duration::from_millis(60));
        assert_eq!(hold.observe_blocked(), BlockedObservation::Emit);
        assert_eq!(hold.observe_blocked(), BlockedObservation::HoldQuiet);
    }

    #[test]
    fn admission_hold_announces_restore_only_after_a_hold() {
        let mut hold = AdmissionHold::new(AdmissionHoldConfig::default());
        assert!(!hold.observe_admitted(), "never blocked: no restore event");

        let _ = hold.observe_blocked();
        assert!(
            hold.observe_admitted(),
            "hold in progress: restore required"
        );
        assert!(!hold.is_blocked());
        assert!(
            !hold.observe_admitted(),
            "hold ended: no second restore event"
        );
    }

    #[test]
    fn admission_hold_backoff_respects_configured_bounds() {
        let config = AdmissionHoldConfig {
            base_backoff: Duration::from_millis(40),
            max_backoff: Duration::from_millis(90),
            heartbeat_interval: Duration::from_secs(30),
        };
        let mut hold = AdmissionHold::new(config);
        for attempt in 1..=10 {
            let _ = hold.observe_blocked();
            let delay = hold.backoff();
            assert!(
                delay >= Duration::from_millis(20) && delay <= Duration::from_millis(90),
                "attempt {attempt} backoff {delay:?} outside [base/2, cap]"
            );
        }
        assert_eq!(hold.attempts(), 10);
        assert!(hold.blocked_for() < Duration::from_secs(5));
    }

    #[test]
    fn provider_conjunction_with_dead_pid_does_not_block() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Registry::new(dir.path());

        // Register one live worker.
        registry
            .register(make_entry("live-alpha", Some("anthropic"), Some("sonnet")))
            .unwrap();

        // Add 10 dead workers with fake PIDs (starting from 9999990 to avoid overflow).
        use crate::registry::RegistryFile;
        let mut dead_workers = Vec::new();
        for i in 0..10 {
            let mut dead_entry =
                make_entry(&format!("dead-{i}"), Some("anthropic"), Some("sonnet"));
            dead_entry.pid = 9999900 + i as u32; // Nonexistent PIDs.
            dead_workers.push(dead_entry);
        }

        let file_content = serde_json::to_string_pretty(&RegistryFile {
            workers: dead_workers,
            updated_at: chrono::Utc::now(),
        })
        .unwrap();
        std::fs::write(registry.path(), file_content).unwrap();

        // Concurrency limit of 5: even with 11 total entries (1 live + 10 dead),
        // only 1 should count after filtering.
        let limits = make_limits(vec![("anthropic", Some(5), None)], vec![]);
        let limiter = RateLimiter::new(limits, dir.path());

        let decision = limiter.check(Some("anthropic"), None, &registry).unwrap();
        assert_eq!(decision, RateLimitDecision::Allowed);
    }
}
