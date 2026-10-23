//! Errors of the selector engine.
//!
//! Compile errors are shown to the administrator who saves a selector, so the
//! messages name the byte offset and the rule that was broken. Evaluation errors
//! never reach a client: authorization turns every one of them into "no match".

/// A selector expression was rejected when it was compiled.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SelectorCompileError {
    /// The expression has no tokens.
    #[error("expression is empty")]
    Empty,
    /// The source text is longer than the configured maximum.
    #[error("expression is {len} bytes long, the limit is {max}")]
    TooLong {
        /// Length of the source in bytes.
        len: usize,
        /// Configured maximum.
        max: usize,
    },
    /// Parentheses, negations or call arguments nest deeper than the configured maximum.
    #[error("expression is nested deeper than {max} levels")]
    TooDeep {
        /// Configured maximum.
        max: usize,
    },
    /// Lexical or grammatical error, or a construct outside the supported subset.
    #[error("syntax error at byte {pos}: {message}")]
    Syntax {
        /// Byte offset in the source.
        pos: usize,
        /// What is wrong.
        message: String,
    },
    /// An operand has the wrong type (for example `path && true`).
    #[error("type error at byte {pos}: {message}")]
    Type {
        /// Byte offset in the source.
        pos: usize,
        /// What is wrong.
        message: String,
    },
    /// A regular expression is invalid or exceeds the complexity limits.
    #[error("invalid regular expression at byte {pos}: {message}")]
    Regex {
        /// Byte offset of the pattern in the source.
        pos: usize,
        /// What is wrong, on one line.
        message: String,
    },
}

/// A compiled selector could not be evaluated for one resource.
///
/// Authorization treats every variant as "the selector does not match".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SelectorEvalError {
    /// The outcome depends on an input the format could not provide (for example
    /// `coordinate.groupId` of a path that has no coordinate) and no other part of
    /// the expression decides the result.
    #[error("{0} is not available for this resource")]
    MissingInput(&'static str),
    /// One input string is longer than the configured maximum.
    #[error("input {field} is {len} bytes long, the limit is {max}")]
    InputTooLarge {
        /// Name of the input.
        field: &'static str,
        /// Length in bytes.
        len: usize,
        /// Configured maximum.
        max: usize,
    },
    /// The evaluation visited more nodes than the configured budget.
    #[error("evaluation exceeded the step budget of {max}")]
    StepBudgetExceeded {
        /// Configured budget.
        max: u32,
    },
}
