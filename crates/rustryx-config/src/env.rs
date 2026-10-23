//! Access to environment variables behind a trait.
//!
//! The loader never touches the process environment directly: tests (here and in
//! the server crate) inject a [`MapEnv`], and `std::env::set_var` is `unsafe` in
//! edition 2024 while `unsafe` is forbidden in this workspace.

use std::collections::BTreeMap;

/// Result of looking up one variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvLookup {
    /// Variable is not set.
    Missing,
    /// Variable is set to this UTF-8 value (possibly empty).
    Present(String),
    /// Variable is set but is not valid UTF-8. Using a lossy copy would silently
    /// change a secret, so it is reported as an error instead.
    NotUnicode,
}

/// Source of environment variables.
pub trait EnvSource {
    /// Looks up one variable by exact name.
    fn get(&self, name: &str) -> EnvLookup;

    /// Names of all variables, used to warn about unknown `RUSTRYX_*` ones.
    fn names(&self) -> Vec<String>;
}

/// The real process environment.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn get(&self, name: &str) -> EnvLookup {
        match std::env::var(name) {
            Ok(value) => EnvLookup::Present(value),
            Err(std::env::VarError::NotPresent) => EnvLookup::Missing,
            Err(std::env::VarError::NotUnicode(_)) => EnvLookup::NotUnicode,
        }
    }

    fn names(&self) -> Vec<String> {
        std::env::vars_os()
            .filter_map(|(name, _)| name.into_string().ok())
            .collect()
    }
}

/// In-memory environment for tests.
#[derive(Debug, Clone, Default)]
pub struct MapEnv(BTreeMap<String, String>);

impl MapEnv {
    /// An empty environment.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a copy with one more variable.
    #[must_use]
    pub fn with(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.0.insert(name.into(), value.into());
        self
    }
}

impl EnvSource for MapEnv {
    fn get(&self, name: &str) -> EnvLookup {
        self.0
            .get(name)
            .map_or(EnvLookup::Missing, |v| EnvLookup::Present(v.clone()))
    }

    fn names(&self) -> Vec<String> {
        self.0.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_env_reports_missing_and_present() {
        let env = MapEnv::new().with("A", "1").with("B", "");
        assert_eq!(env.get("A"), EnvLookup::Present("1".into()));
        assert_eq!(env.get("B"), EnvLookup::Present(String::new()));
        assert_eq!(env.get("C"), EnvLookup::Missing);
        assert_eq!(env.names(), vec!["A".to_owned(), "B".to_owned()]);
    }

    #[test]
    fn process_env_does_not_see_unset_variable() {
        let env = ProcessEnv;
        assert_eq!(
            env.get("RUSTRYX_TEST_DEFINITELY_NOT_SET_3F9A"),
            EnvLookup::Missing
        );
        assert!(env.names().iter().all(|n| !n.is_empty()));
    }
}
