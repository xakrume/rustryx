//! Pagination: the `n` and `last` query parameters and the `Link: <...>; rel="next"` header.
//!
//! OCI Distribution Specification 1.1 (`tags/list`, `_catalog`, referrers): `n` limits the
//! number of entries, `last` is the last entry of the previous page (the next page starts
//! after it), a `Link` header with `rel="next"` says there is more. `n=0` is an empty
//! page without a `Link`. RFC 8288 defines the header.

/// Largest `n` taken at face value. A bigger number is a request for "everything", which
/// the scan caps of the registry bound anyway.
pub const MAX_PAGE_SIZE: usize = 100_000;

/// Longest `last` value. Tags are at most 128 characters and image names 255; the bound
/// leaves room for `<repo>/<image>` and keeps a hostile value out of the work done.
const MAX_LAST_LEN: usize = 512;

/// Why pagination parameters were refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PageError {
    /// `n` is not a non negative integer.
    #[error("the n parameter must be a non negative integer")]
    InvalidN,
    /// `last` is empty, too long or contains a control character.
    #[error("the last parameter is invalid")]
    InvalidLast,
}

/// The pagination parameters of a listing request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageParams {
    /// Maximum number of entries, `None` for no limit.
    pub n: Option<usize>,
    /// The entry after which the page starts.
    pub last: Option<String>,
}

impl PageParams {
    /// Reads `n` and `last` from a raw query string (the first occurrence of each wins).
    ///
    /// # Errors
    /// [`PageError::InvalidN`] or [`PageError::InvalidLast`].
    pub fn from_query(query: Option<&str>) -> Result<Self, PageError> {
        let mut params = Self::default();
        let Some(query) = query else {
            return Ok(params);
        };
        let mut seen_n = false;
        let mut seen_last = false;
        for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
            match key.as_ref() {
                "n" if !seen_n => {
                    seen_n = true;
                    params.n = Some(parse_n(&value)?);
                }
                "last" if !seen_last => {
                    seen_last = true;
                    if value.is_empty()
                        || value.len() > MAX_LAST_LEN
                        || value.chars().any(char::is_control)
                    {
                        return Err(PageError::InvalidLast);
                    }
                    params.last = Some(value.into_owned());
                }
                _ => {}
            }
        }
        Ok(params)
    }

    /// Applies the parameters to a sorted, deduplicated list: drops everything up to and
    /// including `last`, keeps at most `n` entries. Returns the page and whether more
    /// entries follow it.
    #[must_use]
    pub fn apply<T: AsRef<str> + Clone>(&self, sorted: &[T]) -> (Vec<T>, bool) {
        let start = match &self.last {
            None => 0,
            Some(last) => sorted.partition_point(|entry| entry.as_ref() <= last.as_str()),
        };
        let rest = sorted.get(start..).unwrap_or_default();
        match self.n {
            None => (rest.to_vec(), false),
            Some(n) => {
                let page: Vec<T> = rest.iter().take(n).cloned().collect();
                // `n=0` is an empty page and never has a successor link (specification).
                let more = n > 0 && rest.len() > n;
                (page, more)
            }
        }
    }
}

fn parse_n(value: &str) -> Result<usize, PageError> {
    if value.is_empty() || value.len() > 9 || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(PageError::InvalidN);
    }
    value
        .parse::<usize>()
        .map(|n| n.min(MAX_PAGE_SIZE))
        .map_err(|_| PageError::InvalidN)
}

/// Builds `<path?n=..&last=..>; rel="next"`. `extra` carries parameters that must survive
/// into the next request (the `artifactType` filter of referrers).
#[must_use]
pub fn next_link(path: &str, n: Option<usize>, last: &str, extra: &[(&str, &str)]) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    if let Some(n) = n {
        query.append_pair("n", &n.to_string());
    }
    query.append_pair("last", last);
    for (key, value) in extra {
        query.append_pair(key, value);
    }
    format!("<{path}?{}>; rel=\"next\"", query.finish())
}

/// Longest link target taken from an upstream `Link` header.
const MAX_LINK_TARGET: usize = 2048;

/// The target of the `rel="next"` link of an upstream `Link` header, if there is one.
///
/// Commas inside `<...>` and inside quoted parameter values do not separate links.
#[must_use]
pub fn parse_next_link(header: &str) -> Option<String> {
    let mut rest = header;
    loop {
        rest = rest.trim_start_matches(|c: char| c == ',' || c.is_ascii_whitespace());
        let after_open = rest.strip_prefix('<')?;
        let close = after_open.find('>')?;
        let target = &after_open[..close];
        let after_target = &after_open[close + 1..];
        let (params, remainder) = split_params(after_target);
        if has_next_relation(params) && target.len() <= MAX_LINK_TARGET {
            return Some(target.to_owned());
        }
        rest = remainder;
    }
}

/// Splits the parameters of one link from the rest of the header: up to the next comma
/// that is not inside a quoted string.
fn split_params(text: &str) -> (&str, &str) {
    let mut quoted = false;
    for (index, ch) in text.char_indices() {
        match ch {
            '"' => quoted = !quoted,
            ',' if !quoted => return (&text[..index], &text[index + 1..]),
            _ => {}
        }
    }
    (text, "")
}

fn has_next_relation(params: &str) -> bool {
    split_outside_quotes(params, ';').into_iter().any(|param| {
        let Some((name, value)) = param.split_once('=') else {
            return false;
        };
        name.trim().eq_ignore_ascii_case("rel")
            && value
                .trim()
                .trim_matches('"')
                .split_ascii_whitespace()
                .any(|relation| relation.eq_ignore_ascii_case("next"))
    })
}

/// Splits `text` at `separator`, except inside double quotes: a quoted parameter value may
/// contain the separator (and a hostile upstream may put `rel=next` inside one).
fn split_outside_quotes(text: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut quoted = false;
    let mut start = 0;
    for (index, ch) in text.char_indices() {
        if ch == '"' {
            quoted = !quoted;
        } else if ch == separator && !quoted {
            parts.push(text.get(start..index).unwrap_or_default());
            start = index + ch.len_utf8();
        }
    }
    parts.push(text.get(start..).unwrap_or_default());
    parts
}

#[cfg(test)]
mod tests;
