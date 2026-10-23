//! Evaluator of compiled selectors.
//!
//! Three valued logic. A value the resource cannot provide (the coordinate of a
//! path that has none) is *unknown*, and unknown propagates like CEL's error
//! value: `true || unknown` is true, `false && unknown` is false, everything
//! else that touches an unknown is unknown, and an unknown result is an
//! evaluation error, which authorization treats as no match.
//!
//! The invariant this buys: a selector matches only when it is true for every
//! value the missing data could have had. `!(coordinate.groupId == "evil")`
//! therefore never grants access to a path whose group is not known.

use super::SelectorInput;
use super::SelectorLimits;
use super::ast::{BoolExpr, CoordField, StrExpr, Var};
use super::error::SelectorEvalError;

/// Result of a boolean node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tri {
    True,
    False,
    /// Depends on the named input that is not available.
    Unknown(&'static str),
}

impl Tri {
    fn from_bool(value: bool) -> Self {
        if value { Self::True } else { Self::False }
    }

    fn negate(self) -> Self {
        match self {
            Self::True => Self::False,
            Self::False => Self::True,
            unknown @ Self::Unknown(_) => unknown,
        }
    }
}

/// Result of a string node.
#[derive(Debug, Clone, Copy)]
enum StrVal<'a> {
    Known(&'a str),
    Missing(&'static str),
}

/// Evaluates `root` for `input`.
pub(super) fn evaluate(
    root: &BoolExpr,
    limits: &SelectorLimits,
    input: &SelectorInput<'_>,
) -> Result<bool, SelectorEvalError> {
    check_input_sizes(limits, input)?;
    let mut evaluator = Evaluator {
        input,
        steps: 0,
        max_steps: limits.max_eval_steps,
    };
    match evaluator.bool_expr(root)? {
        Tri::True => Ok(true),
        Tri::False => Ok(false),
        Tri::Unknown(name) => Err(SelectorEvalError::MissingInput(name)),
    }
}

/// Rejects oversized inputs before any regex runs on them. Matching cost is
/// linear in the input length, so this cap is what bounds the cost of one
/// evaluation.
fn check_input_sizes(
    limits: &SelectorLimits,
    input: &SelectorInput<'_>,
) -> Result<(), SelectorEvalError> {
    let coordinate = input.coordinate;
    let fields = [
        ("path", Some(input.path)),
        ("repository", Some(input.repository)),
        (
            "coordinate.group",
            coordinate.and_then(|c| c.group.as_deref()),
        ),
        (
            "coordinate.name",
            coordinate.and_then(|c| c.name.as_deref()),
        ),
        (
            "coordinate.version",
            coordinate.and_then(|c| c.version.as_deref()),
        ),
        (
            "coordinate.extension",
            coordinate.and_then(|c| c.extension.as_deref()),
        ),
        (
            "coordinate.classifier",
            coordinate.and_then(|c| c.classifier.as_deref()),
        ),
    ];
    for (field, value) in fields {
        let len = value.map_or(0, str::len);
        if len > limits.max_input_bytes {
            return Err(SelectorEvalError::InputTooLarge {
                field,
                len,
                max: limits.max_input_bytes,
            });
        }
    }
    Ok(())
}

struct Evaluator<'a> {
    input: &'a SelectorInput<'a>,
    steps: u32,
    max_steps: u32,
}

impl<'a> Evaluator<'a> {
    /// Charges one node. The tree has no loops and every node is visited at most
    /// once, so the budget is a backstop against a future grammar change, not
    /// something a valid expression can reach.
    fn tick(&mut self) -> Result<(), SelectorEvalError> {
        self.steps = self.steps.saturating_add(1);
        if self.steps > self.max_steps {
            return Err(SelectorEvalError::StepBudgetExceeded {
                max: self.max_steps,
            });
        }
        Ok(())
    }

