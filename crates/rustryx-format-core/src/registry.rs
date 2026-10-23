//! Registry of format handlers, keyed by repository format.

use std::collections::HashMap;
use std::sync::Arc;

use rustryx_core::model::RepoFormat;

use crate::handler::FormatHandler;

/// Failure to register a handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    /// A handler for this format is already registered.
    #[error("a handler for format {0} is already registered")]
    Duplicate(RepoFormat),
    /// The handler declares no formats.
    #[error("handler declares no formats")]
    NoFormats,
}

/// Maps each [`RepoFormat`] to its handler.
///
/// One handler may serve several labels: the OCI crate registers a single handler
/// that returns `[RepoFormat::Docker, RepoFormat::Oci]` from `formats()`, and both
/// labels resolve to the same `Arc`. Registration is all-or-nothing, so a failed
/// `register` leaves the registry unchanged.
#[derive(Clone, Default)]
pub struct FormatRegistry {
    handlers: HashMap<RepoFormat, Arc<dyn FormatHandler>>,
}

impl FormatRegistry {
    /// Creates an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `handler` for every format it declares.
    ///
    /// # Errors
    /// [`RegistryError::Duplicate`] when any of its formats is taken,
    /// [`RegistryError::NoFormats`] when it declares none.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "owning the Arc is the natural registration signature; it is cloned once per declared format"
    )]
    pub fn register(&mut self, handler: Arc<dyn FormatHandler>) -> Result<(), RegistryError> {
        let formats = handler.formats();
        if formats.is_empty() {
            return Err(RegistryError::NoFormats);
        }
        if let Some(taken) = formats.iter().find(|f| self.handlers.contains_key(f)) {
            return Err(RegistryError::Duplicate(*taken));
        }
        for format in formats {
            self.handlers.insert(*format, Arc::clone(&handler));
        }
        Ok(())
    }

    /// Builder-style [`FormatRegistry::register`].
    ///
    /// # Errors
    /// See [`FormatRegistry::register`].
    pub fn with(mut self, handler: Arc<dyn FormatHandler>) -> Result<Self, RegistryError> {
        self.register(handler)?;
        Ok(self)
    }

    /// The handler for a format.
    #[must_use]
    pub fn get(&self, format: RepoFormat) -> Option<&Arc<dyn FormatHandler>> {
        self.handlers.get(&format)
    }

    /// The formats that have a handler, in a stable order.
    #[must_use]
    pub fn formats(&self) -> Vec<RepoFormat> {
        let mut formats: Vec<_> = self.handlers.keys().copied().collect();
        formats.sort();
        formats
    }
}
