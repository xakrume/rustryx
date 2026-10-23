//! The glob dialect of name selectors, and its translation to an anchored regular expression.
//!
//! Immutability and retention rules are evaluated inside SQL (the publish transaction has no
//! application engine), so a glob is translated here to a regular expression that PostgreSQL
//! evaluates in bounded time. The translation is stored next to the glob. The same text is what
//! the in memory test double (`rustryx-testkit`) feeds to the `regex` crate, so both engines
//! agree on one definition.
//!
//! Dialect:
//!
//! | Glob | Meaning | Regular expression |
//! |---|---|---|
//! | `*` | any characters except `/` | `[^/]*` |
//! | `**` | any characters | `.*` |
//! | `?` | one character except `/` | `[^/]` |
//! | `{a,b}` | one of at most 8 alternatives, no nesting | `(?:a\|b)` |
//! | anything else | itself | the escaped literal |
//!
//! Limits: a glob has 1 to 256 characters, at most 8 wildcards (`*`, `**` and `?` each count
//! one, also inside an alternative), no control characters, no run of three or more `*`, no
//! empty alternative and no unbalanced or nested braces. A `,` outside braces is a literal.
//!
//! Trap: the regular expression uses `.` for `**`, which in the `regex` crate does not match a
//! line break. The items a selector is applied to (tags, versions, repository and asset paths)
//! cannot contain one, so no `(?s)` flag is emitted: PostgreSQL accepts embedded options only at
//! the very start of an expression and the two engines would disagree about where they apply.
//!
//! The output uses no quantifier on a group and no quantifier on a quantifier, which is what
//! keeps evaluation linear (`must_not_emit_nested_quantifiers`).

use crate::error::DomainError;

/// Longest glob, in characters.
pub const MAX_GLOB_LEN: usize = 256;
/// Most wildcards (`*`, `**`, `?`) in one glob.
pub const MAX_WILDCARDS: usize = 8;
/// Most alternatives in one `{...}` group.
pub const MAX_ALTERNATIVES: usize = 8;

/// One element of a parsed glob.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Literal(char),
    Star,
    DoubleStar,
    Question,
    Group(Vec<Vec<Node>>),
}

/// Checks a glob against the dialect and its limits.
///
/// # Errors
/// Returns [`DomainError::Invalid`] naming the rule that is broken.
pub fn validate(glob: &str) -> Result<(), DomainError> {
    parse(glob).map(|_| ())
}

/// Translates a glob to an anchored regular expression made only of escaped literals,
/// `[^/]*`, `.*`, `[^/]` and non capturing alternation groups.
///
/// # Errors
/// Returns [`DomainError::Invalid`] when the glob is not valid (see [`validate`]).
pub fn to_regex(glob: &str) -> Result<String, DomainError> {
    let nodes = parse(glob)?;
    let mut out = String::with_capacity(glob.len() * 2 + 2);
    out.push('^');
    emit(&nodes, &mut out);
    out.push('$');
    Ok(out)
}

fn invalid(rule: &'static str) -> DomainError {
    DomainError::invalid(format!("invalid glob: {rule}"))
}

fn parse(glob: &str) -> Result<Vec<Node>, DomainError> {
    if glob.is_empty() {
        return Err(invalid("a glob must not be empty"));
    }
    if glob.chars().count() > MAX_GLOB_LEN {
        return Err(invalid("a glob must be at most 256 characters"));
    }
    if glob.chars().any(char::is_control) {
        return Err(invalid("a glob must not contain control characters"));
    }
    let chars: Vec<char> = glob.chars().collect();
    let mut position = 0;
    let mut wildcards = 0;
    let nodes = parse_sequence(&chars, &mut position, false, &mut wildcards)?;
    if position != chars.len() {
        // `parse_sequence` outside a group stops only at a closing brace.
        return Err(invalid("a '}' has no matching '{'"));
    }
    Ok(nodes)
}

