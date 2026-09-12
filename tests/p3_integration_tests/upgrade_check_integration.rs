//! Integration coverage for the supervisor's periodic upgrade-check gate.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use needle::cli::{Cli, CliCommand};
use needle::config::{Config, SupervisorConfig as ConfigSupervisorConfig};
use needle::supervisor::{SupervisorConfig, UpgradeCheckFn, UpgradePoller};
use needle::telemetry::{EventKind, Telemetry};

fn counting_checker(calls: &Arc<AtomicUsize>) -> UpgradeCheckFn {
    let calls = Arc::clone(calls);
    Arc::new(move |_| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
}

#[test]
fn supervisor_upgrade_config_round_trips_and_reaches_runtime_config() {
    let parsed: ConfigSupervisorConfig =
        serde_yaml::from_str("auto_upgrade_check: true\nupdate_check_interval_secs: 3600\n")
            .expect("upgrade-check config should deserialize");

    assert!(parsed.auto_upgrade_check);
    assert_eq!(parsed.update_check_interval_secs, 3600);

    let config = Config {
        supervisor: parsed,
        ..Config::default()
    };
    let runtime = SupervisorConfig::from_config(&config);
    assert!(runtime.auto_upgrade_check);
    assert_eq!(runtime.update_check_interval_secs, 3600);
}

#[test]
fn upgrade_interval_validation_matches_documented_minimum() {
    let mut config = Config::default();
    config.supervisor.update_check_interval_secs = 59;
    let errors = needle::config::ConfigLoader::validate(&config);
    assert!(errors.iter().any(|error| {
        error.full_path == "supervisor.update_check_interval_secs"
            && error.message.contains("minimum is 60 seconds")
    }));

    config.supervisor.update_check_interval_secs = 60;
    assert!(!needle::config::ConfigLoader::validate(&config)
        .iter()
        .any(|error| error.full_path == "supervisor.update_check_interval_secs"));
}

#[test]
fn enabled_supervisor_poller_runs_immediately_and_at_each_interval() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut poller = UpgradePoller::with_checker(true, 60, counting_checker(&calls));
    let telemetry = Telemetry::new("upgrade-check-integration".to_string());
    let start = Instant::now();

    assert!(poller.poll_at(&telemetry, start));
    assert!(!poller.poll_at(&telemetry, start + Duration::from_secs(59)));
    assert!(poller.poll_at(&telemetry, start + Duration::from_secs(60)));
    assert!(!poller.poll_at(&telemetry, start + Duration::from_secs(119)));
    assert!(poller.poll_at(&telemetry, start + Duration::from_secs(120)));
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn auto_upgrade_check_false_disables_supervisor_polling() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut poller = UpgradePoller::with_checker(false, 60, counting_checker(&calls));
    let telemetry = Telemetry::new("upgrade-check-disabled-integration".to_string());
    let start = Instant::now();

    assert!(!poller.poll_at(&telemetry, start));
    assert!(!poller.poll_at(&telemetry, start + Duration::from_secs(60)));
    assert!(!poller.poll_at(&telemetry, start + Duration::from_secs(600)));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn manual_upgrade_command_shape_is_unchanged() {
    let plain = Cli::try_parse_from(["needle", "upgrade"]).expect("needle upgrade should parse");
    let check = Cli::try_parse_from(["needle", "upgrade", "--check"])
        .expect("needle upgrade --check should parse");

    assert!(matches!(
        plain.command,
        CliCommand::Upgrade { check: false }
    ));
    assert!(matches!(check.command, CliCommand::Upgrade { check: true }));
}

#[test]
fn upgrade_check_telemetry_event_names_remain_stable() {
    assert_eq!(
        EventKind::UpgradeCheckStarted {
            source: "supervisor".to_string()
        }
        .event_type(),
        "upgrade_check.started"
    );
    assert_eq!(
        EventKind::UpgradeCheckCompleted {
            source: "supervisor".to_string(),
            current_version: "0.3.1".to_string(),
            latest_version: "0.3.2".to_string(),
            update_available: true,
            has_release_notes: true,
        }
        .event_type(),
        "upgrade_check.completed"
    );
    assert_eq!(
        EventKind::UpgradeCheckFailed {
            source: "supervisor".to_string(),
            error_message: "network failure".to_string(),
            error_type: "network".to_string(),
        }
        .event_type(),
        "upgrade_check.failed"
    );
}

#[test]
fn comprehensive_upgrade_config_validation_rejects_invalid_values() {
    let mut config = Config::default();

    // Test 1: Zero interval should be rejected
    config.supervisor.update_check_interval_secs = 0;
    let errors = needle::config::ConfigLoader::validate(&config);
    assert!(
        errors.iter().any(|error| {
            error.full_path == "supervisor.update_check_interval_secs"
                && error.message.contains("must be at least 60 seconds")
        }),
        "Zero interval should be rejected"
    );

    // Test 2: Interval below minimum (59 seconds) should be rejected
    config.supervisor.update_check_interval_secs = 59;
    let errors = needle::config::ConfigLoader::validate(&config);
    assert!(
        errors.iter().any(|error| {
            error.full_path == "supervisor.update_check_interval_secs"
                && error.message.contains("minimum is 60 seconds")
        }),
        "Interval of 59 seconds should be rejected"
    );

    // Test 3: Interval below minimum (1 second) should be rejected
    config.supervisor.update_check_interval_secs = 1;
    let errors = needle::config::ConfigLoader::validate(&config);
    assert!(
        errors.iter().any(|error| {
            error.full_path == "supervisor.update_check_interval_secs"
                && error.message.contains("minimum is 60 seconds")
        }),
        "Interval of 1 second should be rejected"
    );

    // Test 4: Valid minimum interval (60 seconds) should be accepted
    config.supervisor.update_check_interval_secs = 60;
    let errors = needle::config::ConfigLoader::validate(&config);
    assert!(
        !errors
            .iter()
            .any(|error| error.full_path == "supervisor.update_check_interval_secs"),
        "Interval of 60 seconds should be accepted"
    );

    // Test 5: Valid standard interval (3600 seconds / 1 hour) should be accepted
    config.supervisor.update_check_interval_secs = 3600;
    let errors = needle::config::ConfigLoader::validate(&config);
    assert!(
        !errors
            .iter()
            .any(|error| error.full_path == "supervisor.update_check_interval_secs"),
        "Interval of 3600 seconds should be accepted"
    );

    // Test 6: Valid long interval (21600 seconds / 6 hours) should be accepted
    config.supervisor.update_check_interval_secs = 21600;
    let errors = needle::config::ConfigLoader::validate(&config);
    assert!(
        !errors
            .iter()
            .any(|error| error.full_path == "supervisor.update_check_interval_secs"),
        "Interval of 21600 seconds should be accepted"
    );

    // Test 7: Valid very long interval (86400 seconds / 24 hours) should be accepted
    config.supervisor.update_check_interval_secs = 86400;
    let errors = needle::config::ConfigLoader::validate(&config);
    assert!(
        !errors
            .iter()
            .any(|error| error.full_path == "supervisor.update_check_interval_secs"),
        "Interval of 86400 seconds should be accepted"
    );
}

#[test]
fn auto_upgrade_check_enabled_field_validates_correctly() {
    // Test 1: auto_upgrade_check = true should be valid
    let yaml_true = "auto_upgrade_check: true\nupdate_check_interval_secs: 3600\n";
    let parsed: ConfigSupervisorConfig =
        serde_yaml::from_str(yaml_true).expect("true value should deserialize");
    assert!(
        parsed.auto_upgrade_check,
        "auto_upgrade_check should be true"
    );
    assert_eq!(
        parsed.update_check_interval_secs, 3600,
        "interval should be parsed correctly"
    );

    let config = Config {
        supervisor: parsed,
        ..Config::default()
    };
    let errors = needle::config::ConfigLoader::validate(&config);
    assert!(
        errors.is_empty()
            || !errors
                .iter()
                .any(|e| e.full_path == "supervisor.auto_upgrade_check"),
        "auto_upgrade_check=true should not produce validation errors"
    );

    // Test 2: auto_upgrade_check = false should be valid
    let yaml_false = "auto_upgrade_check: false\nupdate_check_interval_secs: 3600\n";
    let parsed: ConfigSupervisorConfig =
        serde_yaml::from_str(yaml_false).expect("false value should deserialize");
    assert!(
        !parsed.auto_upgrade_check,
        "auto_upgrade_check should be false"
    );
    assert_eq!(
        parsed.update_check_interval_secs, 3600,
        "interval should be parsed correctly"
    );

    let config = Config {
        supervisor: parsed,
        ..Config::default()
    };
    let errors = needle::config::ConfigLoader::validate(&config);
    assert!(
        errors.is_empty()
            || !errors
                .iter()
                .any(|e| e.full_path == "supervisor.auto_upgrade_check"),
        "auto_upgrade_check=false should not produce validation errors"
    );
}

#[test]
fn upgrade_config_runtime_state_properly_initialized() {
    // Test 1: Enabled config creates active poller
    let config = Config {
        supervisor: ConfigSupervisorConfig {
            auto_upgrade_check: true,
            update_check_interval_secs: 3600,
            ..Default::default()
        },
        ..Default::default()
    };
    let runtime = SupervisorConfig::from_config(&config);
    assert!(
        runtime.auto_upgrade_check,
        "runtime should reflect enabled state"
    );
    assert_eq!(
        runtime.update_check_interval_secs, 3600,
        "runtime should reflect interval"
    );

    // Test 2: Disabled config creates inactive poller
    let config = Config {
        supervisor: ConfigSupervisorConfig {
            auto_upgrade_check: false,
            update_check_interval_secs: 3600,
            ..Default::default()
        },
        ..Default::default()
    };
    let runtime = SupervisorConfig::from_config(&config);
    assert!(
        !runtime.auto_upgrade_check,
        "runtime should reflect disabled state"
    );
    assert_eq!(
        runtime.update_check_interval_secs, 3600,
        "runtime interval should still be set even when disabled"
    );
}

#[test]
fn last_check_runtime_state_initializes_as_none_and_updates_on_poll() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut poller = UpgradePoller::with_checker(true, 60, counting_checker(&calls));

    // Initial state should have no last_check
    // This is tested implicitly by the first poll_at succeeding below

    let telemetry = Telemetry::new("last-check-test".to_string());
    let start = Instant::now();

    // First poll should succeed and set last_check
    assert!(
        poller.poll_at(&telemetry, start),
        "first poll should succeed"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "check should have been called"
    );

    // Immediate second poll should be skipped (within interval)
    assert!(
        !poller.poll_at(&telemetry, start + Duration::from_secs(59)),
        "poll within interval should be skipped"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "no additional check should have been made"
    );

    // Poll at exact interval should succeed
    assert!(
        poller.poll_at(&telemetry, start + Duration::from_secs(60)),
        "poll at interval should succeed"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "check should have been called again"
    );
}
