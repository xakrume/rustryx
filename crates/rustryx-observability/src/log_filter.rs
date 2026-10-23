//! The log level filter: comma separated `level` or `target=level` directives.
//!
//! This is the subset of `tracing_subscriber::EnvFilter` syntax that
//! `rustryx-config` validates. It is implemented here instead of using
//! `EnvFilter` because the filter must be applied to *events only* (see
//! [`LogFilter::enabled`]): a global `EnvFilter` also disables spans below the
//! level, and with them the `request_id` that every log line inherits from the
//! request span.

use tracing::Metadata;
use tracing_subscriber::filter::LevelFilter;

use crate::ObservabilityError;

/// Parsed log filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogFilter {
    default: LevelFilter,
    /// Directives with a target, longest prefix wins.
    targets: Vec<(String, LevelFilter)>,
}

impl LogFilter {
    /// Parses `info`, `debug,hyper=warn`, `warn,rustryx_api=trace`, ...
    ///
    /// A bare level sets the default (the last one wins); without one the default is `info`.
    ///
    /// # Errors
    /// [`ObservabilityError::LogFilter`] for an unknown level or an empty target.
    pub fn parse(text: &str) -> Result<Self, ObservabilityError> {
        let mut default = None;
        let mut targets = Vec::new();
        for directive in text.split(',').map(str::trim).filter(|d| !d.is_empty()) {
            match directive.split_once('=') {
                Some((target, level)) => {
                    let target = target.trim().replace('-', "_");
                    if target.is_empty() {
                        return Err(ObservabilityError::LogFilter(
                            "a directive has an empty target".to_owned(),
                        ));
                    }
                    targets.push((target, parse_level(level.trim())?));
                }
                None => default = Some(parse_level(directive)?),
            }
        }
        targets.sort_by_key(|(target, _)| std::cmp::Reverse(target.len()));
        Ok(Self {
            default: default.unwrap_or(LevelFilter::INFO),
            targets,
        })
    }

    /// Level that applies to `target`.
    #[must_use]
    pub fn level_for(&self, target: &str) -> LevelFilter {
        self.targets
            .iter()
            .find(|(prefix, _)| target_matches(target, prefix))
            .map_or(self.default, |(_, level)| *level)
    }

    /// Decides for one callsite.
    ///
    /// Events follow the filter. Spans of this workspace (`rustryx_*`) are always
    /// enabled so that log lines keep their request context whatever the level;
    /// spans of other crates follow the filter like events.
    #[must_use]
    pub fn enabled(&self, meta: &Metadata<'_>) -> bool {
        if meta.is_span() && meta.target().starts_with("rustryx") {
            return true;
        }
        self.level_for(meta.target()) >= *meta.level()
    }

    /// The most verbose level any directive allows, as a hint that lets `tracing`
    /// skip callsites nobody wants.
    #[must_use]
    pub fn max_level(&self) -> LevelFilter {
        self.targets
            .iter()
            .map(|(_, level)| *level)
            .chain(std::iter::once(self.default))
            .max()
            .unwrap_or(LevelFilter::INFO)
    }
}

/// `target` equals `prefix` or continues it at a module boundary.
fn target_matches(target: &str, prefix: &str) -> bool {
    target == prefix
        || target
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with("::"))
}

fn parse_level(text: &str) -> Result<LevelFilter, ObservabilityError> {
    text.to_ascii_lowercase()
        .parse::<LevelFilter>()
        .map_err(|_| ObservabilityError::LogFilter(format!("unknown level `{text}`")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_level_sets_the_default() {
        let f = LogFilter::parse("debug").expect("parses");
        assert_eq!(f.level_for("anything"), LevelFilter::DEBUG);
        assert_eq!(
            LogFilter::parse("").expect("parses").level_for("x"),
            LevelFilter::INFO
        );
    }

    #[test]
    fn longest_target_prefix_wins_and_respects_module_boundaries() {
        let f = LogFilter::parse("warn,rustryx_api=debug,rustryx_api::auth=trace").expect("parses");
        assert_eq!(f.level_for("rustryx_api::router"), LevelFilter::DEBUG);
        assert_eq!(f.level_for("rustryx_api::auth::basic"), LevelFilter::TRACE);
        assert_eq!(f.level_for("rustryx_api"), LevelFilter::DEBUG);
        assert_eq!(
            f.level_for("rustryx_apix"),
            LevelFilter::WARN,
            "no match inside a name"
        );
        assert_eq!(f.level_for("hyper"), LevelFilter::WARN);
    }

    #[test]
    fn dashes_in_targets_are_normalised() {
        let f = LogFilter::parse("hyper-util=error").expect("parses");
        assert_eq!(f.level_for("hyper_util::client"), LevelFilter::ERROR);
    }

    #[test]
    fn off_and_case_are_accepted() {
        let f = LogFilter::parse("OFF,Rustryx_Api=INFO").expect("parses");
        assert_eq!(f.level_for("other"), LevelFilter::OFF);
        assert_eq!(f.level_for("Rustryx_Api"), LevelFilter::INFO);
    }

    #[test]
    fn bad_directives_are_rejected() {
        assert!(LogFilter::parse("loud").is_err());
        assert!(LogFilter::parse("=info").is_err());
        assert!(LogFilter::parse("a=loud").is_err());
    }

    #[test]
    fn max_level_is_the_most_verbose_directive() {
        assert_eq!(
            LogFilter::parse("warn,x=trace").expect("p").max_level(),
            LevelFilter::TRACE
        );
        assert_eq!(
            LogFilter::parse("error").expect("p").max_level(),
            LevelFilter::ERROR
        );
        assert_eq!(
            LogFilter::parse("off").expect("p").max_level(),
            LevelFilter::OFF
        );
    }
}
