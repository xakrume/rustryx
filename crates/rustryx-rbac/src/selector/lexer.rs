//! Tokenizer of the selector language.
//!
//! Hand written instead of generated so that every rejected input has a byte
//! offset and so that no input can make it recurse: it is one loop over the
//! characters and the caller has already capped the length.

use super::error::SelectorCompileError;

/// Token kinds. Keywords (`true`, `false`, `in`) are identifiers here; the parser
/// gives them meaning, which keeps `coordinate.in` style typos reportable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Kind {
    Ident(String),
    Str(String),
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Dot,
    Bang,
    EqEq,
    NotEq,
    Match,
    AndAnd,
    OrOr,
    Eof,
}

impl Kind {
    /// Short description for error messages.
    pub(super) fn describe(&self) -> String {
        match self {
            Self::Ident(name) => format!("identifier '{name}'"),
            Self::Str(_) => "string literal".to_owned(),
            Self::LParen => "'('".to_owned(),
            Self::RParen => "')'".to_owned(),
            Self::LBracket => "'['".to_owned(),
            Self::RBracket => "']'".to_owned(),
            Self::Comma => "','".to_owned(),
            Self::Dot => "'.'".to_owned(),
            Self::Bang => "'!'".to_owned(),
            Self::EqEq => "'=='".to_owned(),
            Self::NotEq => "'!='".to_owned(),
            Self::Match => "'=~'".to_owned(),
            Self::AndAnd => "'&&'".to_owned(),
            Self::OrOr => "'||'".to_owned(),
            Self::Eof => "end of expression".to_owned(),
        }
    }
}

/// A token and the byte offset where it starts.
#[derive(Debug, Clone)]
pub(super) struct Token {
    pub(super) kind: Kind,
    pub(super) pos: usize,
}

