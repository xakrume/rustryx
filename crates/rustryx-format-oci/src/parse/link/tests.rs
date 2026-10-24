use proptest::prelude::*;

use super::*;

#[test]
fn reads_n_and_last() {
    let params = PageParams::from_query(Some("n=2&last=b")).unwrap();
    assert_eq!(params.n, Some(2));
    assert_eq!(params.last.as_deref(), Some("b"));
    assert_eq!(PageParams::from_query(None).unwrap(), PageParams::default());
    assert_eq!(
        PageParams::from_query(Some("x=1")).unwrap(),
        PageParams::default()
    );
}

#[test]
fn the_first_occurrence_wins_and_values_are_decoded() {
    let params = PageParams::from_query(Some("n=1&n=5&last=a%2Fb&last=c")).unwrap();
    assert_eq!(params.n, Some(1));
    assert_eq!(params.last.as_deref(), Some("a/b"));
}

#[test]
fn rejects_bad_n_and_last() {
    for bad in ["n=", "n=-1", "n=1.5", "n=abc", "n=9999999999", "n=%20"] {
        assert_eq!(
            PageParams::from_query(Some(bad)),
            Err(PageError::InvalidN),
            "{bad}"
        );
    }
    for bad in ["last=", "last=%00", "last=a%0Ab"] {
        assert_eq!(
            PageParams::from_query(Some(bad)),
            Err(PageError::InvalidLast),
            "{bad}"
        );
    }
    let long = format!("last={}", "a".repeat(600));
    assert_eq!(
        PageParams::from_query(Some(&long)),
        Err(PageError::InvalidLast)
    );
}

#[test]
fn a_huge_n_is_capped_not_refused() {
    let params = PageParams::from_query(Some("n=999999")).unwrap();
    assert_eq!(params.n, Some(MAX_PAGE_SIZE));
}

fn tags() -> Vec<String> {
    ["a", "b", "c", "d", "e"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
}

#[test]
fn apply_slices_after_last_and_limits_to_n() {
    let all = tags();
    let (page, more) = PageParams {
        n: Some(2),
        last: None,
    }
    .apply(&all);
    assert_eq!(page, ["a", "b"]);
    assert!(more);
    let (page, more) = PageParams {
        n: Some(2),
        last: Some("b".into()),
    }
    .apply(&all);
    assert_eq!(page, ["c", "d"]);
    assert!(more);
    let (page, more) = PageParams {
        n: Some(2),
        last: Some("d".into()),
    }
    .apply(&all);
    assert_eq!(page, ["e"]);
    assert!(!more);
    let (page, more) = PageParams {
        n: Some(5),
        last: None,
    }
    .apply(&all);
    assert_eq!(page.len(), 5);
    assert!(
        !more,
        "exactly n entries left is not a reason for a next link"
    );
}

#[test]
fn n_zero_is_an_empty_page_without_a_next_link() {
    let (page, more) = PageParams {
        n: Some(0),
        last: None,
    }
    .apply(&tags());
    assert!(page.is_empty());
    assert!(!more);
}

#[test]
fn last_that_is_not_in_the_list_still_positions_the_page() {
    let (page, _) = PageParams {
        n: None,
        last: Some("bb".into()),
    }
    .apply(&tags());
    assert_eq!(page, ["c", "d", "e"]);
    let (page, _) = PageParams {
        n: None,
        last: Some("zzz".into()),
    }
    .apply(&tags());
    assert!(page.is_empty());
}

#[test]
fn builds_a_next_link() {
    assert_eq!(
        next_link("/v2/repo/app/tags/list", Some(2), "b", &[]),
        "</v2/repo/app/tags/list?n=2&last=b>; rel=\"next\""
    );
    assert_eq!(
        next_link(
            "/v2/r/a/referrers/sha256:x",
            None,
            "a b/c",
            &[("artifactType", "x/y")]
        ),
        "</v2/r/a/referrers/sha256:x?last=a+b%2Fc&artifactType=x%2Fy>; rel=\"next\""
    );
}

#[test]
fn parses_the_next_target_of_an_upstream_link_header() {
    assert_eq!(
        parse_next_link("</v2/x/tags/list?last=b&n=2>; rel=\"next\"").as_deref(),
        Some("/v2/x/tags/list?last=b&n=2")
    );
    assert_eq!(
        parse_next_link("<https://a/prev>; rel=\"prev\", <https://a/next?x=1,2>; rel=next")
            .as_deref(),
        Some("https://a/next?x=1,2")
    );
    assert_eq!(
        parse_next_link("<a>; title=\"x, <b>; rel=next\"; rel=\"first\""),
        None
    );
    assert_eq!(parse_next_link(""), None);
    assert_eq!(parse_next_link("garbage"), None);
    assert_eq!(parse_next_link("<unterminated"), None);
    assert_eq!(
        parse_next_link("<a>; rel=\"prev next\"").as_deref(),
        Some("a")
    );
}

proptest! {
    #[test]
    fn query_parsing_never_panics(query in ".{0,200}") {
        let _ = PageParams::from_query(Some(&query));
    }

    #[test]
    fn link_parsing_never_panics(header in ".{0,300}") {
        let _ = parse_next_link(&header);
    }

    #[test]
    fn a_built_link_parses_back(last in "[a-zA-Z0-9._-]{1,40}", n in 0usize..1000) {
        let link = next_link("/v2/r/a/tags/list", Some(n), &last, &[]);
        let target = parse_next_link(&link).unwrap();
        let query = target.split_once('?').unwrap().1;
        let params = PageParams::from_query(Some(query)).unwrap();
        prop_assert_eq!(params.n, Some(n));
        prop_assert_eq!(params.last.as_deref(), Some(last.as_str()));
    }

    #[test]
    fn paging_through_a_list_visits_every_entry_once(len in 0usize..40, n in 1usize..8) {
        let all: Vec<String> = (0..len).map(|i| format!("t{i:03}")).collect();
        let mut seen = Vec::new();
        let mut last: Option<String> = None;
        loop {
            let (page, more) = PageParams { n: Some(n), last: last.clone() }.apply(&all);
            seen.extend(page.iter().cloned());
            match (more, page.last()) {
                (true, Some(tail)) => last = Some(tail.clone()),
                _ => break,
            }
        }
        prop_assert_eq!(seen, all);
    }
}
