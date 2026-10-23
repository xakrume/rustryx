//! End-to-end tests of the real `rustryx` binary: process exit codes, signals,
//! what reaches stdout and stderr.

// An integration test is a separate crate, so the lib's `cfg(test)` allowance does
// not reach it: a panic is the failure signal here too.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::match_wild_err_arm
)]

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_rustryx");
const CANARY: &str = "canary-SECRET-0xFEEDFACE";
const WAIT: Duration = Duration::from_secs(20);

/// A running or finished `rustryx` process with its output captured.
struct Proc {
    child: Child,
    stdout: Receiver<String>,
    stdout_seen: Vec<String>,
    stderr: Receiver<String>,
    stderr_seen: Vec<String>,
}

fn pump(source: impl Read + Send + 'static) -> Receiver<String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(source).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

/// Starts the binary with a clean environment plus `env`.
fn spawn(args: &[&str], env: &[(&str, &str)]) -> Proc {
    let mut command = Command::new(BIN);
    command.args(args).env_clear().envs(env.iter().copied());
    // Under `cargo llvm-cov` the child must write its coverage data too; the clean
    // environment would otherwise hide it. Not a RUSTRYX_ variable, so no effect on config.
    if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
        command.env("LLVM_PROFILE_FILE", profile);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn rustryx");
    let stdout = pump(child.stdout.take().expect("stdout"));
    let stderr = pump(child.stderr.take().expect("stderr"));
    Proc {
        child,
        stdout,
        stdout_seen: Vec::new(),
        stderr,
        stderr_seen: Vec::new(),
    }
}

fn dev_env<'a>(extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    let mut env = vec![
        ("RUSTRYX_ENVIRONMENT", "development"),
        (
            "RUSTRYX_DATABASE_URL",
            "postgres://app:pw@127.0.0.1:1/rustryx",
        ),
        ("RUSTRYX_SERVER_LISTEN_ADDR", "127.0.0.1:0"),
        ("RUSTRYX_OBSERVABILITY_METRICS_PUBLIC", "true"),
        ("RUSTRYX_SERVER_SHUTDOWN_TIMEOUT_SECS", "5"),
    ];
    env.extend_from_slice(extra);
    env
}

impl Proc {
    fn drain(&mut self) {
        while let Ok(line) = self.stdout.try_recv() {
            self.stdout_seen.push(line);
        }
        while let Ok(line) = self.stderr.try_recv() {
            self.stderr_seen.push(line);
        }
    }

