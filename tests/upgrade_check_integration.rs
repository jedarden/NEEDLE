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
        error.field == "supervisor.update_check_interval_secs"
            && error.message.contains("minimum is 60 seconds")
    }));

    config.supervisor.update_check_interval_secs = 60;
    assert!(!needle::config::ConfigLoader::validate(&config)
        .iter()
        .any(|error| error.field == "supervisor.update_check_interval_secs"));
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
