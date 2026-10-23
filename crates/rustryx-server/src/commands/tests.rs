//! The subcommands in process, with an injected environment and migrator.

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use clap::Parser;
use rustryx_config::MapEnv;

use super::*;
use crate::migrate::NotWiredMigrator;

const CANARY: &str = "canary-SECRET-0xC0FFEE";

struct CountingMigrator {
    calls: AtomicUsize,
    outcome: Result<&'static str, &'static str>,
}

impl CountingMigrator {
    fn ok() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            outcome: Ok("applied 3 migrations"),
        }
    }

    fn failing() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            outcome: Err("lock held by another process"),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Migrator for CountingMigrator {
    async fn migrate(&self, _config: &AppConfig) -> anyhow::Result<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.outcome
            .map(str::to_owned)
            .map_err(|m| anyhow::anyhow!(m))
    }
}

struct Run {
    result: Result<(), CliError>,
    stdout: String,
    stderr: String,
}

fn run(args: &[&str], env: &MapEnv, migrator: &dyn Migrator) -> Run {
    let cli =
        Cli::try_parse_from(std::iter::once("rustryx").chain(args.iter().copied())).expect("args");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let result = {
        let mut host = Host {
            env,
            migrator,
            stdout: &mut out,
            stderr: &mut err,
            init_telemetry: false,
        };
        execute(&cli, &mut host)
    };
    Run {
        result,
        stdout: String::from_utf8(out).expect("utf8"),
        stderr: String::from_utf8(err).expect("utf8"),
    }
}

fn dev_env() -> MapEnv {
    MapEnv::new()
        .with("RUSTRYX_ENVIRONMENT", "development")
        .with(
            "RUSTRYX_DATABASE_URL",
            format!("postgres://app:{CANARY}@db.example:5432/rustryx"),
        )
        .with("RUSTRYX_OBSERVABILITY_METRICS_PUBLIC", "true")
}

#[test]
fn print_config_shows_the_effective_configuration_with_secrets_redacted() {
    let env = dev_env()
        .with("RUSTRYX_AUTH_BOOTSTRAP_ADMIN_PASSWORD", CANARY)
        .with("RUSTRYX_SERVER_SHUTDOWN_TIMEOUT_SECS", "12");
    let outcome = run(&["print-config"], &env, &NotWiredMigrator);
    outcome.result.expect("prints");
    assert!(
        outcome.stdout.contains("environment: development"),
        "{}",
        outcome.stdout
    );
    assert!(outcome.stdout.contains("shutdown_timeout_secs: 12"));
    assert!(outcome.stdout.contains("[REDACTED]"));
    assert!(
        !outcome.stdout.contains(CANARY),
        "a secret reached the output"
    );
    assert!(!outcome.stderr.contains(CANARY));
}

#[test]
fn must_not_print_secrets_given_through_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("dburl");
    std::fs::write(&file, format!("postgres://app:{CANARY}@db/x\n")).expect("write");
    let env = MapEnv::new()
        .with("RUSTRYX_ENVIRONMENT", "development")
        .with(
            "RUSTRYX_DATABASE_URL_FILE",
            file.to_string_lossy().into_owned(),
        );
    let outcome = run(&["print-config"], &env, &NotWiredMigrator);
    outcome.result.expect("prints");
    assert!(!outcome.stdout.contains(CANARY));
    assert!(outcome.stdout.contains("[REDACTED]"), "{}", outcome.stdout);
}

#[test]
fn an_invalid_configuration_is_exit_code_78_with_every_error_and_no_output() {
    let env = MapEnv::new().with("RUSTRYX_SERVER_LISTEN_ADDR", "garbage");
    let outcome = run(&["print-config"], &env, &NotWiredMigrator);
    let error = outcome.result.expect_err("must fail");
    assert_eq!(error.exit_code(), 78);
    let text = error.to_string();
    assert!(text.contains("server.listen_addr"), "{text}");
    assert!(
        text.contains("database.url"),
        "rule errors come in the same pass: {text}"
    );
    assert!(text.contains("auth.bootstrap_admin.password"), "{text}");
    assert_eq!(outcome.stdout, "");
}

