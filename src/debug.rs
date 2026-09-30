//! `MOD-DEBUG` — optional remote debug / 联调 surface.
//!
//! Mounted only when **both** factors of `SAF-DEBUG-GATE` are present: the process was launched
//! with the `--debug` flag **and** the `DEBUG_TOKEN` Secret is non-empty. If either factor is
//! absent the router builder returns an empty router and no `/debug/*` route exists at all, so a
//! request falls through to the generic `404`. There is deliberately no "unconfigured = open"
//! fallback: the default state is absolutely closed.
//!
//! Even once mounted, every handler authenticates `Authorization: Bearer DEBUG_TOKEN` in constant
//! time (`SAF-DEBUG-AUTH`) and performs no side effect before that check passes; failure is `401`.
//! `REQ-DEBUG-ENDPOINTS`: the `GET` handlers are read-only, and `POST /debug/notify` reuses the
//! production outbound path (`TelegramClient::send_text`) for one test message.
//!
//! No response body ever contains a secret (`SAF-NO-SECRET-ECHO`) — only presence flags, counts
//! and probe verdicts — and `DEBUG_TOKEN` is never echoed or logged (`SAF-LOG-PURITY`). The
//! Worker safelist (`ARCH-LB-WORKER`) does not include `/debug/*`, so the public ingress cannot
//! reach this module; debug is for direct-origin remote 联调 only.

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use secrecy::ExposeSecret;

use crate::notify::{
    business_client, chat_allowlist_snapshot, error_response, probe_jmap_session,
    probe_telegram_get_me, refresh_business_config, worker_authorized, AppState,
};
use crate::state::StateError;

const DEFAULT_DEBUG_MESSAGE: &str = "messageweave debug: outbound test OK";
const MAX_DEBUG_MESSAGE_CHARS: usize = 1024;

/// `POST /debug/notify` parameters, taken from the query string so the handler authenticates
/// before any payload is interpreted (`SAF-DEBUG-AUTH`).
#[derive(serde::Deserialize)]
struct DebugNotifyQuery {
    chat_id: Option<i64>,
    text: Option<String>,
}

/// `SAF-DEBUG-AUTH`: Bearer `DEBUG_TOKEN`, constant-time, checked before any side effect.
fn debug_authorized(app: &AppState, headers: &HeaderMap) -> bool {
    match app.debug_token.as_ref() {
        Some(token) => worker_authorized(headers, token.expose_secret()),
        None => false,
    }
}

fn unauthorized() -> Response {
    error_response(StatusCode::UNAUTHORIZED, "unauthorized", false)
}

fn unavailable() -> Response {
    error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true)
}

fn chat_not_allowed() -> Response {
    error_response(StatusCode::FORBIDDEN, "chat_not_allowed", false)
}

fn send_failed() -> Response {
    error_response(StatusCode::BAD_GATEWAY, "telegram_send_failed", false)
}

/// `SAF-DEBUG-GATE` (second layer): an empty router, so callers that never merge this
/// builder simply never expose the surface. The caller decides the merge, so the default is
/// "absolutely closed": no `/debug/*` route exists unless the dual factor fired. This is
/// belt-and-suspenders on top of `main.rs` only merging under `--debug` + `DEBUG_TOKEN`.
pub(crate) fn debug_router() -> Router<AppState> {
    Router::new()
        .route("/debug/ping", get(debug_ping))
        .route("/debug/config", get(debug_config))
        .route("/debug/redis", get(debug_redis))
        .route("/debug/jmap", get(debug_jmap))
        .route("/debug/telegram", get(debug_telegram))
        .route("/debug/worker", get(debug_worker))
        .route("/debug/notify", post(debug_notify))
}

/// Liveness of the debug surface itself. No dependency is touched.
async fn debug_ping(State(app): State<AppState>, headers: HeaderMap) -> Response {
    if !debug_authorized(&app, &headers) {
        return unauthorized();
    }
    Json(serde_json::json!({ "ok": true })).into_response()
}