fn syntax(pos: usize, message: impl Into<String>) -> SelectorCompileError {
    SelectorCompileError::Syntax {
        pos,
        message: message.into(),
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_part(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Splits `src` into tokens. The returned list always ends with [`Kind::Eof`].
pub(super) fn lex(src: &str) -> Result<Vec<Token>, SelectorCompileError> {
    let chars: Vec<(usize, char)> = src.char_indices().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(&(pos, c)) = chars.get(i) {
        let next = chars.get(i + 1).map(|&(_, n)| n);
        let (kind, width) = match c {
            ' ' | '\t' | '\r' | '\n' => {
                i += 1;
                continue;
            }
            '(' => (Kind::LParen, 1),
            ')' => (Kind::RParen, 1),
            '[' => (Kind::LBracket, 1),
            ']' => (Kind::RBracket, 1),
            ',' => (Kind::Comma, 1),
            '.' => (Kind::Dot, 1),
            '!' if next == Some('=') => (Kind::NotEq, 2),
            '!' => (Kind::Bang, 1),
            '=' if next == Some('=') => (Kind::EqEq, 2),
            '=' if next == Some('~') => (Kind::Match, 2),
            '&' if next == Some('&') => (Kind::AndAnd, 2),
            '|' if next == Some('|') => (Kind::OrOr, 2),
            '"' | '\'' => {
                let (text, used) = lex_string(&chars, i, false)?;
                (Kind::Str(text), used)
            }
            'r' | 'R' if matches!(next, Some('"' | '\'')) => {
                let (text, used) = lex_string(&chars, i + 1, true)?;
                (Kind::Str(text), used + 1)
            }
            c if is_ident_start(c) => {
                let len = chars[i..]
                    .iter()
                    .take_while(|&&(_, ch)| is_ident_part(ch))
                    .count();
                let name: String = chars[i..i + len].iter().map(|&(_, ch)| ch).collect();
                (Kind::Ident(name), len)
            }
            '=' | '&' | '|' => {
                return Err(syntax(
                    pos,
                    format!("unexpected '{c}', the operators are ==, !=, =~, && and ||"),
                ));
            }
            other => return Err(syntax(pos, format!("unexpected character '{other}'"))),
        };
        out.push(Token { kind, pos });
        i += width;
    }
    out.push(Token {
        kind: Kind::Eof,
        pos: src.len(),
    });
    Ok(out)
}

/// Lexes a quoted string starting at the quote `chars[start]`. Returns the text
/// and the number of characters consumed, quotes included.
///
/// Raw strings take every character literally. Escaped strings know `\\`, `\"`,
/// `\'`, `\n`, `\r` and `\t` only: anything else is rejected instead of guessed,
/// because a regex typed as `"\d"` would otherwise silently mean something else.
/// Control characters must be escaped, so a literal cannot span lines.
fn lex_string(
    chars: &[(usize, char)],
    start: usize,
    raw: bool,
) -> Result<(String, usize), SelectorCompileError> {
    let (open_pos, quote) = chars[start];
    let mut text = String::new();
    let mut i = start + 1;
    while let Some(&(pos, c)) = chars.get(i) {
        if c == quote {
            return Ok((text, i + 1 - start));
        }
        if c.is_control() {
            return Err(syntax(pos, "control character inside a string literal"));
        }
        if c == '\\' && !raw {
            let Some(&(_, escaped)) = chars.get(i + 1) else {
                break;
            };
            text.push(match escaped {
                '\\' => '\\',
                '"' => '"',
                '\'' => '\'',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                other => {
                    return Err(syntax(
                        pos,
                        format!(
                            "unknown escape '\\{other}', use \\\\, \\\", \\', \\n, \\r, \\t or a raw string r\"...\""
                        ),
                    ));
                }
            });
            i += 2;
            continue;
        }
        text.push(c);
        i += 1;
    }
    Err(syntax(open_pos, "unterminated string literal"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<Kind> {
        lex(src).unwrap().into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn lexes_operators_and_punctuation() {
        assert_eq!(
            kinds("a == b != c =~ d && e || !f ( ) [ ] , ."),
            vec![
                Kind::Ident("a".into()),
                Kind::EqEq,
                Kind::Ident("b".into()),
                Kind::NotEq,
                Kind::Ident("c".into()),
                Kind::Match,
                Kind::Ident("d".into()),
                Kind::AndAnd,
                Kind::Ident("e".into()),
                Kind::OrOr,
                Kind::Bang,
                Kind::Ident("f".into()),
                Kind::LParen,
                Kind::RParen,
                Kind::LBracket,
                Kind::RBracket,
                Kind::Comma,
                Kind::Dot,
                Kind::Eof,
            ]
        );
    }

    #[test]
    fn lexes_escapes_and_raw_strings() {
        assert_eq!(
            kinds(r#""a\"b\\c\n" 'it\'s' r"\d+" R'x'"#)[..4],
            [
                Kind::Str("a\"b\\c\n".into()),
                Kind::Str("it's".into()),
                Kind::Str("\\d+".into()),
                Kind::Str("x".into()),
            ]
        );
    }

    #[test]
    fn identifier_starting_with_r_is_not_a_raw_string() {
        assert_eq!(
            kinds("repository r")[..2],
            [Kind::Ident("repository".into()), Kind::Ident("r".into())]
        );
    }

    #[test]
    fn rejects_bad_input_with_offsets() {
        for (src, pos) in [
            ("a = b", 2),
            ("a & b", 2),
            ("a | b", 2),
            ("a # b", 2),
            ("\"abc", 0),
            ("\"abc\\", 0),
            ("x == \"\\q\"", 6),
            ("x == \"a\nb\"", 7),
        ] {
            let err = lex(src).unwrap_err();
            assert!(
                matches!(err, SelectorCompileError::Syntax { pos: got, .. } if got == pos),
                "{src}: {err:?}"
            );
        }
    }

    #[test]
    fn eof_token_sits_at_the_end_of_the_source() {
        let tokens = lex("ab").unwrap();
        assert_eq!(tokens.last().unwrap().pos, 2);
    }

    #[test]
    fn every_kind_has_a_description() {
        for kind in kinds("a \"s\" ( ) [ ] , . ! == != =~ && ||") {
            assert_ne!(kind.describe(), "");
        }
    }
}
