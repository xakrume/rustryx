//! Running real package clients against a [`TestServer`](crate::TestServer).
//!
//! Real-client tests are slow and need tools that not every machine has, so they are gated:
//!
//! * they run only with the environment variable `RUSTRYX_IT=1`;
//! * each one first asks [`require`] for its tool and returns early when it says no. `require`
//!   prints a line starting with `SKIP` that names the reason (tests off, tool missing, Docker
//!   daemon down, `PyPI` unreachable for `twine`), and the test passes.
//!
//! ```ignore
//! #[tokio::test(flavor = "multi_thread")]
//! async fn npm_publish_and_install() {
//!     rustryx_testserver::require_tool!(Tool::Npm);
//!     // ... build a TestServer, write files into a TempProject, call run(Tool::Npm, ..)
//! }
//! ```
//!
//! The runner never inherits stdin (a prompt must fail, not hang), captures stdout and stderr,
//! kills the client on a timeout (`RUSTRYX_IT_TIMEOUT_SECS`, default 300) and keeps the
//! user's own credentials out of reach: `docker`, `helm` and the Dart container run with
//! configuration directories inside the working directory unless the caller sets them.

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use tokio::sync::OnceCell;

use crate::venv;

/// Default limit of one client run.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
/// Limit of the quick probes (`docker info`, `docker image inspect`).
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
/// The Dart SDK image `Tool::DartDocker` runs in; `RUSTRYX_DART_IMAGE` replaces it.
const DEFAULT_DART_IMAGE: &str = "dart:stable";
/// Prefixes of the secrets the harness generates, scrubbed from captured output.
const SECRET_PREFIXES: [&str; 2] = ["ryx_", "v2t_"];
/// A token is at least this many characters after its prefix.
const MIN_SECRET_TAIL: usize = 20;

/// A package client the harness can run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    /// `mvn`.
    Mvn,
    /// `npm`.
    Npm,
    /// `pip`, from a Python venv created on first use.
    Pip,
    /// `twine`, from the same venv (installed from `PyPI` when missing).
    Twine,
    /// `skopeo`.
    Skopeo,
    /// `docker`.
    Docker,
    /// `helm`.
    Helm,
    /// `dart`, run as a command inside the `dart:stable` Docker image: the machine has no
    /// Dart SDK.
    DartDocker,
}

impl Tool {
    /// Every tool.
    pub const ALL: [Tool; 8] = [
        Tool::Mvn,
        Tool::Npm,
        Tool::Pip,
        Tool::Twine,
        Tool::Skopeo,
        Tool::Docker,
        Tool::Helm,
        Tool::DartDocker,
    ];

    /// The name used in SKIP lines.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Tool::Mvn => "mvn",
            Tool::Npm => "npm",
            Tool::Pip => "pip",
            Tool::Twine => "twine",
            Tool::Skopeo => "skopeo",
            Tool::Docker => "docker",
            Tool::Helm => "helm",
            Tool::DartDocker => "dart (in docker)",
        }
    }

    /// The executable that must be on `PATH`: `python3` for the venv tools, `docker` for the
    /// Dart container.
    #[must_use]
    pub const fn program(self) -> &'static str {
        match self {
            Tool::Mvn => "mvn",
            Tool::Npm => "npm",
            Tool::Pip | Tool::Twine => "python3",
            Tool::Skopeo => "skopeo",
            Tool::Docker | Tool::DartDocker => "docker",
            Tool::Helm => "helm",
        }
    }

    /// Whether the executable is on `PATH`. Cheap and cached for the process. A true answer
    /// does not mean the tool can run (Docker may be stopped, `twine` may be missing and
    /// `PyPI` unreachable): [`require`] checks that too.
    #[must_use]
    pub fn available(self) -> bool {
        static CACHE: OnceLock<Vec<(Tool, bool)>> = OnceLock::new();
        let cache = CACHE.get_or_init(|| {
            // One scan of `PATH` per tool per process. A few `stat` calls, done once at the
            // top of a test before anything async runs in earnest.
            Tool::ALL
                .iter()
                .map(|tool| (*tool, on_path(tool.program())))
                .collect()
        });
        cache
            .iter()
            .find(|(tool, _)| *tool == self)
            .is_some_and(|(_, found)| *found)
    }
}