    /// Waits for a JSON log line on stdout with `msg == wanted`; returns it.
    fn wait_for_log(&mut self, wanted: &str) -> serde_json::Value {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self
                .stdout
                .recv_timeout(left.min(Duration::from_millis(200)))
            {
                Ok(line) => {
                    self.stdout_seen.push(line.clone());
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line)
                        && json["msg"] == wanted
                    {
                        return json;
                    }
                }
                Err(RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
                Err(_) => panic!(
                    "no `{wanted}` log line; saw {:?}; stderr {:?}",
                    self.stdout_seen, self.stderr_seen
                ),
            }
        }
    }

    fn listening_addr(&mut self) -> SocketAddr {
        let line = self.wait_for_log("listening");
        line["addr"]
            .as_str()
            .expect("addr field")
            .parse()
            .expect("socket address")
    }

    fn signal(&self, name: &str) {
        let status = Command::new("kill")
            .arg(format!("-{name}"))
            .arg(self.child.id().to_string())
            .status()
            .expect("kill");
        assert!(status.success());
    }

    fn wait_exit(&mut self) -> i32 {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.child.try_wait().expect("wait") {
                // Let the pump threads deliver the tail of the output.
                thread::sleep(Duration::from_millis(100));
                self.drain();
                return status.code().unwrap_or(-1);
            }
            assert!(
                Instant::now() < deadline,
                "process did not exit; stdout {:?}",
                self.stdout_seen
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn all_output(&mut self) -> String {
        self.drain();
        format!(
            "{}\n{}",
            self.stdout_seen.join("\n"),
            self.stderr_seen.join("\n")
        )
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn http(addr: SocketAddr, path: &str, headers: &[(&str, &str)]) -> (u16, String) {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    let mut request = format!("GET {path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\n");
    for (k, v) in headers {
        let _ = write!(request, "{k}: {v}\r\n");
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).expect("write");
    let mut raw = String::new();
    stream.read_to_string(&mut raw).expect("read");
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("status line");
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    (status, body)
}

// ---- serve ------------------------------------------------------------------

#[test]
fn serves_probes_and_exits_zero_on_sigterm_after_draining() {
    let mut p = spawn(&["serve"], &dev_env(&[]));
    let addr = p.listening_addr();

    let (status, body) = http(addr, "/healthz", &[]);
    assert_eq!((status, body.as_str()), (200, r#"{"status":"ok"}"#));
    let (status, body) = http(addr, "/readyz", &[]);
    assert_eq!(status, 503, "no database wired in this build: {body}");
    let (status, body) = http(addr, "/metrics", &[]);
    assert_eq!(status, 200);
    assert!(body.contains("http_requests_total"), "{body}");

    p.signal("TERM");
    assert_eq!(p.wait_exit(), 0);
    let output = p.all_output();
    for expected in [
        "stop signal received",
        "draining: readiness is now 503",
        "shutdown complete",
    ] {
        assert!(
            output.contains(expected),
            "missing `{expected}` in {output}"
        );
    }
    for line in p.stdout_seen.iter().filter(|l| !l.is_empty()) {
        let json: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("stdout line is not JSON ({e}): {line}"));
        for key in ["ts", "level", "msg"] {
            assert!(json.get(key).is_some(), "{key} missing in {line}");
        }
    }
}

#[test]
fn sigint_stops_the_server_gracefully_too() {
    let mut p = spawn(&["serve"], &dev_env(&[]));
    let _ = p.listening_addr();
    p.signal("INT");
    assert_eq!(p.wait_exit(), 0);
    assert!(p.all_output().contains("shutdown complete"));
}

#[test]
fn must_not_open_a_port_when_the_configuration_is_invalid() {
    // The test keeps the port bound itself. Probing a freed port afterwards races with
    // other test processes that take ephemeral ports in parallel (seen in a workspace
    // run). If the server ever got as far as binding, it would fail with "address in
    // use" instead of exit code 78 and never print the "listening" line.
    let guard = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = guard.local_addr().expect("addr").to_string();
    // Production (the default) without any of its required secrets.
    let mut p = spawn(&["serve"], &[("RUSTRYX_SERVER_LISTEN_ADDR", addr.as_str())]);
    assert_eq!(p.wait_exit(), 78);
    let output = p.all_output();
    for key in [
        "database.url",
        "auth.bootstrap_admin.password",
        "auth.internal_jwt_key",
        "auth.cookie_key",
        "observability.metrics_token",
    ] {
        assert!(output.contains(key), "missing {key} in {output}");
    }
    assert!(output.contains("invalid configuration"), "{output}");
    assert!(
        !output.contains("listening"),
        "nothing may listen: {output}"
    );
    guard.set_nonblocking(true).expect("nonblocking");
    assert!(
        guard.accept().is_err(),
        "nothing may connect to the reserved port"
    );
    assert!(
        p.stdout_seen.is_empty(),
        "no logs are produced before the configuration is valid"
    );
}

#[test]
fn must_not_start_with_a_default_admin_password_in_production() {
    let mut env = dev_env(&[("RUSTRYX_AUTH_BOOTSTRAP_ADMIN_PASSWORD", "admin123")]);
    env.retain(|(k, _)| *k != "RUSTRYX_ENVIRONMENT");
    env.push(("RUSTRYX_ENV", "production"));
    let mut p = spawn(&["serve"], &env);
    assert_eq!(p.wait_exit(), 78);
    let output = p.all_output();
    assert!(output.contains("auth.bootstrap_admin.password"), "{output}");
    assert!(
        !output.contains("admin123"),
        "the weak value itself is never echoed: {output}"
    );
}

#[test]
fn must_not_start_with_plaintext_ldap() {
    let mut p = spawn(
        &["serve"],
        &dev_env(&[
            ("RUSTRYX_AUTH_LDAP_URL", "ldap://ldap.example:389"),
            ("RUSTRYX_AUTH_LDAP_ENABLED", "true"),
        ]),
    );
    assert_eq!(p.wait_exit(), 78);
    assert!(p.all_output().contains("auth.ldap.url"));
}

#[test]
fn starts_while_keycloak_is_unreachable() {
    let mut p = spawn(
        &["serve"],
        &dev_env(&[
            ("RUSTRYX_AUTH_OIDC_ENABLED", "true"),
            ("RUSTRYX_AUTH_OIDC_ISSUER", "http://127.0.0.1:1/realms/none"),
            ("RUSTRYX_AUTH_OIDC_CLIENT_ID", "rustryx-ui"),
            ("RUSTRYX_AUTH_OIDC_CLIENT_SECRET", "client-secret"),
            ("RUSTRYX_SERVER_BASE_URL", "http://localhost:8081"),
        ]),
    );
    let addr = p.listening_addr();
    assert_eq!(http(addr, "/healthz", &[]).0, 200);
    p.signal("TERM");
    assert_eq!(p.wait_exit(), 0);
}

#[test]
fn a_second_instance_on_the_same_port_fails_with_a_clear_message() {
    let mut first = spawn(&["serve"], &dev_env(&[]));
    let addr = first.listening_addr();
    let taken = addr.to_string();
    let mut second = spawn(
        &["serve"],
        &dev_env(&[("RUSTRYX_SERVER_LISTEN_ADDR", taken.as_str())]),
    );
    assert_eq!(second.wait_exit(), 1);
    assert!(
        second.all_output().contains("cannot listen on"),
        "{}",
        second.all_output()
    );
    first.signal("TERM");
    assert_eq!(first.wait_exit(), 0);
}

#[test]
fn must_not_write_any_secret_to_stdout_or_stderr() {
    let db_url = format!("postgres://app:{CANARY}@127.0.0.1:1/rustryx");
    let token = format!("{CANARY}-metrics-token");
    let mut p = spawn(
        &["serve"],
        &dev_env(&[
            ("RUSTRYX_DATABASE_URL", db_url.as_str()),
            ("RUSTRYX_AUTH_BOOTSTRAP_ADMIN_PASSWORD", CANARY),
            ("RUSTRYX_OBSERVABILITY_METRICS_PUBLIC", "false"),
            ("RUSTRYX_OBSERVABILITY_METRICS_TOKEN", token.as_str()),
            ("RUSTRYX_OBSERVABILITY_LOG_LEVEL", "debug"),
        ]),
    );
    let addr = p.listening_addr();
    let bearer = format!("Bearer {token}");
    assert_eq!(
        http(addr, "/metrics", &[("Authorization", bearer.as_str())]).0,
        200
    );
    assert_eq!(
        http(addr, "/metrics", &[("Authorization", "Bearer wrong")]).0,
        401
    );
    let query = format!("/api/x?access_token={CANARY}&password={CANARY}");
    let _ = http(
        addr,
        &query,
        &[
            ("Authorization", bearer.as_str()),
            ("Cookie", &format!("s={CANARY}")),
        ],
    );
    p.signal("TERM");
    assert_eq!(p.wait_exit(), 0);
    let output = p.all_output();
    assert!(
        !output.contains(CANARY),
        "a secret reached the process output:\n{output}"
    );
}

// ---- other commands ---------------------------------------------------------

#[test]
fn print_config_redacts_secrets_and_exits_zero() {
    let db_url = format!("postgres://app:{CANARY}@db/x");
    let mut p = spawn(
        &["print-config"],
        &dev_env(&[
            ("RUSTRYX_DATABASE_URL", db_url.as_str()),
            ("RUSTRYX_AUTH_BOOTSTRAP_ADMIN_PASSWORD", CANARY),
        ]),
    );
    assert_eq!(p.wait_exit(), 0);
    let output = p.all_output();
    assert!(output.contains("[REDACTED]"), "{output}");
    assert!(!output.contains(CANARY), "{output}");
    assert!(output.contains("environment: development"));
}

#[test]
fn print_config_accepts_a_file_and_the_environment_wins() {
    let dir = std::env::temp_dir().join(format!("rustryx-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let file = dir.join("config.yaml");
    std::fs::write(
        &file,
        "server:\n  shutdown_timeout_secs: 11\n  drain_delay_secs: 3\n",
    )
    .expect("write");
    let path = file.to_string_lossy().into_owned();
    let mut p = spawn(
        &["--config", path.as_str(), "print-config", "--no-validate"],
        &[("RUSTRYX_SERVER_SHUTDOWN_TIMEOUT_SECS", "22")],
    );
    assert_eq!(p.wait_exit(), 0);
    let output = p.all_output();
    assert!(
        output.contains("shutdown_timeout_secs: 22"),
        "env beats file: {output}"
    );
    assert!(
        output.contains("drain_delay_secs: 3"),
        "file beats default: {output}"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn migrate_says_clearly_that_it_is_not_wired() {
    let mut p = spawn(&["migrate"], &dev_env(&[]));
    assert_eq!(p.wait_exit(), 1);
    let output = p.all_output();
    assert!(output.contains("not wired"), "{output}");
}

#[test]
fn migrate_refuses_an_invalid_configuration_like_serve_does() {
    let mut p = spawn(&["migrate"], &[]);
    assert_eq!(p.wait_exit(), 78);
}

#[test]
fn help_and_version_work_and_a_missing_subcommand_is_a_usage_error() {
    let mut help = spawn(&["--help"], &[]);
    assert_eq!(help.wait_exit(), 0);
    let text = help.all_output();
    for sub in ["serve", "migrate", "print-config"] {
        assert!(text.contains(sub), "{text}");
    }
    let mut version = spawn(&["--version"], &[]);
    assert_eq!(version.wait_exit(), 0);
    assert!(version.all_output().contains("rustryx"));
    let mut bare = spawn(&[], &[]);
    assert_eq!(bare.wait_exit(), 2);
}
