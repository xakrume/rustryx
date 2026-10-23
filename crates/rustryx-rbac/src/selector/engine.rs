//! The shipped selector engine: a parser and evaluator for a restricted CEL
//! subset. See ADR-RBAC-1 in `docs/rbac.md` for why it is not the `cel` crate.

use super::error::{SelectorCompileError, SelectorEvalError};
use super::{SelectorEngine, SelectorInput, SelectorLimits, ast::BoolExpr, eval, parser};

/// A compiled selector expression. Immutable, so it is shared between tasks
/// behind an `Arc` without any locking.
#[derive(Debug)]
pub struct Program {
    root: BoolExpr,
    limits: SelectorLimits,
}

/// Engine for the supported subset of CEL: the variables `format`, `path`,
/// `repository` and `coordinate.*`, the operators `==`, `!=`, `=~`, `&&`, `||`,
/// `!`, `in [..]` and parentheses, and the string methods `startsWith`,
/// `endsWith`, `contains` and `matches`. Anything else is a compile error.
#[derive(Debug, Clone, Default)]
pub struct RestrictedEngine {
    limits: SelectorLimits,
}

impl RestrictedEngine {
    /// An engine with explicit limits.
    #[must_use]
    pub fn new(limits: SelectorLimits) -> Self {
        Self { limits }
    }

    /// The limits this engine enforces.
    #[must_use]
    pub fn limits(&self) -> &SelectorLimits {
        &self.limits
    }
}

impl SelectorEngine for RestrictedEngine {
    type Program = Program;

    fn compile(&self, expression: &str) -> Result<Program, SelectorCompileError> {
        let root = parser::parse(expression, &self.limits)?;
        Ok(Program {
            root,
            limits: self.limits,
        })
    }

    fn evaluate(
        &self,
        program: &Program,
        input: &SelectorInput<'_>,
    ) -> Result<bool, SelectorEvalError> {
        eval::evaluate(&program.root, &program.limits, input)
    }
}