fn on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| is_executable(&dir.join(program)))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// True when real-client tests are switched on (`RUSTRYX_IT=1`).
#[must_use]
pub fn it_enabled() -> bool {
    std::env::var("RUSTRYX_IT").is_ok_and(|value| value == "1")
}

/// Decides whether a real-client test may run. Returns `false` after printing
/// `SKIP: <reason>` when the tests are off or the tool cannot run here; the test then returns
/// and passes.
pub async fn require(tool: Tool) -> bool {
    match skip_reason(tool).await {
        None => true,
        Some(reason) => {
            println!("SKIP: {reason}");
            false
        }
    }
}

/// Why `tool` cannot run now, or `None` when it can.
pub async fn skip_reason(tool: Tool) -> Option<String> {
    if !it_enabled() {
        return Some(format!(
            "{}: real-client tests are off (set RUSTRYX_IT=1 to run them)",
            tool.name()
        ));
    }
    if !tool.available() {
        return Some(format!(
            "{}: `{}` is not on PATH on this machine",
            tool.name(),
            tool.program()
        ));
    }
    match tool {
        Tool::Docker | Tool::DartDocker if !docker_ready().await => {
            return Some(format!(
                "{}: the Docker daemon is not reachable",
                tool.name()
            ));
        }
        Tool::DartDocker if !dart_image_present().await => {
            return Some(format!(
                "{}: image {} is not present locally (run `docker pull {}`; it is not pulled automatically, it is over a gigabyte)",
                tool.name(),
                dart_image(),
                dart_image()
            ));
        }
        Tool::Pip => {
            if let Err(reason) = venv::shared_venv().await {
                return Some(format!("{}: no Python venv: {reason}", tool.name()));
            }
        }
        Tool::Twine => {
            if let Err(reason) = venv::twine_bin().await {
                return Some(format!("{}: {reason}", tool.name()));
            }
        }
        _ => {}
    }
    None
}

/// `return`s from the enclosing async test (printing `SKIP`) unless the tool can run.
///
/// ```ignore
/// rustryx_testserver::require_tool!(rustryx_testserver::Tool::Mvn);
/// ```
#[macro_export]
macro_rules! require_tool {
    ($tool:expr) => {
        if !$crate::require($tool).await {
            return;
        }
    };
}

/// What a client run produced.
#[derive(Debug, Clone)]
pub struct Output {
    /// The command line, with secrets scrubbed.
    pub command: String,
    /// Exit code; `None` when the process could not start, was killed by a signal or timed out.
    pub code: Option<i32>,
    /// Captured standard output (secrets scrubbed).
    pub stdout: String,
    /// Captured standard error (secrets scrubbed).
    pub stderr: String,
    /// True when the run exceeded its timeout and was killed.
    pub timed_out: bool,
}

impl Output {
    /// Exit code 0.
    #[must_use]
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// Standard output followed by standard error, for assertions on a message wherever the
    /// client prints it.
    #[must_use]
    pub fn combined(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }

    /// Panics with the command and both streams unless the run succeeded.
    ///
    /// # Panics
    /// When the exit code is not 0.
    #[allow(
        clippy::panic,
        reason = "test harness: the assertion that makes a failed client run fail the test with its output"
    )]
    pub fn assert_success(&self) {
        assert!(
            self.success(),
            "command failed (code {:?}, timed out: {})\n$ {}\n--- stdout\n{}\n--- stderr\n{}",
            self.code,
            self.timed_out,
            self.command,
            self.stdout,
            self.stderr
        );
    }

    fn failed(command: String, message: String) -> Self {
        Self {
            command,
            code: None,
            stdout: String::new(),
            stderr: message,
            timed_out: false,
        }
    }
}

