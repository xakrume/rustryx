//! Command line definition.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// `rustryx` command line.
#[derive(Debug, Parser)]
#[command(name = "rustryx", version, about = "Rustryx artifact manager")]
pub struct Cli {
    /// YAML configuration file. Without it the file named by `RUSTRYX_CONFIG` is used,
    /// and without that only defaults and `RUSTRYX_*` variables.
    #[arg(long, short = 'c', global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// Subcommands.
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Run the server. Never changes the database schema.
    Serve,
    /// Apply database migrations, under an advisory lock. The only place the schema changes.
    Migrate,
    /// Print the effective configuration as YAML, secrets redacted.
    PrintConfig {
        /// Print even when the semantic rules fail (unknown keys and wrong types
        /// are still errors).
        #[arg(long)]
        no_validate: bool,
    },
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn subcommands_and_global_config_parse() {
        let cli = Cli::try_parse_from(["rustryx", "serve"]).expect("serve");
        assert_eq!(cli.command, Command::Serve);
        assert_eq!(cli.config, None);

        let cli = Cli::try_parse_from(["rustryx", "migrate", "--config", "/etc/r.yaml"])
            .expect("migrate");
        assert_eq!(cli.command, Command::Migrate);
        assert_eq!(cli.config, Some(PathBuf::from("/etc/r.yaml")));

        let cli = Cli::try_parse_from(["rustryx", "-c", "x.yaml", "print-config", "--no-validate"])
            .expect("print");
        assert_eq!(cli.command, Command::PrintConfig { no_validate: true });
    }

    #[test]
    fn a_subcommand_is_required_and_unknown_ones_fail() {
        assert!(Cli::try_parse_from(["rustryx"]).is_err());
        assert!(Cli::try_parse_from(["rustryx", "frobnicate"]).is_err());
    }
}
