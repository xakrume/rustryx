//! Recursive descent parser of the selector language.
//!
//! Grammar (lowest precedence first):
//!
//! ```text
//! or      = and ( "||" and )*
//! and     = rel ( "&&" rel )*
//! rel     = unary ( ( "==" | "!=" | "=~" | "in" ) unary )?
//! unary   = "!" unary | postfix
//! postfix = primary ( "." ident [ "(" [ or ] ")" ] )*
//! primary = string | "true" | "false" | variable | "(" or ")" | "[" string ( "," string )* "]"
//! ```
//!
//! Recursion happens only through parentheses, `!` and call arguments, and each
//! of those goes through [`Parser::nested`], which enforces `max_depth`. Chains
//! of `&&` and `||` are loops, so a long flat expression cannot deepen the tree.

use super::SelectorLimits;
use super::ast::{BoolExpr, CoordField, StrExpr, Var};
use super::error::SelectorCompileError;
use super::lexer::{Kind, Token, lex};

mod pattern;
use pattern::build_regex;

/// Whether a regex must match the whole string or any part of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchMode {
    /// `=~`: the whole string, as in the compat selector (JEXL) heritage.
    Full,
    /// `matches()`: any substring, as in the CEL specification.
    Partial,
}

/// A parsed construct together with its type.
enum Typed {
    Bool(BoolExpr),
    Str(StrExpr),
    List(Vec<String>),
    /// The bare `coordinate` object: only meaningful as the receiver of `.field`.
    Coordinate,
}

impl Typed {
    fn describe(&self) -> &'static str {
        match self {
            Self::Bool(_) => "a boolean",
            Self::Str(_) => "a string",
            Self::List(_) => "a list",
            Self::Coordinate => "the coordinate object",
        }
    }

    fn into_bool(self, pos: usize, context: &str) -> Result<BoolExpr, SelectorCompileError> {
        match self {
            Self::Bool(b) => Ok(b),
            other => Err(type_error(
                pos,
                format!("{context} must be a boolean, found {}", other.describe()),
            )),
        }
    }

    fn into_str(self, pos: usize, context: &str) -> Result<StrExpr, SelectorCompileError> {
        match self {
            Self::Str(s) => Ok(s),
            Self::Coordinate => Err(type_error(
                pos,
                format!(
                    "{context} must be a string, use coordinate.group, coordinate.name or coordinate.version"
                ),
            )),
            other => Err(type_error(
                pos,
                format!("{context} must be a string, found {}", other.describe()),
            )),
        }
    }
}

fn syntax(pos: usize, message: impl Into<String>) -> SelectorCompileError {
    SelectorCompileError::Syntax {
        pos,
        message: message.into(),
    }
}

fn type_error(pos: usize, message: impl Into<String>) -> SelectorCompileError {
    SelectorCompileError::Type {
        pos,
        message: message.into(),
    }
}

/// Parses `src` into a boolean expression.
pub(super) fn parse(src: &str, limits: &SelectorLimits) -> Result<BoolExpr, SelectorCompileError> {
    if src.trim().is_empty() {
        return Err(SelectorCompileError::Empty);
    }
    if src.len() > limits.max_expression_bytes {
        return Err(SelectorCompileError::TooLong {
            len: src.len(),
            max: limits.max_expression_bytes,
        });
    }
    let mut parser = Parser {
        tokens: lex(src)?,
        idx: 0,
        depth: 0,
        limits,
    };
    let start = parser.peek().pos;
    let root = parser.parse_or()?;
    let trailing = parser.peek();
    if trailing.kind != Kind::Eof {
        return Err(syntax(
            trailing.pos,
            format!("unexpected {}", trailing.kind.describe()),
        ));
    }
    root.into_bool(start, "the selector")
}

struct Parser<'l> {
    tokens: Vec<Token>,
    idx: usize,
    depth: usize,
    limits: &'l SelectorLimits,
}

