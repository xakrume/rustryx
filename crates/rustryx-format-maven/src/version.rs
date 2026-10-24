//! Maven version ordering.
//!
//! The order follows the "Version Order Specification" of the POM reference
//! (<https://maven.apache.org/pom.html#version-order-specification>) and the behaviour that
//! `org.apache.maven.artifact.versioning.ComparableVersion` documents: a version is split
//! into numeric and string items at `.`, `-` and at every digit or letter transition, `-`
//! opens a sub list, trailing zero and release-equivalent items are dropped, and the string
//! qualifiers sort `alpha < beta < milestone < rc = cr < snapshot < "" = ga = final = release
//! < sp < any other qualifier (alphabetical)`.
//!
//! Why this lives here and not on `semver`: Maven versions are free text (`1.0.0.RELEASE`,
//! `2.0-beta-3`, `4.12-SNAPSHOT`) and the answer to "which is the latest" in
//! `maven-metadata.xml` must match what a Maven client computes for a version range.
//!
//! An underscore separates like a hyphen (the specification lists it next to `.` and `-`).
//!
//! The comparison is deliberately not wired into `Ord for str` wrappers that are handed to
//! `slice::sort_by`: the ordering is Maven's and is not proven to be a strict total order
//! for adversarial input, and `sort_by` is allowed to panic on one. [`sort_versions`] uses a
//! merge sort that terminates and never panics whatever the comparator returns.

use std::cmp::Ordering;

/// Qualifiers that Maven ranks, in ascending order. The empty string is the release.
const QUALIFIERS: [&str; 7] = ["alpha", "beta", "milestone", "rc", "snapshot", "", "sp"];

/// Position of the release (empty) qualifier in [`QUALIFIERS`].
const RELEASE_RANK: usize = 5;

/// Longest version text that is parsed; longer input compares as plain text. Bounds the work
/// an adversarial `<version>` element can cause in a merge.
const MAX_VERSION_LEN: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Item {
    /// Decimal digits without leading zeros (`"0"` for zero). Kept as text because a version
    /// such as `20240101120000123456789` does not fit any integer type.
    Int(String),
    /// Lowercase qualifier after alias resolution.
    Str(String),
    /// A `-` sub list.
    List(Vec<Item>),
}

impl Item {
    fn is_null(&self) -> bool {
        match self {
            Self::Int(digits) => digits == "0",
            Self::Str(text) => qualifier_rank(text) == (RELEASE_RANK, ""),
            Self::List(items) => items.is_empty(),
        }
    }
}

/// `(rank, text)`: known qualifiers rank by position, an unknown one ranks after `sp` and
/// is ordered alphabetically among the unknown ones.
fn qualifier_rank(text: &str) -> (usize, &str) {
    match QUALIFIERS.iter().position(|q| *q == text) {
        Some(index) => (index, ""),
        None => (QUALIFIERS.len(), text),
    }
}

fn make_string_item(text: &str, followed_by_digit: bool) -> Item {
    let text = if followed_by_digit && text.len() == 1 {
        match text {
            "a" => "alpha",
            "b" => "beta",
            "m" => "milestone",
            other => other,
        }
    } else {
        text
    };
    let canonical = match text {
        "ga" | "final" | "release" => "",
        "cr" => "rc",
        other => other,
    };
    Item::Str(canonical.to_owned())
}

fn make_item(digits: bool, text: &str) -> Item {
    if digits {
        let trimmed = text.trim_start_matches('0');
        Item::Int(if trimmed.is_empty() { "0" } else { trimmed }.to_owned())
    } else {
        make_string_item(text, false)
    }
}

/// Removes trailing null items. An empty or all-null sub list at the end counts as null; a
/// non empty sub list does not stop the scan, which is how `1.0-x` equals `1-x`.
fn normalize(items: &mut Vec<Item>) {
    let mut index = items.len();
    while index > 0 {
        index -= 1;
        let Some(last) = items.get(index) else { break };
        if last.is_null() {
            items.remove(index);
        } else if !matches!(last, Item::List(_)) {
            break;
        }
    }
}