/// Parses until the end of input or, inside a group, until `,` or `}`.
fn parse_sequence(
    chars: &[char],
    position: &mut usize,
    in_group: bool,
    wildcards: &mut usize,
) -> Result<Vec<Node>, DomainError> {
    let mut nodes = Vec::new();
    while let Some(&current) = chars.get(*position) {
        match current {
            '}' => return Ok(nodes),
            ',' if in_group => return Ok(nodes),
            '{' if in_group => return Err(invalid("braces must not be nested")),
            '{' => {
                *position += 1;
                nodes.push(parse_group(chars, position, wildcards)?);
            }
            '*' => {
                let mut run = 0;
                while chars.get(*position) == Some(&'*') {
                    run += 1;
                    *position += 1;
                }
                count_wildcard(wildcards)?;
                nodes.push(match run {
                    1 => Node::Star,
                    2 => Node::DoubleStar,
                    _ => return Err(invalid("a run of three or more '*' is not allowed")),
                });
            }
            '?' => {
                *position += 1;
                count_wildcard(wildcards)?;
                nodes.push(Node::Question);
            }
            literal => {
                *position += 1;
                nodes.push(Node::Literal(literal));
            }
        }
    }
    Ok(nodes)
}

fn parse_group(
    chars: &[char],
    position: &mut usize,
    wildcards: &mut usize,
) -> Result<Node, DomainError> {
    let mut alternatives = Vec::new();
    loop {
        let alternative = parse_sequence(chars, position, true, wildcards)?;
        if alternative.is_empty() {
            return Err(invalid("an alternative must not be empty"));
        }
        alternatives.push(alternative);
        if alternatives.len() > MAX_ALTERNATIVES {
            return Err(invalid("a group has at most 8 alternatives"));
        }
        match chars.get(*position) {
            Some(',') => *position += 1,
            Some('}') => {
                *position += 1;
                return Ok(Node::Group(alternatives));
            }
            _ => return Err(invalid("a '{' has no matching '}'")),
        }
    }
}

fn count_wildcard(wildcards: &mut usize) -> Result<(), DomainError> {
    *wildcards += 1;
    if *wildcards > MAX_WILDCARDS {
        return Err(invalid("a glob has at most 8 wildcards"));
    }
    Ok(())
}

fn emit(nodes: &[Node], out: &mut String) {
    for node in nodes {
        match node {
            Node::Literal(c) => push_escaped(*c, out),
            Node::Star => out.push_str("[^/]*"),
            Node::DoubleStar => out.push_str(".*"),
            Node::Question => out.push_str("[^/]"),
            Node::Group(alternatives) => {
                out.push_str("(?:");
                for (index, alternative) in alternatives.iter().enumerate() {
                    if index > 0 {
                        out.push('|');
                    }
                    emit(alternative, out);
                }
                out.push(')');
            }
        }
    }
}