impl Parser<'_> {
    fn peek(&self) -> &Token {
        // `lex` always ends the list with Eof and `bump` never moves past it.
        &self.tokens[self.idx.min(self.tokens.len() - 1)]
    }

    fn bump(&mut self) -> Token {
        let token = self.peek().clone();
        if self.idx + 1 < self.tokens.len() {
            self.idx += 1;
        }
        token
    }

    fn eat(&mut self, kind: &Kind) -> bool {
        if &self.peek().kind == kind {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: &Kind) -> Result<(), SelectorCompileError> {
        let token = self.bump();
        if &token.kind == kind {
            Ok(())
        } else {
            Err(syntax(
                token.pos,
                format!(
                    "expected {}, found {}",
                    kind.describe(),
                    token.kind.describe()
                ),
            ))
        }
    }

    /// Runs `f` one nesting level deeper, failing when `max_depth` is exceeded.
    fn nested<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, SelectorCompileError>,
    ) -> Result<T, SelectorCompileError> {
        self.depth += 1;
        if self.depth > self.limits.max_depth {
            return Err(SelectorCompileError::TooDeep {
                max: self.limits.max_depth,
            });
        }
        let result = f(self);
        self.depth -= 1;
        result
    }

    fn parse_or(&mut self) -> Result<Typed, SelectorCompileError> {
        let start = self.peek().pos;
        let first = self.parse_and()?;
        if self.peek().kind != Kind::OrOr {
            return Ok(first);
        }
        let mut items = vec![first.into_bool(start, "an operand of ||")?];
        while self.eat(&Kind::OrOr) {
            let pos = self.peek().pos;
            items.push(self.parse_and()?.into_bool(pos, "an operand of ||")?);
        }
        Ok(Typed::Bool(BoolExpr::Or(items)))
    }

    fn parse_and(&mut self) -> Result<Typed, SelectorCompileError> {
        let start = self.peek().pos;
        let first = self.parse_rel()?;
        if self.peek().kind != Kind::AndAnd {
            return Ok(first);
        }
        let mut items = vec![first.into_bool(start, "an operand of &&")?];
        while self.eat(&Kind::AndAnd) {
            let pos = self.peek().pos;
            items.push(self.parse_rel()?.into_bool(pos, "an operand of &&")?);
        }
        Ok(Typed::Bool(BoolExpr::And(items)))
    }

    fn parse_rel(&mut self) -> Result<Typed, SelectorCompileError> {
        let left_pos = self.peek().pos;
        let left = self.parse_unary()?;
        let op = self.peek().clone();
        let is_in = matches!(&op.kind, Kind::Ident(word) if word == "in");
        if !matches!(op.kind, Kind::EqEq | Kind::NotEq | Kind::Match) && !is_in {
            return Ok(left);
        }
        self.bump();
        let right_pos = self.peek().pos;
        let right = self.parse_unary()?;
        let name = op.kind.describe();
        let subject = left.into_str(left_pos, &format!("the left side of {name}"))?;
        let built = match op.kind {
            Kind::EqEq | Kind::NotEq => {
                let rhs = right.into_str(right_pos, &format!("the right side of {name}"))?;
                if op.kind == Kind::EqEq {
                    BoolExpr::Eq(subject, rhs)
                } else {
                    BoolExpr::Ne(subject, rhs)
                }
            }
            Kind::Match => self.regex_node(subject, right, right_pos, MatchMode::Full, "=~")?,
            _ => match right {
                Typed::List(items) => BoolExpr::In(subject, items),
                other => {
                    return Err(type_error(
                        right_pos,
                        format!(
                            "the right side of 'in' must be a list of string literals, found {}",
                            other.describe()
                        ),
                    ));
                }
            },
        };
        Ok(Typed::Bool(built))
    }

    fn parse_unary(&mut self) -> Result<Typed, SelectorCompileError> {
        if self.peek().kind != Kind::Bang {
            return self.parse_postfix();
        }
        let bang = self.bump();
        let inner = self.nested(Self::parse_unary)?;
        let inner = inner.into_bool(bang.pos, "the operand of '!'")?;
        Ok(Typed::Bool(BoolExpr::Not(Box::new(inner))))
    }

    fn parse_postfix(&mut self) -> Result<Typed, SelectorCompileError> {
        let mut current = self.parse_primary()?;
        while self.eat(&Kind::Dot) {
            let name_token = self.bump();
            let Kind::Ident(name) = name_token.kind else {
                return Err(syntax(
                    name_token.pos,
                    "expected a field or method name after '.'",
                ));
            };
            current = if self.peek().kind == Kind::LParen {
                self.parse_method(current, &name, name_token.pos)?
            } else {
                Self::field(&current, &name, name_token.pos)?
            };
        }
        Ok(current)
    }

    fn field(receiver: &Typed, name: &str, pos: usize) -> Result<Typed, SelectorCompileError> {
        if !matches!(receiver, Typed::Coordinate) {
            return Err(syntax(
                pos,
                format!("'.{name}' is not a field: only coordinate has fields"),
            ));
        }
        let field = match name {
            "group" | "groupId" => CoordField::Group,
            "name" | "artifactId" => CoordField::Name,
            "version" => CoordField::Version,
            other => {
                return Err(syntax(
                    pos,
                    format!(
                        "unknown coordinate field '{other}', use group (groupId), name (artifactId) or version"
                    ),
                ));
            }
        };
        Ok(Typed::Str(StrExpr::Coord(field)))
    }

    fn parse_method(
        &mut self,
        receiver: Typed,
        name: &str,
        pos: usize,
    ) -> Result<Typed, SelectorCompileError> {
        if !matches!(name, "startsWith" | "endsWith" | "contains" | "matches") {
            return Err(syntax(
                pos,
                format!(
                    "unsupported method '{name}', use startsWith, endsWith, contains or matches"
                ),
            ));
        }
        let open = self.bump();
        let arg_pos = self.peek().pos;
        let args = self.nested(|p| {
            let mut args = Vec::new();
            if p.peek().kind != Kind::RParen {
                args.push(p.parse_or()?);
                while p.eat(&Kind::Comma) {
                    args.push(p.parse_or()?);
                }
            }
            Ok(args)
        })?;
        self.expect(&Kind::RParen)?;
        let mut args = args.into_iter();
        let (Some(arg), None) = (args.next(), args.next()) else {
            return Err(syntax(
                open.pos,
                format!("{name} takes exactly one argument"),
            ));
        };
        let subject = receiver.into_str(pos, &format!("the receiver of {name}"))?;
        if name == "matches" {
            let regex = self.regex_node(subject, arg, arg_pos, MatchMode::Partial, "matches")?;
            return Ok(Typed::Bool(regex));
        }
        let arg = arg.into_str(arg_pos, &format!("the argument of {name}"))?;
        Ok(Typed::Bool(match name {
            "startsWith" => BoolExpr::StartsWith(subject, arg),
            "endsWith" => BoolExpr::EndsWith(subject, arg),
            _ => BoolExpr::Contains(subject, arg),
        }))
    }

    fn parse_primary(&mut self) -> Result<Typed, SelectorCompileError> {
        let token = self.bump();
        match token.kind {
            Kind::Str(text) => Ok(Typed::Str(StrExpr::Lit(text))),
            Kind::Ident(name) => Self::identifier(&name, token.pos),
            Kind::LParen => {
                let inner = self.nested(Self::parse_or)?;
                self.expect(&Kind::RParen)?;
                Ok(inner)
            }
            Kind::LBracket => self.parse_list(),
            other => Err(syntax(
                token.pos,
                format!("unexpected {}", other.describe()),
            )),
        }
    }

    fn identifier(name: &str, pos: usize) -> Result<Typed, SelectorCompileError> {
        Ok(match name {
            "true" => Typed::Bool(BoolExpr::Lit(true)),
            "false" => Typed::Bool(BoolExpr::Lit(false)),
            "format" => Typed::Str(StrExpr::Var(Var::Format)),
            "path" => Typed::Str(StrExpr::Var(Var::Path)),
            "repository" => Typed::Str(StrExpr::Var(Var::Repository)),
            "coordinate" => Typed::Coordinate,
            other => {
                return Err(syntax(
                    pos,
                    format!(
                        "unknown identifier '{other}', the variables are format, path, repository and coordinate"
                    ),
                ));
            }
        })
    }

    fn parse_list(&mut self) -> Result<Typed, SelectorCompileError> {
        let mut items = Vec::new();
        loop {
            let token = self.bump();
            let Kind::Str(text) = token.kind else {
                return Err(syntax(
                    token.pos,
                    format!(
                        "a list holds string literals, found {}",
                        token.kind.describe()
                    ),
                ));
            };
            items.push(text);
            if items.len() > self.limits.max_list_items {
                return Err(syntax(
                    token.pos,
                    format!("a list holds at most {} items", self.limits.max_list_items),
                ));
            }
            if !self.eat(&Kind::Comma) {
                break;
            }
        }
        self.expect(&Kind::RBracket)?;
        Ok(Typed::List(items))
    }

    /// Builds the regex node for `=~` and `matches`. The pattern has to be a
    /// literal so that it is compiled and size checked once, when the selector
    /// is saved, and never while a request is waiting.
    fn regex_node(
        &self,
        subject: StrExpr,
        pattern: Typed,
        pattern_pos: usize,
        mode: MatchMode,
        operator: &str,
    ) -> Result<BoolExpr, SelectorCompileError> {
        let Typed::Str(StrExpr::Lit(pattern)) = pattern else {
            return Err(type_error(
                pattern_pos,
                format!("the pattern of {operator} must be a string literal"),
            ));
        };
        let regex = build_regex(&pattern, mode, self.limits, pattern_pos)?;
        Ok(BoolExpr::Regex { subject, regex })
    }
}

#[cfg(test)]
mod tests;