/// Redacted business-config snapshot: only presence flags, counts and non-secret values.
/// `SAF-NO-SECRET-ECHO`: bot token, JMAP password, worker/webhook/reconcile tokens and the LLM
/// key are reported as `*_configured` booleans only — never their values.
async fn debug_config(State(app): State<AppState>, headers: HeaderMap) -> Response {
    if !debug_authorized(&app, &headers) {
        return unauthorized();
    }
    refresh_business_config(&app).await;

    let revision = app.state.business_config_revision().await.unwrap_or(0);
    let setup_missing = app.setup_missing.as_ref().clone();
    let allowlist_size = chat_allowlist_snapshot(&app).len();
    let outbound = app.state.get_outbound_config().await.ok();
    let config = app
        .business_runtime
        .read()
        .ok()
        .and_then(|guard| guard.clone());

    let body = match config {
        Some(config) => serde_json::json!({
            "revision": revision,
            "setup_missing": setup_missing,
            "business_configured": true,
            "allowlist_size": allowlist_size,
            "timezone": config.timezone,
            "jmap": {
                "session_url": config.jmap_session_url,
                "username": config.jmap_username,
                "account_id": config.account_id,
            },
            "telegram": {
                "chat_id": config.telegram_chat_id,
                "webhook_secret_configured": !config.telegram_webhook_secret.expose_secret().is_empty(),
            },
            "worker": {
                "worker_token_configured": !config.worker_token.expose_secret().is_empty(),
                "reconcile_token_configured": !config.reconcile_token.expose_secret().is_empty(),
            },
            "llm": {
                "enabled": config.llm_enabled,
                "allow_net": config.llm_allow_net,
                "api_key_configured": config.llm_api_key.is_some(),
                "base_url": config.llm_base_url,
                "model": config.llm_model,
            },
            "outbound": outbound.map(|o| serde_json::json!({
                "jmap_timeout_ms": o.jmap_timeout_ms,
                "telegram_timeout_ms": o.telegram_timeout_ms,
                "llm_timeout_ms": o.llm_timeout_ms,
                "max_retries": o.max_retries,
            })),
        }),
        None => serde_json::json!({
            "revision": revision,
            "setup_missing": setup_missing,
            "business_configured": false,
            "allowlist_size": allowlist_size,
        }),
    };
    Json(body).into_response()
}

/// Redis reachability plus the persisted global enable flag.
async fn debug_redis(State(app): State<AppState>, headers: HeaderMap) -> Response {
    if !debug_authorized(&app, &headers) {
        return unauthorized();
    }
    let body = match app.state.is_enabled().await {
        Ok(enabled) => serde_json::json!({ "reachable": true, "global_enabled": enabled }),
        Err(err) => serde_json::json!({ "reachable": false, "detail": redis_failure_detail(&err) }),
    };
    Json(body).into_response()
}

/// Classify a Redis probe failure into a stable reason.
///
/// `StateError::Redis` carries the underlying `redis::RedisError`, so the class
/// can be read here without losing information. Previously every failure was
/// collapsed to the single string `"redis_probe_failed"`, which made an
/// authentication mistake, a dead TCP connection, and a network outage
/// indistinguishable from `/debug/redis`. `ErrorKind` is `#[non_exhaustive]`,
/// so the fallback keeps this compiling across redis minor releases.
fn redis_failure_detail(err: &StateError) -> &'static str {
    match err {
        StateError::Redis(redis_error) => match redis_error.kind() {
            redis::ErrorKind::AuthenticationFailed => "authentication_failed",
            redis::ErrorKind::IoError => "io_error",
            redis::ErrorKind::BusyLoadingError => "busy_loading",
            redis::ErrorKind::ClientError => "client_error",
            _ => "response_error",
        },
        StateError::Poisoned => "state_lock_poisoned",
        StateError::Encryption => "encryption_failed",
    }
}

/// JMAP reachability. Reuses `probe_jmap_session` so `/ready` and debug can never drift.
async fn debug_jmap(State(app): State<AppState>, headers: HeaderMap) -> Response {
    if !debug_authorized(&app, &headers) {
        return unauthorized();
    }
    refresh_business_config(&app).await;
    let body = match probe_jmap_session(&app).await {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(reason) => serde_json::json!({ "ok": false, "detail": reason }),
    };
    Json(body).into_response()
}

/// Telegram reachability. Reuses `probe_telegram_get_me` (token only ever rides in the URL).
async fn debug_telegram(State(app): State<AppState>, headers: HeaderMap) -> Response {
    if !debug_authorized(&app, &headers) {
        return unauthorized();
    }
    refresh_business_config(&app).await;
    let body = match probe_telegram_get_me(&app).await {
        Ok(()) => serde_json::json!({ "ok": true }),
        Err(reason) => serde_json::json!({ "ok": false, "detail": reason }),
    };
    Json(body).into_response()
}

/// Worker position: persisted JMAP changes cursor plus the live outbound runtime parameters.
async fn debug_worker(State(app): State<AppState>, headers: HeaderMap) -> Response {
    if !debug_authorized(&app, &headers) {
        return unauthorized();
    }
    let revision = app.state.business_config_revision().await.unwrap_or(0);
    let cursor = app.state.get_reconcile_state().await.ok().flatten();
    let outbound = app.state.get_outbound_config().await.ok();
    let body = serde_json::json!({
        "revision": revision,
        "reconcile_cursor": cursor,
        "outbound": outbound.map(|o| serde_json::json!({
            "jmap_timeout_ms": o.jmap_timeout_ms,
            "telegram_timeout_ms": o.telegram_timeout_ms,
            "llm_timeout_ms": o.llm_timeout_ms,
            "max_retries": o.max_retries,
        })),
    });
    Json(body).into_response()
}

