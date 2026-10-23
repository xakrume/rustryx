//! Regular expression compilation for `=~` and `matches`.

use regex::{Regex, RegexBuilder};

use super::MatchMode;
use crate::selector::SelectorLimits;
use crate::selector::error::SelectorCompileError;

/// Nesting limit handed to the `regex` parser. A pattern needs nowhere near 32
/// levels of groups to describe a path, and the default of 250 only widens the
/// parser's own recursion.
const REGEX_NEST_LIMIT: u32 = 32;

pub(super) fn build_regex(
    pattern: &str,
    mode: MatchMode,
    limits: &SelectorLimits,
    pos: usize,
) -> Result<Regex, SelectorCompileError> {
    let fail = |message: String| SelectorCompileError::Regex {
        pos,
        message: message.replace('\n', " "),
    };
    if pattern.len() > limits.max_regex_pattern_bytes {
        return Err(fail(format!(
            "pattern is {} bytes long, the limit is {}",
            pattern.len(),
            limits.max_regex_pattern_bytes
        )));
    }
    let build = |source: &str| {
        RegexBuilder::new(source)
            .size_limit(limits.max_regex_program_bytes)
            .dfa_size_limit(limits.max_regex_program_bytes)
            .nest_limit(REGEX_NEST_LIMIT)
            .build()
            .map_err(|e| fail(e.to_string()))
    };
    // Compiled alone first: a pattern that is not valid by itself (for example
    // "a)|(b") could become valid inside the anchoring wrapper below and then
    // mean something the author did not write.
    let plain = build(pattern)?;
    match mode {
        MatchMode::Partial => Ok(plain),
        // `\A` and `\z` instead of `^` and `$`: a `(?m)` flag inside the pattern
        // cannot turn them into line anchors, and the group keeps the flag local.
        MatchMode::Full => build(&format!(r"\A(?:{pattern})\z")),
    }
}