/// Removes anything that looks like a token the harness generated.
pub(crate) fn scrub(text: &str) -> String {
    let mut out = text.to_owned();
    for prefix in SECRET_PREFIXES {
        let mut from = 0;
        while let Some(found) = out[from..].find(prefix) {
            let start = from + found;
            let tail_start = start + prefix.len();
            let tail_len = out[tail_start..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .count();
            if tail_len >= MIN_SECRET_TAIL {
                out.replace_range(start..tail_start + tail_len, "[REDACTED-TOKEN]");
                from = start + "[REDACTED-TOKEN]".len();
            } else {
                from = tail_start;
            }
        }
    }
    out
}

/// A process to run. Internal: tests use [`run`] and [`run_program`].
#[derive(Debug, Clone)]
pub(crate) struct Command {
    program: OsString,
    args: Vec<String>,
    env: Vec<(String, String)>,
    workdir: Option<PathBuf>,
    timeout: Duration,
    container: Option<String>,
}

impl Command {
    pub(crate) fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            program: program.as_ref().to_owned(),
            args: Vec::new(),
            env: Vec::new(),
            workdir: None,
            timeout: default_timeout(),
            container: None,
        }
    }

    pub(crate) fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.args
            .extend(args.into_iter().map(|arg| arg.as_ref().to_owned()));
        self
    }

    pub(crate) fn arg(mut self, arg: impl AsRef<str>) -> Self {
        self.args.push(arg.as_ref().to_owned());
        self
    }

    /// Sets a variable; a later call for the same name replaces the earlier one, so explicit
    /// settings of the caller win over the defaults of a tool.
    pub(crate) fn env(mut self, key: &str, value: &str) -> Self {
        self.env.retain(|(existing, _)| existing != key);
        self.env.push((key.to_owned(), value.to_owned()));
        self
    }

    pub(crate) fn workdir(mut self, dir: &Path) -> Self {
        self.workdir = Some(dir.to_owned());
        self
    }

    pub(crate) fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Names a container to remove if the run times out (killing the `docker run` client does
    /// not stop the container).
    pub(crate) fn container(mut self, name: String) -> Self {
        self.container = Some(name);
        self
    }

    fn display(&self) -> String {
        let mut text = self.program.to_string_lossy().into_owned();
        for arg in &self.args {
            let _ = write!(text, " {arg}");
        }
        scrub(&text)
    }
}

