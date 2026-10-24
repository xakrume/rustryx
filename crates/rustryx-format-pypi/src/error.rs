//! Errors in the shape `PyPI` clients read: the status code, a plain text body.
//!
//! pip and twine act on the status code and print the body. Twine shows the response text of a
//! failed upload, so the body is the client-safe message of the error and nothing else (the
//! rules of `FormatError::public_message` hold: no SQL, no paths, no upstream addresses).
//! A 401 carries `WWW-Authenticate: Basic`, which is what makes pip and twine send the
//! credentials they hold (an API token goes in as the password of the user `__token__`).

use axum_core::body::Body;
use http::{HeaderValue, Response, StatusCode, header};
use rustryx_format_core::{ErrorInfo, FormatError};

/// The challenge sent with every 401.
const CHALLENGE: &str = "Basic realm=\"Rustryx\", charset=\"UTF-8\"";

/// Renders `error` for a client.
pub(crate) fn render(error: &FormatError, info: &ErrorInfo<'_>) -> Response<Body> {
    let message = match error {
        FormatError::IndexTooLarge { .. } => "the package index is too large to serve".to_owned(),
        other => other.public_message(),
    };
    let mut response = Response::new(Body::from(format!("{message}\n")));
    *response.status_mut() = error.status();
    let headers = response.headers_mut();
    headers.extend(error.headers());
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if error.status() == StatusCode::UNAUTHORIZED {
        headers.insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static(CHALLENGE),
        );
    }
    if let Some(id) = info.request_id
        && let Ok(value) = HeaderValue::from_str(id)
    {
        headers.insert("x-request-id", value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustryx_core::model::{RepoFormat, RepoType};
    use rustryx_testkit::fixtures;

    fn render_for(error: &FormatError, anonymous: bool, id: Option<&str>) -> Response<Body> {
        let repository = fixtures::repository("p", RepoFormat::Pypi, RepoType::Hosted);
        render(
            error,
            &ErrorInfo {
                request_id: id,
                repository: &repository,
                anonymous,
            },
        )
    }

    #[test]
    fn a_401_challenges_and_a_403_does_not() {
        let unauthenticated = render_for(&FormatError::Unauthenticated, true, None);
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
        assert!(
            unauthenticated
                .headers()
                .get(header::WWW_AUTHENTICATE)
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("Basic")
        );
        let forbidden = render_for(&FormatError::Forbidden, false, None);
        assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
        assert!(forbidden.headers().get(header::WWW_AUTHENTICATE).is_none());
    }

    #[test]
    fn bodies_are_plain_text_and_carry_protocol_headers() {
        let conflict = render_for(
            &FormatError::conflict("File already exists: x"),
            false,
            Some("rid-1"),
        );
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        assert_eq!(
            conflict.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/plain; charset=utf-8"
        );
        assert_eq!(conflict.headers().get("x-request-id").unwrap(), "rid-1");
        let unavailable = render_for(
            &FormatError::Unavailable {
                reason: "busy",
                retry_after: Some(std::time::Duration::from_secs(2)),
            },
            false,
            None,
        );
        assert_eq!(unavailable.headers().get(header::RETRY_AFTER).unwrap(), "2");
        let method = render_for(
            &FormatError::MethodNotAllowed { allow: "POST" },
            false,
            None,
        );
        assert_eq!(method.headers().get(header::ALLOW).unwrap(), "POST");
        let large = render_for(&FormatError::IndexTooLarge { limit: 3 }, false, None);
        assert_eq!(large.status(), StatusCode::BAD_GATEWAY);
    }
}
