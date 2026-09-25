//! Embedded configuration SPA (C-NO-LOCAL-WRITE, C-REDIS-ONLY-STATE).
//!
//! The UI is compiled into the executable so serving it creates no runtime files.
//! It sends the in-memory WORKER_TOKEN only to the same-origin `/api/config` routes;
//! the Worker forwards its Authorization header under SAF-LB-PASSTHRU, and configuration
//! persistence remains the backend's Redis responsibility.

use axum::{
    body::Body,
    http::{header, HeaderValue, Response, StatusCode},
    routing::get,
    Router,
};

const INDEX: &str = include_str!("../web/index.html");
const SCRIPT: &str = include_str!("../web/config.js");
const STYLES: &str = include_str!("../web/styles.css");

pub fn router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/assets/config.js", get(script))
        .route("/assets/styles.css", get(styles))
}

async fn index() -> Response<Body> {
    static_response("text/html; charset=utf-8", INDEX, true)
}

async fn script() -> Response<Body> {
    static_response("text/javascript; charset=utf-8", SCRIPT, false)
}

async fn styles() -> Response<Body> {
    static_response("text/css; charset=utf-8", STYLES, false)
}

fn static_response(content_type: &'static str, body: &'static str, html: bool) -> Response<Body> {
    let csp = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'";
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    if html {
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(csp),
        );
        headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    }
    response
}
