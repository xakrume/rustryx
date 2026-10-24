//! Content negotiation of a directory listing: HTML for browsers, JSON on request.
//!
//! Only the two representations this format has are weighed, so this is not a general
//! `Accept` parser. The rules: media ranges are read in order with their `q` value; the
//! representation with the higher best `q` wins; a tie goes to the range that was listed
//! first; `*/*` and `text/*` count for HTML, which is the default for a request that says
//! nothing (`curl`, a browser address bar).

use http::{HeaderMap, header};

/// True when the caller prefers the JSON listing.
#[must_use]
pub fn prefers_json(headers: &HeaderMap) -> bool {
    let mut json: Option<(f32, usize)> = None;
    let mut html: Option<(f32, usize)> = None;
    let mut position = 0usize;
    for value in headers.get_all(header::ACCEPT) {
        let Ok(text) = value.to_str() else { continue };
        for range in text.split(',') {
            let (essence, quality) = parse_range(range);
            position += 1;
            if quality <= 0.0 {
                continue;
            }
            let slot = match essence.as_str() {
                "application/json" => &mut json,
                "text/html" | "text/*" | "*/*" | "application/xhtml+xml" => &mut html,
                _ => continue,
            };
            // The best quality wins; the first listed range keeps its position on a tie.
            match slot {
                Some((best, _)) if *best >= quality => {}
                _ => *slot = Some((quality, position)),
            }
        }
    }
    match (json, html) {
        (Some(_), None) => true,
        (Some((json_q, json_at)), Some((html_q, html_at))) => {
            json_q > html_q || ((json_q - html_q).abs() < f32::EPSILON && json_at < html_at)
        }
        _ => false,
    }
}

/// The lowercase media type and the `q` weight (default 1, malformed means 1 as RFC 9110
/// treats an unparsable weight as absent; out of range is clamped).
fn parse_range(range: &str) -> (String, f32) {
    let mut parts = range.split(';');
    let essence = parts.next().unwrap_or_default().trim().to_ascii_lowercase();
    let quality = parts
        .find_map(|parameter| {
            let (name, value) = parameter.split_once('=')?;
            name.trim()
                .eq_ignore_ascii_case("q")
                .then(|| value.trim().parse::<f32>().ok())
                .flatten()
        })
        .map_or(1.0, |q| q.clamp(0.0, 1.0));
    (essence, quality)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    fn accept(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(header::ACCEPT, HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    #[test]
    fn json_only_when_the_caller_asks_for_it() {
        assert!(prefers_json(&accept(&["application/json"])));
        assert!(prefers_json(&accept(&["application/json, text/html"])));
        assert!(prefers_json(&accept(&[
            "application/json, text/plain, */*"
        ])));
        assert!(prefers_json(&accept(&["APPLICATION/JSON;charset=utf-8"])));
        assert!(prefers_json(&accept(&[
            "text/html;q=0.5",
            "application/json"
        ])));
    }

    #[test]
    fn html_is_the_default() {
        assert!(!prefers_json(&HeaderMap::new()));
        assert!(!prefers_json(&accept(&["*/*"])));
        assert!(!prefers_json(&accept(&["text/html"])));
        assert!(!prefers_json(&accept(&[
            "text/html,application/json;q=0.9"
        ])));
        assert!(!prefers_json(&accept(&["text/*, application/json"])));
        assert!(!prefers_json(&accept(&["application/xml"])));
        assert!(!prefers_json(&accept(&[""])));
    }

    #[test]
    fn a_weight_of_zero_excludes_a_representation() {
        assert!(!prefers_json(&accept(&["application/json;q=0"])));
        assert!(prefers_json(&accept(&[
            "text/html;q=0, application/json;q=0.1"
        ])));
    }

    #[test]
    fn malformed_weights_and_values_do_not_panic() {
        assert!(prefers_json(&accept(&[
            "application/json;q=abc, text/html;q=0.5"
        ])));
        assert!(!prefers_json(&accept(&["application/json;q=-5"])));
        assert!(prefers_json(&accept(&["application/json;q=7, text/html"])));
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap(),
        );
        assert!(!prefers_json(&headers));
    }
}