/// The timeout of a run: `RUSTRYX_IT_TIMEOUT_SECS` or five minutes.
fn default_timeout() -> Duration {
    std::env::var("RUSTRYX_IT_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map_or(DEFAULT_TIMEOUT, Duration::from_secs)
}

/// Spawns the process, captures its output and enforces the timeout; a run that times out
/// also removes its container, if it named one.
pub(crate) async fn run_command(command: Command) -> Output {
    let container = command.container.clone();
    let output = execute(command).await;
    if output.timed_out
        && let Some(name) = container
    {
        let _ = execute(
            Command::new("docker")
                .args(["rm", "-f", &name])
                .timeout(PROBE_TIMEOUT),
        )
        .await;
    }
    output
}

async fn execute(command: Command) -> Output {
    let shown = command.display();
    let mut process = tokio::process::Command::new(&command.program);
    process
        .args(&command.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // The future is dropped on a timeout; this kills the client with it.
        .kill_on_drop(true);
    if let Some(dir) = &command.workdir {
        process.current_dir(dir);
    }
    for (key, value) in &command.env {
        process.env(key, value);
    }
    let child = match process.spawn() {
        Ok(child) => child,
        Err(error) => return Output::failed(shown, format!("cannot start the command: {error}")),
    };
    match tokio::time::timeout(command.timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => Output {
            command: shown,
            code: output.status.code(),
            stdout: scrub(&String::from_utf8_lossy(&output.stdout)),
            stderr: scrub(&String::from_utf8_lossy(&output.stderr)),
            timed_out: false,
        },
        Ok(Err(error)) => Output::failed(shown, format!("cannot wait for the command: {error}")),
        Err(_elapsed) => Output {
            command: shown,
            code: None,
            stdout: String::new(),
            stderr: format!("timed out after {:?} and was killed", command.timeout),
            timed_out: true,
        },
    }
}

/// Runs any program with captured output, a timeout and the same hygiene as [`run`] (no stdin,
/// killed on timeout). For helpers that are not in [`Tool`], such as `curl` or `sh`.
pub async fn run_program(
    program: &str,
    args: &[&str],
    env: &[(&str, &str)],
    workdir: &Path,
    timeout: Duration,
) -> Output {
    let mut command = Command::new(program)
        .args(args)
        .workdir(workdir)
        .timeout(timeout);
    for (key, value) in env {
        command = command.env(key, value);
    }
    run_command(command).await
}

/// Runs `tool` with `args` in `workdir`, with `env` added to the environment, and the
/// default timeout.
///
/// Per tool:
///
/// * `Pip` and `Twine` run the executables of the shared Python venv (created on first use;
///   `twine` is installed from `PyPI` when missing). Pip is called with
///   `PIP_CONFIG_FILE=/dev/null`, `PIP_NO_INPUT=1` and no version check. Do not install into
///   the shared venv: use `--target <dir>`.
/// * `Docker` and `Helm` use configuration directories inside `workdir` (`DOCKER_CONFIG`,
///   `HELM_*`) so a login never touches the user's credentials; `DOCKER_HOST` is taken from
///   the current docker context, so the isolated config still finds the daemon.
/// * `DartDocker` runs `args` as a command inside `dart:stable`: `docker run --rm --add-host
///   host.docker.internal:host-gateway --add-host localhost:host-gateway -v <workdir>:/work
///   -w /work ... <args>`. Inside the container `http://localhost:PORT` and
///   `http://host.docker.internal:PORT` both reach a server started with `bind_all(true)`; pub
///   sends a token only to https or to `localhost`, so use the latter (see
///   `TestServer::localhost_repository_url`). Every `env`
///   entry becomes an `-e` of the container (so `PUB_HOSTED_URL`, `PUB_TOKEN`), `HOME` and
///   `PUB_CACHE` default to directories under `/work`.
pub async fn run(tool: Tool, args: &[&str], env: &[(&str, &str)], workdir: &Path) -> Output {
    run_with_timeout(tool, args, env, workdir, default_timeout()).await
}

/// [`run`] with an explicit timeout.
pub async fn run_with_timeout(
    tool: Tool,
    args: &[&str],
    env: &[(&str, &str)],
    workdir: &Path,
    timeout: Duration,
) -> Output {
    match prepare(tool, args, env, workdir).await {
        Ok(command) => run_command(command.timeout(timeout)).await,
        Err(message) => Output::failed(tool.name().to_owned(), message),
    }
}

async fn prepare(
    tool: Tool,
    args: &[&str],
    env: &[(&str, &str)],
    workdir: &Path,
) -> Result<Command, String> {
    let base = Command::new(tool.program()).workdir(workdir).env("CI", "1");
    let mut command = match tool {
        Tool::Mvn | Tool::Skopeo => base.args(args),
        Tool::Npm => base
            .args(args)
            .env("npm_config_update_notifier", "false")
            .env("npm_config_fund", "false"),
        Tool::Pip => Command::new(venv::pip_bin().await?)
            .workdir(workdir)
            .env("CI", "1")
            .args(args)
            .env("PIP_DISABLE_PIP_VERSION_CHECK", "1")
            .env("PIP_CONFIG_FILE", "/dev/null")
            .env("PIP_NO_INPUT", "1"),
        Tool::Twine => Command::new(venv::twine_bin().await?)
            .workdir(workdir)
            .env("CI", "1")
            .args(args),
        Tool::Docker => docker_base(base.args(args), workdir).await?,
        Tool::Helm => helm_base(base.args(args), workdir),
        Tool::DartDocker => dart_command(base, args, env, workdir).await?,
    };
    if tool != Tool::DartDocker {
        for (key, value) in env {
            command = command.env(key, value);
        }
    }
    Ok(command)
}

/// `DOCKER_CONFIG` inside the working directory and the daemon of the current context.
async fn docker_base(command: Command, workdir: &Path) -> Result<Command, String> {
    let config = workdir.join(".docker-config");
    tokio::fs::create_dir_all(&config)
        .await
        .map_err(|error| format!("cannot create {}: {error}", config.display()))?;
    let mut command = command.env("DOCKER_CONFIG", &config.to_string_lossy());
    if let Some(host) = docker_host().await {
        command = command.env("DOCKER_HOST", &host);
    }
    Ok(command)
}

/// Helm state inside the working directory, so `helm registry login` cannot touch the user's.
fn helm_base(command: Command, workdir: &Path) -> Command {
    let at = |relative: &str| {
        workdir
            .join(".helm")
            .join(relative)
            .to_string_lossy()
            .into_owned()
    };
    command
        .env("HELM_REGISTRY_CONFIG", &at("registry.json"))
        .env("HELM_REPOSITORY_CONFIG", &at("repositories.yaml"))
        .env("HELM_REPOSITORY_CACHE", &at("repository-cache"))
        .env("HELM_CACHE_HOME", &at("cache"))
        .env("HELM_CONFIG_HOME", &at("config"))
        .env("HELM_DATA_HOME", &at("data"))
}

fn dart_image() -> String {
    std::env::var("RUSTRYX_DART_IMAGE").unwrap_or_else(|_| DEFAULT_DART_IMAGE.to_owned())
}

/// Composes `docker run ... <image> <args>` for a Dart command.
async fn dart_command(
    base: Command,
    args: &[&str],
    env: &[(&str, &str)],
    workdir: &Path,
) -> Result<Command, String> {
    let absolute = tokio::fs::canonicalize(workdir)
        .await
        .map_err(|error| format!("cannot resolve {}: {error}", workdir.display()))?;
    let name = format!("rustryx-it-{}", uuid::Uuid::now_v7().simple());
    let mut command = docker_base(base, &absolute).await?;
    command = command
        .container(name.clone())
        .args([
            "run",
            "--rm",
            "--name",
            &name,
            "--add-host",
            "host.docker.internal:host-gateway",
            // `dart pub token add` refuses a plain http URL unless the host is `localhost`
            // ("insecure repositories cannot use authentication"), and inside the container
            // `localhost` is the container. This extra /etc/hosts line makes it the host too
            // (the resolver returns both addresses and the client falls through to the one
            // that answers), so an authenticated pub test uses `http://localhost:PORT`.
            "--add-host",
            "localhost:host-gateway",
            "-v",
        ])
        .arg(format!("{}:/work", absolute.display()))
        .args(["-w", "/work"]);
    if let Some(user) = owner_of(&absolute).await {
        // The files the container writes must belong to the user who runs the tests, or the
        // temporary directory cannot be removed afterwards on Linux.
        command = command.args(["--user", &user]);
    }
    command = command.args(["-e", "HOME=/work/.home", "-e", "PUB_CACHE=/work/.pub-cache"]);
    for (key, value) in env {
        // The value travels in the environment of the docker client (`-e KEY` without a
        // value reads it from there), so tokens are not part of the argument list.
        command = command.arg("-e").arg(key).env(key, value);
    }
    command = command.arg(dart_image()).args(args);
    Ok(command)
}

#[cfg(unix)]
async fn owner_of(dir: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let meta = tokio::fs::metadata(dir).await.ok()?;
    Some(format!("{}:{}", meta.uid(), meta.gid()))
}

#[cfg(not(unix))]
async fn owner_of(_dir: &Path) -> Option<String> {
    None
}

/// `DOCKER_HOST`, or the endpoint of the current docker context.
async fn docker_host() -> Option<String> {
    static HOST: OnceCell<Option<String>> = OnceCell::const_new();
    HOST.get_or_init(|| async {
        if let Ok(host) = std::env::var("DOCKER_HOST")
            && !host.is_empty()
        {
            return Some(host);
        }
        let output = run_command(
            Command::new("docker")
                .args([
                    "context",
                    "inspect",
                    "--format",
                    "{{.Endpoints.docker.Host}}",
                ])
                .timeout(PROBE_TIMEOUT),
        )
        .await;
        let host = output.stdout.trim().to_owned();
        (output.success() && !host.is_empty()).then_some(host)
    })
    .await
    .clone()
}

async fn docker_ready() -> bool {
    static READY: OnceCell<bool> = OnceCell::const_new();
    *READY
        .get_or_init(|| async {
            let mut command = Command::new("docker")
                .args(["info", "--format", "{{.ServerVersion}}"])
                .timeout(PROBE_TIMEOUT);
            if let Some(host) = docker_host().await {
                command = command.env("DOCKER_HOST", &host);
            }
            run_command(command).await.success()
        })
        .await
}

async fn dart_image_present() -> bool {
    let mut command = Command::new("docker")
        .args(["image", "inspect", "--format", "{{.Id}}", &dart_image()])
        .timeout(PROBE_TIMEOUT);
    if let Some(host) = docker_host().await {
        command = command.env("DOCKER_HOST", &host);
    }
    run_command(command).await.success()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(tail: &str) -> String {
        format!("ryx_{tail}")
    }

    #[test]
    fn scrub_removes_generated_tokens_and_keeps_short_lookalikes() {
        let long = token("A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8S9t0U1v");
        let text = format!(
            "Authorization: Bearer {long}\nand v2t_abcdefghijklmnopqrstuvwxyz0123 twice {long}"
        );
        let scrubbed = scrub(&text);
        assert!(!scrubbed.contains("A1b2C3d4"));
        assert!(!scrubbed.contains("abcdefghijklmnop"));
        assert_eq!(scrubbed.matches("[REDACTED-TOKEN]").count(), 3);
        // A word that merely starts with the prefix is not a token.
        assert_eq!(scrub("ryx_short and v2t_x"), "ryx_short and v2t_x");
        assert_eq!(scrub("nothing to hide"), "nothing to hide");
    }

    #[test]
    fn command_env_replaces_earlier_values_and_display_hides_secrets() {
        let command = Command::new("echo")
            .arg("--token")
            .arg(token("A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8S9t0U1v"))
            .env("A", "1")
            .env("A", "2");
        assert_eq!(command.env, vec![("A".to_owned(), "2".to_owned())]);
        let shown = command.display();
        assert!(
            shown.starts_with("echo --token [REDACTED-TOKEN]"),
            "{shown}"
        );
    }

    #[test]
    fn tools_have_distinct_names_and_known_programs() {
        let mut names: Vec<_> = Tool::ALL.iter().map(|tool| tool.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Tool::ALL.len());
        assert_eq!(Tool::Twine.program(), "python3");
        assert_eq!(Tool::DartDocker.program(), "docker");
        // `available` is a pure PATH lookup and cached: asking twice gives one answer.
        for tool in Tool::ALL {
            assert_eq!(tool.available(), tool.available());
        }
    }

    #[tokio::test]
    async fn skip_reason_names_why_a_test_does_not_run() {
        let reason = skip_reason(Tool::Mvn).await;
        if !it_enabled() {
            assert!(reason.unwrap().contains("RUSTRYX_IT=1"));
        } else if !Tool::Mvn.available() {
            assert!(reason.unwrap().contains("not on PATH"));
        }
        assert_eq!(
            require(Tool::Mvn).await,
            skip_reason(Tool::Mvn).await.is_none()
        );
    }

    #[tokio::test]
    async fn npm_and_mvn_get_their_defaults_and_the_callers_env_wins() {
        let dir = tempfile::tempdir().unwrap();
        let npm = prepare(
            Tool::Npm,
            &["ping"],
            &[("npm_config_fund", "true")],
            dir.path(),
        )
        .await
        .unwrap();
        let env =
            |c: &Command, key: &str| c.env.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        assert_eq!(env(&npm, "npm_config_fund").as_deref(), Some("true"));
        assert_eq!(
            env(&npm, "npm_config_update_notifier").as_deref(),
            Some("false")
        );
        assert_eq!(env(&npm, "CI").as_deref(), Some("1"));
        assert_eq!(npm.args, ["ping"]);
        let mvn = prepare(Tool::Mvn, &["-v"], &[], dir.path()).await.unwrap();
        assert_eq!(mvn.program, OsString::from("mvn"));
        assert_eq!(mvn.workdir.as_deref(), Some(dir.path()));
    }

    #[tokio::test]
    async fn helm_state_lives_in_the_working_directory() {
        let dir = tempfile::tempdir().unwrap();
        let helm = prepare(Tool::Helm, &["version"], &[], dir.path())
            .await
            .unwrap();
        for key in [
            "HELM_REGISTRY_CONFIG",
            "HELM_REPOSITORY_CONFIG",
            "HELM_REPOSITORY_CACHE",
            "HELM_CACHE_HOME",
            "HELM_CONFIG_HOME",
            "HELM_DATA_HOME",
        ] {
            let value = &helm.env.iter().find(|(k, _)| k == key).unwrap().1;
            assert!(
                value.starts_with(&*dir.path().to_string_lossy()),
                "{key}={value}"
            );
        }
    }

    #[tokio::test]
    async fn docker_login_state_is_isolated_and_the_dart_container_is_composed() {
        let dir = tempfile::tempdir().unwrap();
        let docker = prepare(Tool::Docker, &["login"], &[], dir.path())
            .await
            .unwrap();
        let config = &docker
            .env
            .iter()
            .find(|(k, _)| k == "DOCKER_CONFIG")
            .unwrap()
            .1;
        assert!(config.starts_with(&*dir.path().to_string_lossy()));
        assert!(
            std::path::Path::new(config).is_dir(),
            "the isolated config directory exists"
        );

        let dart = prepare(
            Tool::DartDocker,
            &["dart", "pub", "get"],
            &[("PUB_HOSTED_URL", "http://localhost:1")],
            dir.path(),
        )
        .await
        .unwrap();
        let args = dart.args.join(" ");
        assert!(args.starts_with("run --rm --name rustryx-it-"), "{args}");
        assert!(args.contains("--add-host host.docker.internal:host-gateway"));
        assert!(args.contains("--add-host localhost:host-gateway"));
        assert!(args.contains(":/work -w /work"));
        assert!(args.contains("-e HOME=/work/.home -e PUB_CACHE=/work/.pub-cache"));
        // The value travels in the docker client's environment, not in the argument list.
        assert!(args.contains("-e PUB_HOSTED_URL "), "{args}");
        assert!(!args.contains("http://localhost:1"));
        assert!(
            dart.env
                .iter()
                .any(|(k, v)| k == "PUB_HOSTED_URL" && v == "http://localhost:1")
        );
        assert!(
            args.ends_with(&format!("{} dart pub get", dart_image())),
            "{args}"
        );
        assert!(dart.container.is_some());
        #[cfg(unix)]
        assert!(args.contains("--user "), "{args}");
    }
}
