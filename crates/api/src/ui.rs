//! The local web UI (ADR 0010): one page, a script and a stylesheet,
//! embedded in the binary and served with headers that keep third-party
//! content from ever running as code in this origin.

use axum::Router;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::AppState;

const INDEX: &str = include_str!("ui/index.html");
const APP_JS: &str = include_str!("ui/app.js");
const APP_CSS: &str = include_str!("ui/app.css");

/// Only this origin, no inline scripts or styles, no framing. `img-src data:`
/// is for the inline SVG favicon; `style-src` without 'unsafe-inline' still
/// allows the CSSOM writes the script makes (element.style.width).
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
                   connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/",
            get(|| async { asset("text/html; charset=utf-8", INDEX) }),
        )
        .route(
            "/ui/app.js",
            get(|| async { asset("text/javascript; charset=utf-8", APP_JS) }),
        )
        .route(
            "/ui/app.css",
            get(|| async { asset("text/css; charset=utf-8", APP_CSS) }),
        )
}

fn asset(content_type: &'static str, body: &'static str) -> Response {
    let mut res = body.into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    // The assets change only with the binary; revalidate rather than cache
    // across upgrades.
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    res
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use signalsift_core::Profile;
    use tower::ServiceExt;

    use crate::router;
    use crate::tests::test_state;

    #[tokio::test]
    async fn page_and_assets_are_served_with_a_strict_csp() {
        let dir = tempfile::tempdir().unwrap();
        let db = signalsift_storage::Db::open(&dir.path().join("t.db")).unwrap();
        let app = router(test_state(db, Profile::Balanced));
        for (path, kind) in [
            ("/", "text/html"),
            ("/ui/app.js", "text/javascript"),
            ("/ui/app.css", "text/css"),
        ] {
            let res = app
                .clone()
                .oneshot(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK, "{path}");
            let h = res.headers();
            assert!(
                h[header::CONTENT_TYPE].to_str().unwrap().starts_with(kind),
                "{path}"
            );
            let csp = h[header::CONTENT_SECURITY_POLICY].to_str().unwrap();
            assert!(
                csp.contains("script-src 'self'") && !csp.contains("unsafe-inline"),
                "{path}"
            );
            assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        }
    }

    #[test]
    fn the_script_never_injects_html() {
        // Third-party text must go through textContent; a stray innerHTML
        // or insertAdjacentHTML would turn summaries into markup.
        let js = super::APP_JS;
        for forbidden in [
            "innerHTML",
            "outerHTML",
            "insertAdjacentHTML",
            "document.write",
            "eval(",
        ] {
            assert!(!js.contains(forbidden), "app.js uses {forbidden}");
        }
        assert!(
            !super::INDEX.contains("<script>"),
            "no inline scripts (the CSP would block them)"
        );
    }
}