/// Parses into the item tree. `stack` holds the lists under construction, the last one is the
/// innermost. Finishing folds each list into its parent after normalizing it, deepest first.
fn parse(version: &str) -> Vec<Item> {
    let lower = version.to_ascii_lowercase();
    let mut stack: Vec<Vec<Item>> = vec![Vec::new()];
    let mut is_digit = false;
    let mut start = 0usize;
    let bytes = lower.as_bytes();

    for (i, &byte) in bytes.iter().enumerate() {
        match byte {
            b'.' | b'-' | b'_' => {
                let item = if i == start {
                    Item::Int("0".to_owned())
                } else {
                    make_item(is_digit, lower.get(start..i).unwrap_or(""))
                };
                push_item(&mut stack, item);
                start = i + 1;
                is_digit = false;
                if byte != b'.' {
                    stack.push(Vec::new());
                }
            }
            b'0'..=b'9' => {
                if !is_digit && i > start {
                    // `1.0.0.x1` sorts like `1.0.0-x1`: a qualifier directly followed by a
                    // digit opens a sub list of its own.
                    if stack.last().is_some_and(|list| !list.is_empty()) {
                        stack.push(Vec::new());
                    }
                    push_item(
                        &mut stack,
                        make_string_item(lower.get(start..i).unwrap_or(""), true),
                    );
                    start = i;
                    stack.push(Vec::new());
                }
                is_digit = true;
            }
            _ => {
                if is_digit && i > start {
                    push_item(
                        &mut stack,
                        make_item(true, lower.get(start..i).unwrap_or("")),
                    );
                    start = i;
                    stack.push(Vec::new());
                }
                is_digit = false;
            }
        }
    }
    if bytes.len() > start {
        if !is_digit && stack.last().is_some_and(|list| !list.is_empty()) {
            stack.push(Vec::new());
        }
        push_item(
            &mut stack,
            make_item(is_digit, lower.get(start..).unwrap_or("")),
        );
    }
    fold(stack)
}

fn push_item(stack: &mut [Vec<Item>], item: Item) {
    if let Some(list) = stack.last_mut() {
        list.push(item);
    }
}

fn fold(mut stack: Vec<Vec<Item>>) -> Vec<Item> {
    while stack.len() > 1 {
        let Some(mut inner) = stack.pop() else { break };
        normalize(&mut inner);
        if let Some(parent) = stack.last_mut() {
            parent.push(Item::List(inner));
        }
    }
    let mut root = stack.pop().unwrap_or_default();
    normalize(&mut root);
    root
}

fn cmp_ranks(a: &str, b: &str) -> Ordering {
    qualifier_rank(a).cmp(&qualifier_rank(b))
}

fn cmp_ints(a: &str, b: &str) -> Ordering {
    // No leading zeros, so a longer digit string is a larger number.
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// Compares an item with "nothing there" (the other side is shorter).
fn cmp_with_nothing(item: &Item) -> Ordering {
    match item {
        Item::Int(digits) => {
            if digits == "0" {
                Ordering::Equal
            } else {
                Ordering::Greater
            }
        }
        Item::Str(text) => cmp_ranks(text, ""),
        Item::List(items) => match items.first() {
            None => Ordering::Equal,
            Some(first) => cmp_with_nothing(first),
        },
    }
}

/// A number outranks a sub list, which outranks a qualifier (`1.1 > 1-1 > 1-alpha`'s position
/// in the same slot), exactly as the reference comparison orders items of different kinds.
fn kind_rank(item: &Item) -> u8 {
    match item {
        Item::Str(_) => 0,
        Item::List(_) => 1,
        Item::Int(_) => 2,
    }
}

fn cmp_items(left: &Item, right: &Item) -> Ordering {
    match (left, right) {
        (Item::Int(a), Item::Int(b)) => cmp_ints(a, b),
        (Item::Str(a), Item::Str(b)) => cmp_ranks(a, b),
        (Item::List(a), Item::List(b)) => cmp_lists(a, b),
        _ => kind_rank(left).cmp(&kind_rank(right)),
    }
}

fn cmp_lists(left: &[Item], right: &[Item]) -> Ordering {
    let mut l = left.iter();
    let mut r = right.iter();
    loop {
        let result = match (l.next(), r.next()) {
            (None, None) => return Ordering::Equal,
            (Some(a), Some(b)) => cmp_items(a, b),
            (Some(a), None) => cmp_with_nothing(a),
            (None, Some(b)) => cmp_with_nothing(b).reverse(),
        };
        if result != Ordering::Equal {
            return result;
        }
    }
}

/// Compares two version strings in Maven order. `1.0`, `1` and `1.0.0` compare equal; use
/// [`sort_versions`] for a deterministic list (it breaks such ties by the text).
#[must_use]
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    if a.len() > MAX_VERSION_LEN || b.len() > MAX_VERSION_LEN {
        return a.cmp(b);
    }
    cmp_lists(&parse(a), &parse(b))
}