/// Escapes the characters that are special in both the `regex` crate and PostgreSQL's
/// advanced regular expressions. Only ASCII punctuation is escaped, never a letter or digit
/// (`\d` would turn a literal into a class), and `<` and `>` are left alone because the `regex`
/// crate reads `\<` and `\>` as word boundaries.
fn push_escaped(c: char, out: &mut String) {
    if matches!(
        c,
        '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '|' | '[' | ']' | '{' | '}' | '^' | '$'
    ) {
        out.push('\\');
    }
    out.push(c);
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use regex::Regex;

    use super::*;

    #[test]
    fn translation_of_every_construct_is_pinned() {
        for (glob, expected) in [
            ("app", "^app$"),
            ("app-*", "^app-[^/]*$"),
            ("**/*.jar", r"^.*/[^/]*\.jar$"),
            ("v?.?", r"^v[^/]\.[^/]$"),
            ("{a,b}x", "^(?:a|b)x$"),
            ("x{a*,/b}y", "^x(?:a[^/]*|/b)y$"),
            ("a,b", "^a,b$"),
            ("c++", r"^c\+\+$"),
            ("<b>", "^<b>$"),
            ("é*", "^é[^/]*$"),
        ] {
            assert_eq!(to_regex(glob).expect(glob), expected, "{glob}");
        }
    }

    #[test]
    fn regex_metacharacters_are_escaped_as_literals() {
        let glob = "a+b(c)[d].e^$|\\z";
        let translated = to_regex(glob).expect("valid");
        assert_eq!(translated, r"^a\+b\(c\)\[d\]\.e\^\$\|\\z$");
        let compiled = Regex::new(&translated).expect("compiles");
        assert!(compiled.is_match(glob), "the glob matches itself literally");
        assert!(!compiled.is_match("aab(c)[d].e^$|\\z"));
    }

    #[test]
    fn rejects_every_rule_violation() {
        let long = "a".repeat(MAX_GLOB_LEN + 1);
        let nine_wildcards = "*a".repeat(9);
        let nine_alternatives = "{a,b,c,d,e,f,g,h,i}";
        for (bad, why) in [
            ("", "empty"),
            (long.as_str(), "too long"),
            ("a\nb", "control character"),
            ("a\u{0}b", "nul"),
            ("{a", "unclosed brace"),
            ("a}", "unmatched closing brace"),
            ("}", "unmatched closing brace"),
            ("{a,{b,c}}", "nested braces"),
            ("{a,}", "empty alternative"),
            ("{,a}", "empty alternative"),
            ("{}", "empty group"),
            ("{a,,b}", "empty alternative"),
            ("***", "three stars"),
            ("a****b", "four stars"),
            (nine_wildcards.as_str(), "nine wildcards"),
            (nine_alternatives, "nine alternatives"),
            ("{a,b", "unclosed after comma"),
        ] {
            assert!(validate(bad).is_err(), "{why}: {bad:?}");
            assert!(to_regex(bad).is_err(), "{why}: {bad:?}");
        }
    }

    #[test]
    fn accepts_the_limits_exactly() {
        assert!(validate(&"a".repeat(MAX_GLOB_LEN)).is_ok());
        assert!(
            validate(&"é".repeat(MAX_GLOB_LEN)).is_ok(),
            "the limit counts characters"
        );
        assert!(validate(&"*a".repeat(MAX_WILDCARDS)).is_ok());
        assert!(validate("{a,b,c,d,e,f,g,h}").is_ok());
        assert!(validate("**").is_ok());
        assert!(validate("{a*,b?}**??").is_ok());
        assert!(
            validate("{a*,b?}***").is_err(),
            "a run of three stars is refused"
        );
    }

    #[test]
    fn wildcards_inside_alternatives_count() {
        // 4 inside the group + 5 outside = 9, 4 + 4 = 8.
        assert!(validate("{a*,b*,c*,d*}?????").is_err());
        assert!(validate("{a*,b*,c*,d*}????").is_ok());
        assert!(validate("{a*,b*,c*,d*}**??").is_ok(), "`**` counts once");
    }

    #[test]
    fn star_stops_at_the_slash_and_double_star_does_not() {
        let single = Regex::new(&to_regex("lib/*.jar").expect("glob")).expect("regex");
        assert!(single.is_match("lib/a.jar"));
        assert!(!single.is_match("lib/sub/a.jar"));
        let double = Regex::new(&to_regex("lib/**.jar").expect("glob")).expect("regex");
        assert!(double.is_match("lib/sub/a.jar"));
        assert!(!double.is_match("libs/a.jar"));
        let question = Regex::new(&to_regex("a?c").expect("glob")).expect("regex");
        assert!(question.is_match("abc") && !question.is_match("a/c") && !question.is_match("ac"));
    }

    #[test]
    fn the_translation_is_anchored_at_both_ends() {
        let regex = Regex::new(&to_regex("rel").expect("glob")).expect("regex");
        assert!(regex.is_match("rel"));
        assert!(!regex.is_match("release") && !regex.is_match("pre-rel"));
    }

    #[test]
    fn the_largest_pattern_compiles_to_a_small_program() {
        // The heaviest valid shape: one full group of 8 alternatives plus 7 more wildcards.
        let glob = format!("{{{}}}{}", "abcdefg,".repeat(7) + "hijklmn", "*a".repeat(7));
        assert!(validate(&glob).is_ok(), "{glob}");
        let regex = to_regex(&glob).expect("valid");
        let compiled = regex::RegexBuilder::new(&regex)
            .size_limit(64 * 1024)
            .build()
            .expect("stays under 64 KiB of compiled program");
        assert!(compiled.is_match(&format!("abcdefg{}", "a".repeat(7))));
    }

    // Independent reference: tokenize each part by plain string handling, expand the braces into
    // token lists (adjacent stars from different parts stay separate wildcards, which is the
    // meaning of the glob), and match with a backtracking matcher. It shares no code with the
    // translation above.
    #[derive(Clone, Copy)]
    enum Tok {
        Lit(char),
        Star,
        DoubleStar,
        Any,
    }

    fn tokenize(part: &str) -> Vec<Tok> {
        let chars: Vec<char> = part.chars().collect();
        let mut tokens = Vec::new();
        let mut index = 0;
        while index < chars.len() {
            match chars[index] {
                '*' if chars.get(index + 1) == Some(&'*') => {
                    tokens.push(Tok::DoubleStar);
                    index += 1;
                }
                '*' => tokens.push(Tok::Star),
                '?' => tokens.push(Tok::Any),
                literal => tokens.push(Tok::Lit(literal)),
            }
            index += 1;
        }
        tokens
    }

    fn expand(glob: &str) -> Vec<Vec<Tok>> {
        let mut results: Vec<Vec<Tok>> = vec![Vec::new()];
        let mut rest = glob;
        while let Some(open) = rest.find('{') {
            let close = open + rest[open..].find('}').expect("balanced in this test");
            let prefix = tokenize(&rest[..open]);
            let alternatives: Vec<Vec<Tok>> =
                rest[open + 1..close].split(',').map(tokenize).collect();
            results = results
                .into_iter()
                .flat_map(|done| {
                    let prefix = prefix.clone();
                    alternatives.iter().map(move |alternative| {
                        let mut next = done.clone();
                        next.extend(prefix.iter().copied());
                        next.extend(alternative.iter().copied());
                        next
                    })
                })
                .collect();
            rest = &rest[close + 1..];
        }
        let tail = tokenize(rest);
        for result in &mut results {
            result.extend(tail.iter().copied());
        }
        results
    }

    fn plain(pattern: &[Tok], text: &[char]) -> bool {
        match pattern {
            [] => text.is_empty(),
            [Tok::DoubleStar, rest @ ..] => (0..=text.len()).any(|i| plain(rest, &text[i..])),
            [Tok::Star, rest @ ..] => (0..=text.len())
                .any(|i| text[..i].iter().all(|c| *c != '/') && plain(rest, &text[i..])),
            [Tok::Any, rest @ ..] => {
                text.first().is_some_and(|c| *c != '/') && plain(rest, &text[1..])
            }
            [Tok::Lit(literal), rest @ ..] => {
                text.first() == Some(literal) && plain(rest, &text[1..])
            }
        }
    }

    fn reference_matches(glob: &str, text: &str) -> bool {
        let text: Vec<char> = text.chars().collect();
        expand(glob).iter().any(|pattern| plain(pattern, &text))
    }

    #[test]
    fn adjacent_stars_from_different_parts_stay_separate_wildcards() {
        // `a*` inside the group and the `*` after it must not merge into `**`: "a/" would match.
        let regex = Regex::new(&to_regex("*{a*,/b}*").expect("valid")).expect("compiles");
        assert!(!regex.is_match("a/"));
        assert!(regex.is_match("xab"), "[^/]* then `a` then [^/]*");
        assert!(regex.is_match("x/by"), "[^/]* then `/b` then [^/]*");
        assert!(!reference_matches("*{a*,/b}*", "a/"));
    }

    #[test]
    fn the_reference_matcher_agrees_on_known_cases() {
        for (glob, text, expected) in [
            ("a*", "abc", true),
            ("a*", "a/b", false),
            ("a**", "a/b", true),
            ("{a,b}?", "bx", true),
            ("{a,b}?", "cx", false),
            ("x,y", "x,y", true),
        ] {
            assert_eq!(reference_matches(glob, text), expected, "{glob} vs {text}");
        }
    }

    fn glob_strategy() -> impl Strategy<Value = String> {
        let token = prop_oneof![
            4 => Just("a".to_owned()),
            3 => Just("b".to_owned()),
            2 => Just("/".to_owned()),
            1 => Just(".".to_owned()),
            1 => Just(",".to_owned()),
            1 => Just("-".to_owned()),
            3 => Just("*".to_owned()),
            1 => Just("**".to_owned()),
            2 => Just("?".to_owned()),
            1 => Just("{a,b}".to_owned()),
            1 => Just("{a*,/b}".to_owned()),
            1 => Just("{ab,b?,.}".to_owned()),
        ];
        prop::collection::vec(token, 1..9).prop_map(|parts| parts.concat())
    }

    fn text_strategy() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop::sample::select(vec!['a', 'b', '/', '.', ',', '-']),
            0..10,
        )
        .prop_map(|chars| chars.into_iter().collect())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]

        #[test]
        fn glob_translation_matches_reference(glob in glob_strategy(), text in text_strategy()) {
            prop_assume!(validate(&glob).is_ok());
            let regex = Regex::new(&to_regex(&glob).expect("valid")).expect("compiles");
            prop_assert_eq!(
                regex.is_match(&text),
                reference_matches(&glob, &text),
                "glob {:?} text {:?}", glob, text
            );
        }

        #[test]
        fn a_glob_matches_the_text_made_of_its_own_literals(glob in "[a-b./-]{1,12}") {
            // A glob without wildcards matches exactly itself.
            let regex = Regex::new(&to_regex(&glob).expect("valid")).expect("compiles");
            prop_assert!(regex.is_match(&glob));
            let longer = format!("{glob}a");
            prop_assert!(!regex.is_match(&longer));
        }

        #[test]
        fn must_not_emit_nested_quantifiers(glob in glob_strategy()) {
            prop_assume!(validate(&glob).is_ok());
            let regex = to_regex(&glob).expect("valid");
            assert_no_nested_quantifier(&regex);
        }
    }

    /// Walks the expression the way a regex parser would: an escaped character is a literal, a
    /// class `[^/]` is one atom, `*` is a quantifier, `)` closes a group. A quantifier after a
    /// group, after another quantifier or after a bound would be an unbounded construct.
    fn assert_no_nested_quantifier(regex: &str) {
        let chars: Vec<char> = regex.chars().collect();
        let mut index = 0;
        let mut after_quantifier = false;
        let mut after_group = false;
        while index < chars.len() {
            let mut step = 1;
            let (mut quantifier, mut group) = (false, false);
            match chars[index] {
                '\\' => step = 2,
                '[' => {
                    while chars[index + step - 1] != ']' {
                        step += 1;
                    }
                }
                '(' if chars[index + 1..].starts_with(&['?', ':']) => step = 3,
                '*' | '+' | '?' | '{' => {
                    assert!(!after_quantifier, "quantifier after quantifier in {regex}");
                    assert!(!after_group, "quantified group in {regex}");
                    quantifier = true;
                }
                ')' => group = true,
                _ => {}
            }
            after_quantifier = quantifier;
            after_group = group;
            index += step;
        }
    }

    #[test]
    fn the_nested_quantifier_checker_catches_what_it_should() {
        for bad in ["^(?:a)*$", "^a**$", "^(?:a|b)+$", "^[^/]*+$"] {
            let result = std::panic::catch_unwind(|| assert_no_nested_quantifier(bad));
            assert!(result.is_err(), "{bad} must be flagged");
        }
        for good in [r"^a\*[^/]*$", "^(?:a[^/]*|b)x$", r"^\(a\)*$"] {
            assert_no_nested_quantifier(good);
        }
    }
}
