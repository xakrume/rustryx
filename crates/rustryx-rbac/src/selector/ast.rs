//! Typed syntax tree of the selector language.
//!
//! The parser builds this tree already type checked: a `StrExpr` can only be
//! produced from a string-valued construct and a `BoolExpr` only from a
//! boolean one, so the evaluator needs no runtime type errors and a selector
//! that is not boolean is rejected when it is saved.

use regex::Regex;

/// Variables that hold one string of the resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Var {
    /// `format`: the repository format label.
    Format,
    /// `path`: the normalized path with a leading `/`.
    Path,
    /// `repository`: the repository name.
    Repository,
}

/// Fields of `coordinate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CoordField {
    /// `coordinate.group` and its alias `coordinate.groupId`.
    Group,
    /// `coordinate.name` and its alias `coordinate.artifactId`.
    Name,
    /// `coordinate.version`.
    Version,
}

/// A string valued expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum StrExpr {
    /// A string literal.
    Lit(String),
    /// One of the resource variables.
    Var(Var),
    /// A field of the component coordinate. Unknown when the coordinate is not known.
    Coord(CoordField),
}

/// A boolean valued expression.
#[derive(Debug, Clone)]
pub(super) enum BoolExpr {
    /// `true` or `false`.
    Lit(bool),
    /// `!x`.
    Not(Box<BoolExpr>),
    /// `a && b && ...`, flattened so that long chains do not deepen the tree.
    And(Vec<BoolExpr>),
    /// `a || b || ...`, flattened.
    Or(Vec<BoolExpr>),
    /// `a == b`.
    Eq(StrExpr, StrExpr),
    /// `a != b`.
    Ne(StrExpr, StrExpr),
    /// `a.startsWith(b)`.
    StartsWith(StrExpr, StrExpr),
    /// `a.endsWith(b)`.
    EndsWith(StrExpr, StrExpr),
    /// `a.contains(b)`.
    Contains(StrExpr, StrExpr),
    /// `a =~ "re"` or `a.matches("re")`. The pattern is compiled once, at compile time.
    Regex {
        /// The string that is tested.
        subject: StrExpr,
        /// Compiled pattern; for [`MatchMode::Full`] it is already anchored.
        regex: Regex,
    },
    /// `a in ["x", "y"]`.
    In(StrExpr, Vec<String>),
}
