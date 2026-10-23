//! Process signals.

use std::io;

/// Why the process was asked to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// SIGTERM (Kubernetes, `docker stop`, systemd).
    Terminate,
    /// SIGINT (Ctrl-C).
    Interrupt,
}

/// Handlers for SIGTERM and SIGINT, installed up front.
///
/// They are installed before the listener opens, so that a node that cannot
/// receive its stop signal fails at startup instead of being unstoppable later.
#[cfg(unix)]
pub struct StopSignals {
    terminate: tokio::signal::unix::Signal,
    interrupt: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl StopSignals {
    /// Installs the handlers. Must run inside a tokio runtime.
    ///
    /// # Errors
    /// The operating system refused a handler.
    pub fn install() -> io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            terminate: signal(SignalKind::terminate())?,
            interrupt: signal(SignalKind::interrupt())?,
        })
    }

    /// Resolves on the first SIGTERM or SIGINT.
    pub async fn wait(&mut self) -> StopReason {
        tokio::select! {
            _ = self.terminate.recv() => StopReason::Terminate,
            _ = self.interrupt.recv() => StopReason::Interrupt,
        }
    }
}

/// Ctrl-C handler (platforms without SIGTERM).
#[cfg(not(unix))]
pub struct StopSignals;

#[cfg(not(unix))]
impl StopSignals {
    /// Nothing to install: `tokio::signal::ctrl_c` registers lazily.
    ///
    /// # Errors
    /// Never fails on this platform.
    pub fn install() -> io::Result<Self> {
        Ok(Self)
    }

    /// Resolves on Ctrl-C.
    pub async fn wait(&mut self) -> StopReason {
        let _ = tokio::signal::ctrl_c().await;
        StopReason::Interrupt
    }
}
