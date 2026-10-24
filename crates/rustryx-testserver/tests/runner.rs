//! The client runner and the temporary project, exercised with ordinary programs (`sh`,
//! `sleep`): nothing here needs a package client, so it always runs.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests: a panic is the failure signal"
)]

use std::time::{Duration, Instant};

use rustryx_testserver::{TempProject, run_program};

const LONG: Duration = Duration::from_secs(20);

async fn sh(script: &str, env: &[(&str, &str)]) -> rustryx_testserver::Output {
    let project = TempProject::new();
    run_program("sh", &["-c", script], env, project.path(), LONG).await
}

#[tokio::test]
async fn captures_output_and_the_exit_code() {
    let output = sh("echo out; echo err >&2; exit 3", &[]).await;
    assert_eq!(output.code, Some(3));
    assert!(!output.success());
    assert_eq!(output.stdout, "out\n");
    assert_eq!(output.stderr, "err\n");
    assert_eq!(output.combined(), "out\nerr\n");
    assert!(!output.timed_out);
    assert!(output.command.starts_with("sh -c "));

    let fine = sh("echo fine", &[]).await;
    assert!(fine.success());
    fine.assert_success();
}

#[tokio::test]
#[should_panic(expected = "command failed")]
async fn assert_success_fails_the_test_with_the_output() {
    sh("echo visible-in-panic >&2; exit 1", &[])
        .await
        .assert_success();
}

#[tokio::test]
async fn passes_the_environment_and_runs_in_the_working_directory() {
    let project = TempProject::new();
    let output = run_program(
        "sh",
        &["-c", "echo $GREETING; pwd"],
        &[("GREETING", "hello")],
        project.path(),
        LONG,
    )
    .await;
    let mut lines = output.stdout.lines();
    assert_eq!(lines.next(), Some("hello"));
    let cwd = std::fs::canonicalize(lines.next().unwrap()).unwrap();
    assert_eq!(cwd, std::fs::canonicalize(project.path()).unwrap());
}

#[tokio::test]
async fn a_client_that_reads_stdin_does_not_hang() {
    let started = Instant::now();
    let output = sh("cat; echo done", &[]).await;
    assert_eq!(output.stdout, "done\n");
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn kills_a_client_that_exceeds_the_timeout() {
    let project = TempProject::new();
    let started = Instant::now();
    let output = run_program(
        "sleep",
        &["30"],
        &[],
        project.path(),
        Duration::from_millis(300),
    )
    .await;
    assert!(output.timed_out);
    assert_eq!(output.code, None);
    assert!(output.stderr.contains("timed out"));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the client must be killed, not awaited"
    );
}

#[tokio::test]
async fn reports_a_program_that_cannot_start() {
    let project = TempProject::new();
    let output = run_program(
        "/nonexistent/definitely-not-here",
        &[],
        &[],
        project.path(),
        LONG,
    )
    .await;
    assert_eq!(output.code, None);
    assert!(output.stderr.contains("cannot start"));
    assert!(!output.success());
}

#[tokio::test]
async fn must_not_show_generated_tokens_in_captured_output_or_the_command_line() {
    let token = format!("ryx_{}", "Zz9-_".repeat(9));
    let project = TempProject::new();
    let script = format!("echo {token}; echo {token} >&2");
    let output = run_program("sh", &["-c", &script], &[], project.path(), LONG).await;
    assert!(!output.stdout.contains(&token));
    assert!(!output.stderr.contains(&token));
    assert!(!output.command.contains(&token));
    assert!(output.stdout.contains("[REDACTED-TOKEN]"));
}

#[tokio::test]
async fn the_temp_project_writes_reads_and_isolates() {
    let project = TempProject::default();
    let written = project.write("a/b/pom.xml", "<project/>").await;
    assert!(written.starts_with(project.path()));
    assert_eq!(project.read("a/b/pom.xml").await.unwrap(), b"<project/>");
    assert_eq!(project.join("x"), project.path().join("x"));
    project.write("./plain.txt", b"bytes").await;
    assert!(project.read("missing").await.is_err());

    let env = project.isolated_env();
    let home = &env.iter().find(|(k, _)| k == "HOME").unwrap().1;
    assert!(home.starts_with(&*project.path().to_string_lossy()));
    assert!(env.iter().any(|(k, _)| k == "PUB_CACHE"));
    let pairs = TempProject::env_pairs(&env);
    assert_eq!(pairs.len(), env.len());

    let path = project.path().to_path_buf();
    drop(project);
    assert!(!path.exists(), "the directory is removed on drop");
}

#[tokio::test]
#[should_panic(expected = "must stay inside the project")]
async fn must_not_write_outside_the_project() {
    TempProject::new().write("../escape.txt", "x").await;
}

#[tokio::test]
#[should_panic(expected = "must stay inside the project")]
async fn must_not_write_an_absolute_path() {
    TempProject::new().write("/tmp/escape.txt", "x").await;
}