/// True for a snapshot version: it ends in `-SNAPSHOT` (the text Maven writes into the
/// version directory), with something before it.
#[must_use]
pub fn is_snapshot(version: &str) -> bool {
    version
        .strip_suffix("-SNAPSHOT")
        .is_some_and(|base| !base.is_empty())
}

/// Sorts versions ascending in Maven order, ties broken by the text so that the result does
/// not depend on the input order. Stable merge sort that never panics.
pub fn sort_versions(versions: &mut Vec<String>) {
    let parsed: Vec<(Vec<Item>, String)> = std::mem::take(versions)
        .into_iter()
        .map(|text| {
            let items = if text.len() > MAX_VERSION_LEN {
                Vec::new()
            } else {
                parse(&text)
            };
            (items, text)
        })
        .collect();
    let sorted = merge_sort(parsed, &|a, b| {
        if a.1.len() > MAX_VERSION_LEN || b.1.len() > MAX_VERSION_LEN {
            return a.1.cmp(&b.1);
        }
        cmp_lists(&a.0, &b.0).then_with(|| a.1.cmp(&b.1))
    });
    *versions = sorted.into_iter().map(|(_, text)| text).collect();
}

fn merge_sort<T, F>(mut items: Vec<T>, cmp: &F) -> Vec<T>
where
    F: Fn(&T, &T) -> Ordering,
{
    if items.len() <= 1 {
        return items;
    }
    let right = items.split_off(items.len() / 2);
    let left = merge_sort(items, cmp);
    let right = merge_sort(right, cmp);
    let mut out = Vec::with_capacity(left.len() + right.len());
    let mut left = left.into_iter().peekable();
    let mut right = right.into_iter().peekable();
    loop {
        match (left.peek(), right.peek()) {
            (Some(a), Some(b)) => {
                // `<=` keeps the sort stable.
                if cmp(a, b) == Ordering::Greater {
                    out.extend(right.next());
                } else {
                    out.extend(left.next());
                }
            }
            (Some(_), None) => out.extend(left.next()),
            (None, Some(_)) => out.extend(right.next()),
            (None, None) => break,
        }
    }
    out
}