/// Send one test message through the real outbound path. The chat defaults to the configured
/// business chat; an explicit `chat_id` must be inside the effective allowlist when one is set.
async fn debug_notify(
    State(app): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<DebugNotifyQuery>,
) -> Response {
    if !debug_authorized(&app, &headers) {
        return unauthorized();
    }

    let Some(config) = app
        .business_runtime
        .read()
        .ok()
        .and_then(|guard| guard.clone())
    else {
        return unavailable();
    };
    let chat_id = query.chat_id.unwrap_or(config.telegram_chat_id);
    let allowlist = chat_allowlist_snapshot(&app);
    if !allowlist.is_empty() && !allowlist.contains(&chat_id) {
        return chat_not_allowed();
    }
    let Some(client) = business_client(&app) else {
        return unavailable();
    };

    let text: String = query
        .text
        .unwrap_or_else(|| DEFAULT_DEBUG_MESSAGE.to_string())
        .chars()
        .take(MAX_DEBUG_MESSAGE_CHARS)
        .collect();

    match client.send_text(chat_id, &text).await {
        Ok(()) => Json(serde_json::json!({
            "ok": true,
            "chat_id": chat_id,
            "chars": text.chars().count(),
        }))
        .into_response(),
        Err(_) => send_failed(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AuthSecrets;
    use crate::notify::router_with_worker_state_runtime_bootstrap;
    use crate::state::{runtime_provider, MemoryState, OutboundConfig, ReliableState};
    use crate::worker::NoopWorker;
    use axum::{
        body::Body,
        http::{Method, Request},
    };
    use secrecy::SecretString;
    use std::collections::HashSet;
    use std::sync::Arc;
    use tower::ServiceExt;

    const DEBUG_TOKEN: &str = "debug-token-for-tests";

    /// Router built through the real dual-factor wiring: debug routes are merged only when the
    /// caller passes a `debug_token` (SAF-DEBUG-GATE).
    fn bootstrap_router_with_state<S: crate::state::ReliableState + 'static>(
        debug_token: Option<SecretString>,
        state: S,
    ) -> Router {
        router_with_worker_state_runtime_bootstrap(
            AuthSecrets {
                reconcile_token: SecretString::new("reconcile-secret".into()),
                telegram_webhook_secret: SecretString::new("telegram-secret".into()),
            },
            SecretString::new("worker-secret".into()),
            state,
            HashSet::new(),
            Arc::new(NoopWorker),
            runtime_provider(OutboundConfig::default()),
            SecretString::new(String::new()),
            debug_token,
        )
    }

    fn bootstrap_router(debug_token: Option<SecretString>) -> Router {
        bootstrap_router_with_state(debug_token, MemoryState::enabled_for_tests())
    }

    /// SAF-DEBUG-AUTH regression guard for the populated branch of `debug_config`: the timezone
    /// is a plain value the SPA writes, so it is echoed back for verification, while every
    /// credential stays a presence flag. It drives the whole chain — persisted wire →
    /// `refresh_business_config` → `build_worker` → `business_runtime` → `/debug/config` — so a
    /// silently dropped timezone fails here instead of in production.
    ///
    /// Opt-in like the JMAP smoke test: `business_runtime` is only populated after `build_worker`
    /// succeeds, and both `validate_business_wire` and `normalize_session_url` refuse a
    /// non-HTTPS session URL, so no localhost mock can stand in for a real session. Run with
    /// `JMAP_SESSION_URL`, `JMAP_USERNAME` and `JMAP_PASSWORD` set (and `ACCOUNT_ID` if the
    /// account is not the session's primary one).
    #[tokio::test]
    #[ignore = "requires an explicitly configured JMAP test server"]
    async fn debug_config_reports_timezone_of_business_configured_app() {
        let Some(session_url) = std::env::var("JMAP_SESSION_URL").ok() else {
            eprintln!("skipped: JMAP_SESSION_URL is not configured");
            return;
        };
        let Some(username) = std::env::var("JMAP_USERNAME").ok() else {
            eprintln!("skipped: JMAP_USERNAME is not configured");
            return;
        };
        let Some(password) = std::env::var("JMAP_PASSWORD").ok() else {
            eprintln!("skipped: JMAP_PASSWORD is not configured");
            return;
        };
        let account_id: serde_json::Value = match std::env::var("ACCOUNT_ID") {
            Ok(value) => serde_json::Value::String(value),
            Err(_) => serde_json::Value::Null,
        };

        // A negative supergroup chat id: Telegram ids are i64 and this one overflows an i32.
        let chat_id = -5_260_770_881_i64;
        let wire = serde_json::json!({
            "bot_token": "bot-secret-value",
            "telegram_chat_id": serde_json::Number::from(chat_id),
            "chat_allowlist": [42],
            "telegram_webhook_secret": "hook-secret-value",
            "timezone": "Asia/Tokyo",
            "jmap_session_url": session_url.clone(),
            "jmap_username": username.clone(),
            "jmap_password": password.clone(),
            "account_id": account_id,
            "llm_enabled": false,
            "llm_allow_net": false,
            "llm_api_key": null,
            "llm_base_url": null,
            "llm_model": null,
            "reconcile_token": "reconcile-secret-value",
            "worker_token": "worker-secret-value",
        });

        let state = MemoryState::enabled_for_tests();
        state.set_business_config(&wire).await.unwrap();
        let router =
            bootstrap_router_with_state(Some(SecretString::new(DEBUG_TOKEN.to_string())), state);
        let (status, value) = debug_get_json(router).await;
        let body = value.to_string();

        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["business_configured"], serde_json::json!(true));
        // The plaintext read-back is exactly what makes the timezone verifiable in production.
        assert_eq!(value["timezone"], serde_json::json!("Asia/Tokyo"));
        assert!(body.contains(&format!("\"session_url\":\"{session_url}\"")));
        assert!(body.contains(&format!("\"chat_id\":{chat_id}")));
        assert!(body.contains(&format!("\"username\":\"{username}\"")));
        assert!(value["jmap"].get("account_id").is_some());
        assert_eq!(value["allowlist_size"], serde_json::json!(1));
        assert_eq!(
            value["worker"]["reconcile_token_configured"],
            serde_json::json!(true)
        );
        assert_eq!(
            value["worker"]["worker_token_configured"],
            serde_json::json!(true)
        );
        assert_eq!(
            value["telegram"]["webhook_secret_configured"],
            serde_json::json!(true)
        );
        assert_eq!(value["llm"]["api_key_configured"], serde_json::json!(false));

        // SAF-DEBUG-AUTH: the fields exist, but no credential value may appear in the body.
        for secret in [
            "bot-secret-value",
            "hook-secret-value",
            &password,
            "reconcile-secret-value",
            "worker-secret-value",
            "reconcile-secret",
            "telegram-secret",
        ] {
            assert!(!body.contains(secret), "credential value leaked: {secret}");
        }
    }

    async fn get(uri: &str, authorization: Option<&str>) -> (StatusCode, String) {
        let router = bootstrap_router(Some(SecretString::new(DEBUG_TOKEN.to_string())));
        let mut builder = Request::builder().uri(uri);
        if let Some(token) = authorization {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        let response = router
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&body).to_string())
    }

    /// `/debug/config` against an already-built router, decoded as JSON so callers can assert on
    /// individual fields rather than substrings.
    async fn debug_get_json(router: Router) -> (StatusCode, serde_json::Value) {
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/debug/config")
                    .header("authorization", format!("Bearer {DEBUG_TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, serde_json::from_slice(&body).unwrap())
    }

    /// POST variant: the notification route is `POST`-only, so a GET would be a 405 rather than
    /// ever reaching the authentication check.
    async fn post(uri: &str, authorization: Option<&str>) -> StatusCode {
        let router = bootstrap_router(Some(SecretString::new(DEBUG_TOKEN.to_string())));
        let mut builder = Request::builder().method(Method::POST).uri(uri);
        if let Some(token) = authorization {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        router
            .oneshot(builder.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn debug_ping_accepts_debug_bearer() {
        let (status, body) = get("/debug/ping", Some(DEBUG_TOKEN)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains(r#""ok":true"#));
    }

    #[tokio::test]
    async fn debug_ping_rejects_missing_or_wrong_bearer() {
        assert_eq!(get("/debug/ping", None).await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(
            get("/debug/ping", Some("wrong")).await.0,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn debug_notify_requires_authentication() {
        assert_eq!(post("/debug/notify", None).await, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn debug_surface_absent_when_debug_token_absent() {
        // SAF-DEBUG-GATE: without the DEBUG_TOKEN factor no /debug/* route is registered at all,
        // so the request lands on the generic 404 instead of ever reaching a debug handler.
        let request = Request::builder()
            .uri("/debug/ping")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            bootstrap_router(None)
                .oneshot(request)
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
    }
}