    fn str_expr(&mut self, expr: &'a StrExpr) -> Result<StrVal<'a>, SelectorEvalError> {
        self.tick()?;
        Ok(match expr {
            StrExpr::Lit(text) => StrVal::Known(text),
            StrExpr::Var(Var::Format) => StrVal::Known(self.input.format.as_str()),
            StrExpr::Var(Var::Path) => StrVal::Known(self.input.path),
            StrExpr::Var(Var::Repository) => StrVal::Known(self.input.repository),
            StrExpr::Coord(field) => self.coordinate_field(*field),
        })
    }

    fn coordinate_field(&self, field: CoordField) -> StrVal<'a> {
        let Some(coordinate) = self.input.coordinate else {
            return StrVal::Missing("coordinate");
        };
        let (value, name) = match field {
            CoordField::Group => (coordinate.group.as_deref(), "coordinate.group"),
            CoordField::Name => (coordinate.name.as_deref(), "coordinate.name"),
            CoordField::Version => (coordinate.version.as_deref(), "coordinate.version"),
            CoordField::Extension => (coordinate.extension.as_deref(), "coordinate.extension"),
            CoordField::Classifier => (coordinate.classifier.as_deref(), "coordinate.classifier"),
        };
        value.map_or(StrVal::Missing(name), StrVal::Known)
    }

    fn bool_expr(&mut self, expr: &'a BoolExpr) -> Result<Tri, SelectorEvalError> {
        self.tick()?;
        Ok(match expr {
            BoolExpr::Lit(value) => Tri::from_bool(*value),
            BoolExpr::Not(inner) => self.bool_expr(inner)?.negate(),
            BoolExpr::And(items) => self.fold(items, Tri::False, Tri::True)?,
            BoolExpr::Or(items) => self.fold(items, Tri::True, Tri::False)?,
            BoolExpr::Eq(a, b) => self.compare(a, b, |x, y| x == y)?,
            BoolExpr::Ne(a, b) => self.compare(a, b, |x, y| x != y)?,
            BoolExpr::StartsWith(a, b) => self.compare(a, b, |x, y| x.starts_with(y))?,
            BoolExpr::EndsWith(a, b) => self.compare(a, b, |x, y| x.ends_with(y))?,
            BoolExpr::Contains(a, b) => self.compare(a, b, |x, y| x.contains(y))?,
            BoolExpr::Regex { subject, regex } => match self.str_expr(subject)? {
                StrVal::Known(text) => Tri::from_bool(regex.is_match(text)),
                StrVal::Missing(name) => Tri::Unknown(name),
            },
            BoolExpr::In(subject, items) => match self.str_expr(subject)? {
                StrVal::Known(text) => Tri::from_bool(items.iter().any(|item| item == text)),
                StrVal::Missing(name) => Tri::Unknown(name),
            },
        })
    }

    /// Short circuit evaluation of `&&` (`decisive` = false) and `||`
    /// (`decisive` = true). A decisive operand ends the chain even when an
    /// earlier operand was unknown, which is the CEL error absorption rule.
    fn fold(
        &mut self,
        items: &'a [BoolExpr],
        decisive: Tri,
        neutral: Tri,
    ) -> Result<Tri, SelectorEvalError> {
        let mut unknown = None;
        for item in items {
            match self.bool_expr(item)? {
                value if value == decisive => return Ok(decisive),
                Tri::Unknown(name) => {
                    unknown.get_or_insert(name);
                }
                _ => {}
            }
        }
        Ok(unknown.map_or(neutral, Tri::Unknown))
    }

    fn compare(
        &mut self,
        left: &'a StrExpr,
        right: &'a StrExpr,
        test: impl Fn(&str, &str) -> bool,
    ) -> Result<Tri, SelectorEvalError> {
        let left = self.str_expr(left)?;
        let right = self.str_expr(right)?;
        Ok(match (left, right) {
            (StrVal::Known(a), StrVal::Known(b)) => Tri::from_bool(test(a, b)),
            (StrVal::Missing(name), _) | (_, StrVal::Missing(name)) => Tri::Unknown(name),
        })
    }
}