/// The highest version of an iterator in Maven order (ties: the greater text).
#[must_use]
pub fn max_version<'a>(versions: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    versions
        .into_iter()
        .fold(None, |best, candidate| match best {
            None => Some(candidate),
            Some(current) => {
                match compare_versions(candidate, current).then_with(|| candidate.cmp(current)) {
                    Ordering::Greater => Some(candidate),
                    _ => Some(current),
                }
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn less(a: &str, b: &str) {
        assert_eq!(compare_versions(a, b), Ordering::Less, "{a} < {b}");
        assert_eq!(compare_versions(b, a), Ordering::Greater, "{b} > {a}");
    }

    fn equal(a: &str, b: &str) {
        assert_eq!(compare_versions(a, b), Ordering::Equal, "{a} == {b}");
        assert_eq!(compare_versions(b, a), Ordering::Equal, "{b} == {a}");
    }

    #[test]
    fn numeric_items_compare_as_numbers() {
        less("1", "1.1");
        less("1.2", "1.10");
        less("9", "10");
        less("1.0.1", "1.1");
        less("1.9.9", "2");
        equal("1", "1.0");
        equal("1.0", "1.0.0");
        equal("1.0", "1.00");
    }

    #[test]
    fn qualifiers_follow_the_documented_order() {
        less("1-alpha", "1-beta");
        less("1-beta", "1-milestone");
        less("1-milestone", "1-rc");
        less("1-rc", "1-snapshot");
        less("1-snapshot", "1");
        less("1", "1-sp");
        less("1-sp", "1-zzz");
        equal("1-rc", "1-cr");
        equal("1", "1-ga");
        equal("1", "1-final");
        equal("1", "1-release");
        equal("1.0.0", "1.0.0-GA");
    }

    #[test]
    fn single_letter_qualifiers_followed_by_a_digit_are_aliases() {
        equal("1.0-alpha-1", "1.0-a1");
        equal("1.0-beta2", "1.0-b2");
        equal("1.0-milestone-3", "1.0-m3");
        less("1.0-a1", "1.0-b1");
    }

    #[test]
    fn a_hyphen_opens_a_sub_list_that_sorts_before_the_next_numeric_item() {
        less("1-1", "1.1");
        less("1.0-1", "1.1");
        less("1", "1-1");
        less("1-1", "1-2");
        equal("1.0-x", "1-x");
    }

    #[test]
    fn an_underscore_separates_like_a_hyphen() {
        equal("1_alpha", "1-alpha");
        less("1_alpha", "1");
        equal("1_1", "1-1");
    }

    #[test]
    fn a_qualifier_after_a_dot_sorts_like_after_a_hyphen() {
        equal("1.0.0.x1", "1.0.0-x1");
        less("1.0.0.x1", "1.0.0-x2");
    }

    #[test]
    fn release_candidates_snapshots_and_final() {
        let mut versions: Vec<String> = [
            "1.0",
            "1.0-SNAPSHOT",
            "1.0-RC1",
            "1.0-beta-2",
            "1.0-alpha-1",
            "1.0.1",
            "1.0-sp1",
            "0.9",
            "1.0-beta-10",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        sort_versions(&mut versions);
        assert_eq!(
            versions,
            [
                "0.9",
                "1.0-alpha-1",
                "1.0-beta-2",
                "1.0-beta-10",
                "1.0-RC1",
                "1.0-SNAPSHOT",
                "1.0",
                "1.0-sp1",
                "1.0.1"
            ]
        );
    }

    #[test]
    fn huge_numbers_do_not_overflow() {
        less("20240101120000123456789", "20240101120000123456790");
        less("99999999999999999999999", "100000000000000000000000");
    }

    #[test]
    fn snapshot_detection_needs_the_suffix_and_a_base() {
        assert!(is_snapshot("1.0-SNAPSHOT"));
        assert!(is_snapshot("2.0.0-beta-SNAPSHOT"));
        assert!(!is_snapshot("1.0"));
        assert!(!is_snapshot("SNAPSHOT"));
        assert!(!is_snapshot("-SNAPSHOT"));
        assert!(!is_snapshot("1.0-snapshot"));
    }

    #[test]
    fn max_version_picks_the_highest_and_breaks_ties_by_text() {
        assert_eq!(max_version(["1.0", "1.10", "1.9"]), Some("1.10"));
        assert_eq!(max_version(["1", "1.0"]), Some("1.0"));
        assert_eq!(max_version(std::iter::empty::<&str>()), None);
    }

    #[test]
    fn degenerate_separators_and_long_input_do_not_panic() {
        for text in [
            "", ".", "-", "..", "--", "1..2", "1--2", "-1", "1-", ".1", "a.b-c.d",
        ] {
            let _ = compare_versions(text, "1");
            let _ = compare_versions("1", text);
        }
        let long = "1.".repeat(400);
        let _ = compare_versions(&long, "1");
        let mut versions = vec![long, "1".to_owned(), String::new()];
        sort_versions(&mut versions);
        assert_eq!(versions.len(), 3);
    }

    proptest! {
        #[test]
        fn must_not_panic_on_any_text(a in ".{0,80}", b in ".{0,80}") {
            let _ = compare_versions(&a, &b);
        }

        #[test]
        fn comparison_is_antisymmetric(a in "[0-9a-zA-Z.-]{0,24}", b in "[0-9a-zA-Z.-]{0,24}") {
            prop_assert_eq!(compare_versions(&a, &b), compare_versions(&b, &a).reverse());
        }

        #[test]
        fn comparison_is_reflexive(a in "[0-9a-zA-Z.-]{0,24}") {
            prop_assert_eq!(compare_versions(&a, &a), Ordering::Equal);
        }

        #[test]
        fn sorting_realistic_versions_is_idempotent_and_independent_of_input_order(
            versions in proptest::collection::vec(
                "[0-9]{1,3}(\\.[0-9]{1,3}){0,3}(-(alpha|beta|rc|SNAPSHOT|sp)[0-9]?)?",
                0..16
            )
        ) {
            let mut forward = versions.clone();
            sort_versions(&mut forward);
            let mut reversed: Vec<String> = versions.into_iter().rev().collect();
            sort_versions(&mut reversed);
            let mut again = forward.clone();
            sort_versions(&mut again);
            prop_assert_eq!(&forward, &again);
            prop_assert_eq!(forward, reversed);
        }

        #[test]
        fn must_not_panic_sorting_arbitrary_text(
            versions in proptest::collection::vec(".{0,12}", 0..16)
        ) {
            let mut versions = versions;
            sort_versions(&mut versions);
        }
    }
}
