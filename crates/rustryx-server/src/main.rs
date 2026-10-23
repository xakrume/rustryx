//! `rustryx`: the Rustryx server binary.

use std::io;
use std::process::ExitCode;

use clap::Parser;
use rustryx_config::ProcessEnv;
use rustryx_server::commands::Host;
use rustryx_server::{Cli, execute, wiring};

fn main() -> ExitCode {
    let cli = Cli::parse();
    // Unlocked handles on purpose: holding `stdout().lock()` for the life of `serve`
    // would block the log writer thread, which writes to the same stream.
    let (mut out, mut err) = (io::stdout(), io::stderr());
    let migrator = wiring::migrator();
    let mut host = Host {
        env: &ProcessEnv,
        migrator: migrator.as_ref(),
        stdout: &mut out,
        stderr: &mut err,
        init_telemetry: true,
    };
    match execute(&cli, &mut host) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // Logging may not be up (configuration errors happen before it is), so
            // errors go to standard error as plain text. Nothing printed here can
            // contain a secret: configuration messages never carry values.
            eprintln!("rustryx: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}
