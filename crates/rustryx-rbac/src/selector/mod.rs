//! Content selector engine.
//!
//! A content selector is a boolean expression over `format`, `path`,
//! `repository` and `coordinate.*` that narrows a privilege to part of a
//! repository. The engine sits behind [`SelectorEngine`] so it can be replaced;
//! the shipped implementation is [`RestrictedEngine`], a parser and evaluator for
//! a CEL subset (decision record ADR-RBAC-1 in `docs/rbac.md`).
//!
//! Every engine must be fail closed: a compile error rejects the selector when
//! it is saved, and an evaluation error means "does not match".

mod ast;
mod engine;
mod error;
mod eval;
mod lexer;
mod parser;

use rustryx_core::model::RepoFormat;
use rustryx_core::ports::auth::Coordinate;

pub use engine::{Program, RestrictedEngine};
pub use error::{SelectorCompileError, SelectorEvalError};

/// Safety limits of a selector engine. They apply to every expression
/// regardless of who wrote it (SEC-18).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectorLimits {
    /// Longest accepted source text in bytes.
    pub max_expression_bytes: usize,
    /// Deepest nesting of parentheses, negations and call arguments. It caps the
    /// parser recursion, so a hostile expression cannot overflow the stack.
    pub max_depth: usize,
    /// Most elements in an `in [...]` list.
    pub max_list_items: usize,
    /// Longest regular expression source in bytes.
    pub max_regex_pattern_bytes: usize,
    /// Cap on the compiled size of one regular expression, in bytes. The `regex`
    /// crate refuses to build a larger program, which bounds both memory and the
    /// cost of every later match (matching is linear in the input, never
    /// exponential, because the crate has no backtracking engine).
    pub max_regex_program_bytes: usize,
    /// Longest accepted input string (`path`, `repository`, coordinate parts) in
    /// bytes. A longer input is an evaluation error, not a truncation.
    pub max_input_bytes: usize,
    /// Most nodes one evaluation may visit.
    pub max_eval_steps: u32,
}

impl Default for SelectorLimits {
    fn default() -> Self {
        Self {
            max_expression_bytes: 1024,
            max_depth: 32,
            max_list_items: 64,
            max_regex_pattern_bytes: 512,
            max_regex_program_bytes: 1024 * 1024,
            max_input_bytes: 8192,
            max_eval_steps: 4096,
        }
    }
}

/// What a selector sees about one resource.
#[derive(Debug, Clone, Copy)]
pub struct SelectorInput<'a> {
    /// Repository format, exposed as `format` (its canonical lowercase label).
    pub format: RepoFormat,
    /// Normalized path with a leading `/`, exposed as `path`.
    pub path: &'a str,
    /// Repository name, exposed as `repository`.
    pub repository: &'a str,
    /// Component coordinate, exposed as `coordinate.*`. `None` when the format
    /// cannot derive one before reading the body; selectors that depend on it
    /// then have no definite answer and do not match.
    pub coordinate: Option<&'a Coordinate>,
}

/// Compiles and evaluates content selector expressions.
///
/// Both methods are synchronous and never block, so there is nothing to cancel.
/// Implementations must be pure: the same program and input always give the
/// same answer, because compiled programs are cached and shared between tasks.
pub trait SelectorEngine: Send + Sync + 'static {
    /// A compiled expression. It is built once per selector version and then
    /// evaluated for every request, possibly from many tasks at once.
    type Program: Send + Sync + 'static;

    /// Parses, type checks and validates `expression` against the engine limits.
    ///
    /// Called when a selector is saved (a compile error becomes a validation
    /// error for the administrator) and again when a node loads the selector.
    fn compile(&self, expression: &str) -> Result<Self::Program, SelectorCompileError>;

    /// Evaluates a compiled expression. `Ok(true)` means the selector matches.
    /// Callers must treat `Err` exactly like `Ok(false)`.
    fn evaluate(
        &self,
        program: &Self::Program,
        input: &SelectorInput<'_>,
    ) -> Result<bool, SelectorEvalError>;
}