#[test]
fn must_not_start_serve_with_an_invalid_configuration() {
    let outcome = run(&["serve"], &MapEnv::new(), &NotWiredMigrator);
    let error = outcome.result.expect_err("serve must refuse");
    assert_eq!(
        error.exit_code(),
        78,
        "returned before any runtime or port exists"
    );
}

#[test]
fn must_not_run_the_migrator_with_an_invalid_configuration() {
    let migrator = CountingMigrator::ok();
    let outcome = run(&["migrate"], &MapEnv::new(), &migrator);
    assert_eq!(outcome.result.expect_err("fails").exit_code(), 78);
    assert_eq!(migrator.calls(), 0);
}

#[test]
fn no_validate_prints_a_configuration_that_does_not_pass_the_rules() {
    let outcome = run(
        &["print-config", "--no-validate"],
        &MapEnv::new(),
        &NotWiredMigrator,
    );
    outcome.result.expect("structurally fine");
    assert!(outcome.stdout.contains("url: null"), "{}", outcome.stdout);
    let broken = MapEnv::new().with("RUSTRYX_LIMITS_MAX_BODY_MB", "lots");
    let outcome = run(
        &["print-config", "--no-validate"],
        &broken,
        &NotWiredMigrator,
    );
    assert_eq!(
        outcome
            .result
            .expect_err("types are still checked")
            .exit_code(),
        78
    );
}

#[test]
fn warnings_go_to_standard_error() {
    let env = dev_env().with("RUSTRYX_TYPO_KEY", "1");
    let outcome = run(&["print-config"], &env, &NotWiredMigrator);
    outcome.result.expect("prints");
    assert!(
        outcome
            .stderr
            .contains("warning: ignoring environment variable RUSTRYX_TYPO_KEY"),
        "{}",
        outcome.stderr
    );
}

#[test]
fn the_config_file_comes_from_the_flag_or_the_environment() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("rustryx.yaml");
    std::fs::write(&file, "server:\n  shutdown_timeout_secs: 41\n").expect("write");
    let path = file.to_string_lossy().into_owned();

    let by_flag = run(
        &["--config", &path, "print-config", "--no-validate"],
        &MapEnv::new(),
        &NotWiredMigrator,
    );
    by_flag.result.expect("prints");
    assert!(by_flag.stdout.contains("shutdown_timeout_secs: 41"));

    let by_env = MapEnv::new().with("RUSTRYX_CONFIG", path);
    let outcome = run(
        &["print-config", "--no-validate"],
        &by_env,
        &NotWiredMigrator,
    );
    outcome.result.expect("prints");
    assert!(outcome.stdout.contains("shutdown_timeout_secs: 41"));

    let missing = run(
        &["--config", "/nonexistent/rustryx.yaml", "print-config"],
        &MapEnv::new(),
        &NotWiredMigrator,
    );
    assert_eq!(missing.result.expect_err("fails").exit_code(), 78);
}

#[test]
fn migrate_runs_the_migrator_and_prints_its_summary() {
    let migrator = CountingMigrator::ok();
    let outcome = run(&["migrate"], &dev_env(), &migrator);
    outcome.result.expect("migrates");
    assert_eq!(migrator.calls(), 1);
    assert_eq!(outcome.stdout.trim(), "applied 3 migrations");
}

#[test]
fn a_failing_migration_is_exit_code_1_with_the_reason() {
    let migrator = CountingMigrator::failing();
    let outcome = run(&["migrate"], &dev_env(), &migrator);
    let error = outcome.result.expect_err("fails");
    assert_eq!(error.exit_code(), 1);
    assert!(error.to_string().contains("lock held by another process"));
    assert_eq!(outcome.stdout, "");
}

#[test]
fn the_default_migrator_says_it_is_not_wired() {
    let outcome = run(&["migrate"], &dev_env(), &NotWiredMigrator);
    let error = outcome.result.expect_err("not wired");
    assert_eq!(error.exit_code(), 1);
    let text = error.to_string();
    assert!(text.contains("not wired"), "{text}");
    assert!(!text.contains(CANARY));
}

#[test]
fn cli_errors_display_their_cause() {
    let config = CliError::from(ConfigError::Render("x".into()));
    assert!(config.to_string().contains("cannot render configuration"));
    let other = CliError::from(anyhow::anyhow!("inner").context("outer"));
    assert_eq!(other.to_string(), "outer: inner");
    assert!(std::error::Error::source(&other).is_none());
}
