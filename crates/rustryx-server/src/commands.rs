//! Dispatch of the subcommands.

use std::fmt;
use std::io::Write;

use anyhow::Context;
use rustryx_config::{
    AppConfig, ConfigError, EnvSource, load, load_without_rules, resolve_config_path,
};

use crate::cli::{Cli, Command};
use crate::migrate::Migrator;
use crate::serve;

/// Failure of a subcommand, with the exit code it maps to.
#[derive(Debug)]
pub enum CliError {
    /// The configuration is unusable: exit code 78 (`EX_CONFIG`).
    Config(ConfigError),
    /// Anything else: exit code 1.
    Other(anyhow::Error),
}

impl CliError {
    /// Process exit code for this failure.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Config(error) => error.exit_code(),
            Self::Other(_) => 1,
        }
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(error) => write!(f, "{error}"),
            Self::Other(error) => write!(f, "{error:#}"),
        }
    }
}

impl std::error::Error for CliError {}

impl From<ConfigError> for CliError {
    fn from(error: ConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<anyhow::Error> for CliError {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}

/// What the commands run against: environment, ports and output streams.
///
/// Everything process-global is injected so that the commands are testable in
/// process: the environment (no `set_var`), the migrator, the output streams, and
/// whether to install the global logging subscriber (which can only happen once).
pub struct Host<'a> {
    /// Environment variables.
    pub env: &'a dyn EnvSource,
    /// Implementation of `rustryx migrate`.
    pub migrator: &'a dyn Migrator,
    /// Standard output (the effective configuration, migration summary).
    pub stdout: &'a mut dyn Write,
    /// Standard error (warnings).
    pub stderr: &'a mut dyn Write,
    /// Install the global logging subscriber for `migrate`.
    pub init_telemetry: bool,
}

/// Runs one subcommand.
///
/// `serve` blocks until the process is asked to stop.
///
/// # Errors
/// [`CliError::Config`] when the configuration does not load or validate,
/// [`CliError::Other`] for everything else.
pub fn execute(cli: &Cli, host: &mut Host<'_>) -> Result<(), CliError> {
    let path = resolve_config_path(cli.config.as_deref(), host.env);
    match &cli.command {
        Command::PrintConfig { no_validate } => print_config(path.as_deref(), *no_validate, host),
        Command::Migrate => {
            let loaded = load(path.as_deref(), host.env)?;
            warn_all(host, &loaded.warnings)?;
            migrate(&loaded.config, host)
        }
        Command::Serve => {
            // The whole configuration is checked here, before any port is opened.
            let loaded = load(path.as_deref(), host.env)?;
            serve::run(&loaded.config, &loaded.warnings)?;
            Ok(())
        }
    }
}

fn print_config(
    path: Option<&std::path::Path>,
    no_validate: bool,
    host: &mut Host<'_>,
) -> Result<(), CliError> {
    let config = if no_validate {
        load_without_rules(path, host.env)?
    } else {
        let loaded = load(path, host.env)?;
        warn_all(host, &loaded.warnings)?;
        loaded.config
    };
    let yaml = config.to_redacted_yaml()?;
    host.stdout
        .write_all(yaml.as_bytes())
        .context("cannot write the configuration to standard output")?;
    Ok(())
}

fn migrate(config: &AppConfig, host: &mut Host<'_>) -> Result<(), CliError> {
    let runtime = serve::build_runtime().context("cannot build the runtime")?;
    let enter = runtime.enter();
    let guard = if host.init_telemetry {
        Some(
            rustryx_observability::init(&crate::wiring::observability_options(config))
                .context("cannot initialise logging")?,
        )
    } else {
        None
    };
    let outcome = runtime.block_on(host.migrator.migrate(config));
    drop(enter);
    drop(runtime);
    // Flush logs before reporting the outcome.
    let flushed = guard.map(rustryx_observability::ObservabilityGuard::shutdown);
    let summary = outcome?;
    if let Some(Err(error)) = flushed {
        writeln!(host.stderr, "warning: {error}").context("cannot write to standard error")?;
    }
    writeln!(host.stdout, "{summary}").context("cannot write to standard output")?;
    Ok(())
}

fn warn_all(host: &mut Host<'_>, warnings: &[String]) -> Result<(), CliError> {
    for warning in warnings {
        writeln!(host.stderr, "warning: {warning}").context("cannot write to standard error")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
