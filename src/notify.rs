// SPLIT-EVAL: 已评估暂缓拆分——全部 HTTP 处理器共享同一套 Bearer/admin-session 鉴权、统一错误封装与路由装配顺序，拆分会让每个子模块重复导入并复述这些前置条件。
use axum::{
    body::Bytes,
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use ring::rand::SecureRandom;
use secrecy::{ExposeSecret, SecretString};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, RwLock,
    },
};
use subtle::ConstantTimeEq;

use crate::ai::LlmClient;
use crate::channel::telegram::TelegramClient;
use crate::config::{
    session_digest, validate_business_wire, AuthSecrets, BusinessConfig, BusinessConfigPatch,
    BusinessConfigReadback, BusinessConfigWire,
};
use crate::domain::jmap::{client::JmapClientBackend, JmapService};
use crate::error::BotError;
use crate::state::ttl;
use crate::state::MemoryState;
use crate::state::{runtime_provider, OutboundConfig, ReliableState, RuntimeConfigProvider};
pub use crate::worker::{
    MetadataWorker, NoopWorker, ReloadCoordinator, WorkerHandle, WorkerHandler,
};

/// Build fingerprint baked in by `build.rs` (git SHA plus UTC build time, when available).
///
/// The SPA is `include_str!`-baked into this binary, so this is the only surface an operator
/// has to confirm that a deploy actually landed. See `build.rs` for the fallback chain.
pub(crate) const BUILD_VERSION: &str = env!("BUILD_VERSION");

#[derive(Clone, Debug, serde::Serialize)]
struct WorkerBuildFailure {
    component: String,
    step: String,
    detail: String,
}

impl WorkerBuildFailure {
    fn new(component: &str, step: &str, detail: String) -> Self {
        Self {
            component: component.to_owned(),
            step: step.to_owned(),
            detail: redact_credentials(&detail),
        }
    }
}

/// Removes URL credentials before a transport error reaches logs or a client.
///
/// jmap-client aborts redirects it does not trust and echoes the host it was pointed at;
/// Stalwart additionally bakes `user:pass` into the 307 `Location`. Anything of that shape
/// must not reach the log line or a preflight response body.
fn redact_credentials(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(marker) = rest.find("://") {
        let scheme_end = marker + 3;
        let Some(cred_start) = rest[scheme_end..].find('@') else {
            out.push_str(rest);
            return out;
        };
        let absolute_start = scheme_end + cred_start;
        out.push_str(&rest[..absolute_start]);
        out.push_str("<credentials>");
        rest = &rest[absolute_start + 1..];
    }
    out.push_str(rest);
    out
}

#[derive(Clone)]
struct AuthState {
    reconcile_token: SecretString,
    telegram_webhook_secret: SecretString,
}

#[derive(Clone)]
pub(crate) struct AppState {
    auth: AuthState,
    worker_token: SecretString,
    pub(crate) state: Arc<dyn ReliableState>,
    pub(crate) allowlist: Arc<HashSet<i64>>,
    worker: Arc<dyn WorkerHandler>,
    runtime: RuntimeConfigProvider,
    admin_token: SecretString,
    pub(crate) business_config: Arc<RwLock<Option<serde_json::Value>>>,
    pub(crate) business_runtime: Arc<RwLock<Option<BusinessConfig>>>,
    pub(crate) business_revision: Arc<RwLock<u64>>,
    pub(crate) setup_missing: Arc<Vec<String>>,
    reload: Arc<ReloadCoordinator>,
    /// Dual-factor debug gate (`SAF-DEBUG-GATE`): `Some` only when the process was launched with
    /// `--debug` AND `DEBUG_TOKEN` was set. `debug_router` is mounted iff this is `Some`, so the
    /// `/debug/*` surface is absent by default. `SAF-DEBUG-AUTH`: compared in constant time.
    pub(crate) debug_token: Option<SecretString>,
}

impl From<AuthSecrets> for AuthState {
    fn from(value: AuthSecrets) -> Self {
        Self {
            reconcile_token: value.reconcile_token,
            telegram_webhook_secret: value.telegram_webhook_secret,
        }
    }
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

/// Probe budget for readiness checks. Kept short so `/ready` fails fast instead of holding an
/// upstream connection open (no long-lived connections).
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(3_000);

/// Read the cached runtime configuration without holding the lock guard across an await.
/// Callers refresh the cache first so this never reports a stale view.
fn cached_jmap_session(app: &AppState) -> Option<(String, String, SecretString)> {
    app.business_runtime.read().ok()?.as_ref().map(|config| {
        (
            config.jmap_session_url.clone(),
            config.jmap_username.clone(),
            config.jmap_password.clone(),
        )
    })
}

fn cached_bot_token(app: &AppState) -> Option<secrecy::SecretString> {
    app.business_runtime
        .read()
        .ok()?
        .as_ref()
        .map(|config| config.bot_token.clone())
}

/// End-to-end JMAP session probe: fetch `/.well-known/jmap` with the configured credentials and
/// require a 2xx. Mirrors `JmapClientBackend::connect_with_runtime` (normalized origin + Basic
/// auth): probing the raw URL unauthenticated would report not-ready forever and make ingress
/// stop routing. Reusable by the remote debug surface so readiness and debugging share one
/// implementation.
pub(crate) async fn probe_jmap_session(app: &AppState) -> Result<(), &'static str> {
    let Some((session_url, username, password)) = cached_jmap_session(app) else {
        return Err("jmap session url not configured");
    };
    let origin = crate::domain::jmap::client::normalize_session_url(&session_url)
        .map_err(|_| "jmap session url invalid")?;
    let request = reqwest::Client::new()
        .get(format!("{origin}/.well-known/jmap"))
        .basic_auth(&username, Some(password.expose_secret()))
        .send();
    match tokio::time::timeout(PROBE_TIMEOUT, request).await {
        Ok(Ok(response)) => response
            .error_for_status()
            .map(|_| ())
            .map_err(|_| "jmap session probe failed"),
        Ok(Err(_)) => Err("jmap session probe failed"),
        Err(_) => Err("jmap session probe timed out"),
    }
}

/// End-to-end Telegram probe: `getMe` must answer with a 2xx. The bot token is used to build the
/// request URL only; it is never logged, echoed, or returned (SAF-NO-SECRET-ECHO).
pub(crate) async fn probe_telegram_get_me(app: &AppState) -> Result<(), &'static str> {
    let Some(token) = cached_bot_token(app) else {
        return Err("telegram bot token not configured");
    };
    let request = reqwest::Client::new()
        .get(format!(
            "https://api.telegram.org/bot{}/getMe",
            token.expose_secret()
        ))
        .send();
    match tokio::time::timeout(PROBE_TIMEOUT, request).await {
        Ok(Ok(response)) => response
            .error_for_status()
            .map(|_| ())
            .map_err(|_| "telegram get_me probe failed"),
        Ok(Err(_)) => Err("telegram get_me probe failed"),
        Err(_) => Err("telegram get_me probe timed out"),
    }
}

async fn ready(State(app): State<AppState>) -> Response {
    let configured = app.setup_missing.is_empty();
    let redis = app.state.is_enabled().await.is_ok();
    refresh_business_config(&app).await;
    // Probes run in parallel: worst case one PROBE_TIMEOUT instead of two.
    let (jmap, telegram) = tokio::join!(probe_jmap_session(&app), probe_telegram_get_me(&app));
    let jmap = jmap.is_ok();
    let telegram = telegram.is_ok();
    if configured && redis && jmap && telegram {
        axum::Json(serde_json::json!({
            "status": "ready",
            "configured": configured,
            "redis": redis,
            "jmap": jmap,
            "push": configured,
            "telegram": telegram,
        }))
        .into_response()
    } else {
        error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true)
    }
}

async fn setup_status(State(app): State<AppState>) -> Response {
    let ready = app.setup_missing.is_empty();
    axum::Json(serde_json::json!({
        "ready": ready,
        "mode": if ready { "configured" } else { "configuration-setup" },
        "missing": app.setup_missing.as_ref(),
        "version": BUILD_VERSION,
    }))
    .into_response()
}

async fn telegram_webhook(
    State(app): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    refresh_business_config(&app).await;
    if !business_enabled(&app).await {
        return error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true);
    }
    let (auth, _, allowlist) = auth_snapshot(&app);
    if !header_value_matches(
        &headers,
        "x-telegram-bot-api-secret-token",
        auth.telegram_webhook_secret.expose_secret(),
    ) {
        return error_response(StatusCode::UNAUTHORIZED, "unauthorized", false);
    }
    let Ok(update) = serde_json::from_slice::<TelegramUpdate>(&body) else {
        return error_response(StatusCode::BAD_REQUEST, "invalid_request", false);
    };
    if !allowlist.is_empty() && update.chat_id().is_some_and(|id| !allowlist.contains(&id)) {
        return error_response(StatusCode::FORBIDDEN, "forbidden", false);
    }
    let key = format!("dedup:tg:{}", update.update_id);
    match app.state.claim_dedup(&key, ttl::DEDUP_TG_SECONDS).await {
        Ok(false) => StatusCode::NO_CONTENT.into_response(),
        Ok(true) => match app
            .state
            .enqueue("stalwart:telegram", &String::from_utf8_lossy(&body))
            .await
        {
            Ok(_) => StatusCode::NO_CONTENT.into_response(),
            Err(_) => {
                let _ = app.state.release_dedup(&key).await;
                error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true)
            }
        },
        Err(_) => error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true),
    }
}

async fn reconcile(State(app): State<AppState>, headers: HeaderMap) -> Response {
    refresh_business_config(&app).await;
    if !business_enabled(&app).await {
        return error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true);
    }
    let (auth, _, _) = auth_snapshot(&app);
    let valid = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(|token| {
            constant_time_eq(
                token.as_bytes(),
                auth.reconcile_token.expose_secret().as_bytes(),
            )
        })
        .unwrap_or(false);
    if !valid {
        return error_response(StatusCode::UNAUTHORIZED, "unauthorized", false);
    }
    let lock_owner = reconcile_lock_owner();
    match app
        .state
        .acquire_lock("lock:reconcile", &lock_owner, ttl::RECONCILE_LOCK_SECONDS)
        .await
    {
        Ok(true) => {
            // Sizes only the first `/changes` call: the pass widens it afterwards.
            const RECONCILE_INITIAL_CHANGES: usize = 100;
            let heartbeat_state = Arc::clone(&app.state);
            let heartbeat_owner = lock_owner.clone();
            let lease_lost = Arc::new(AtomicBool::new(false));
            let heartbeat_lease_lost = Arc::clone(&lease_lost);
            let heartbeat = tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    match heartbeat_state
                        .renew_lock(
                            "lock:reconcile",
                            &heartbeat_owner,
                            ttl::RECONCILE_HEARTBEAT_SECONDS,
                        )
                        .await
                    {
                        Ok(true) => {}
                        _ => {
                            heartbeat_lease_lost.store(true, Ordering::Release);
                            break;
                        }
                    }
                }
            });
            let since = match app.state.get_reconcile_state().await {
                Ok(value) => value,
                Err(_) => {
                    heartbeat.abort();
                    let _ = app.state.release_lock("lock:reconcile", &lock_owner).await;
                    return error_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "service_unavailable",
                        true,
                    );
                }
            };
            let result = app
                .worker
                .reconcile(since.as_deref(), RECONCILE_INITIAL_CHANGES)
                .await;
            heartbeat.abort();
            let response = if lease_lost.load(Ordering::Acquire) {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                match result {
                    Ok(new_state) => match app.state.set_reconcile_state(&new_state).await {
                        Ok(()) => StatusCode::NO_CONTENT,
                        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
                    },
                    Err(()) => StatusCode::SERVICE_UNAVAILABLE,
                }
            };
            let _ = app.state.release_lock("lock:reconcile", &lock_owner).await;
            if response == StatusCode::SERVICE_UNAVAILABLE {
                error_response(response, "reconcile_retry", true)
            } else {
                response.into_response()
            }
        }
        Ok(false) => error_response(StatusCode::CONFLICT, "conflict", false),
        Err(_) => error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true),
    }
}

fn reconcile_lock_owner() -> String {
    let mut bytes = [0_u8; 16];
    if SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes).is_err() {
        // CSPRNG failure is effectively unreachable on Linux (getrandom-backed),
        // but never fall back to a constant token: two instances landing on the
        // same static value would make the release/renew CAS useless because
        // they could cancel each other's leases. Derive a still-unique value
        // from time + pid + counter instead (建议-2).
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nonce = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        return format!("fallback-{:x}-{:x}-{:x}", nanos, std::process::id(), nonce);
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn error_response(status: StatusCode, code: &'static str, retry: bool) -> Response {
    let request_id = reconcile_lock_owner();
    let body = axum::Json(serde_json::json!({"error": code, "request_id": request_id}));
    if retry {
        (status, [(header::RETRY_AFTER, "30")], body).into_response()
    } else {
        (status, body).into_response()
    }
}

async fn worker(State(app): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    refresh_business_config(&app).await;
    if !business_enabled(&app).await {
        return error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true);
    }
    let (_, worker_token, _) = auth_snapshot(&app);
    let valid = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(|token| constant_time_eq(token.as_bytes(), worker_token.expose_secret().as_bytes()))
        .unwrap_or(false);
    if !valid {
        error_response(StatusCode::UNAUTHORIZED, "unauthorized", false)
    } else {
        const GROUP: &str = "stalwart-workers";
        const CONSUMER: &str = "http-worker";
        const MAX: usize = 10;
        let batch = if body.is_empty() {
            MAX
        } else {
            let Ok(request) = serde_json::from_slice::<WorkerRequest>(&body) else {
                return error_response(StatusCode::BAD_REQUEST, "invalid_request", false);
            };
            request
                .batch
                .filter(|count| *count > 0)
                .map_or(MAX, |count| count.min(MAX))
        };
        for stream in ["stalwart:jmap", "stalwart:telegram"] {
            let messages = match app.state.read_batch(stream, GROUP, CONSUMER, batch).await {
                Ok(messages) => messages,
                Err(_) => {
                    return error_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "service_unavailable",
                        true,
                    )
                }
            };
            for message in messages.into_iter().take(batch) {
                // Commit is written only after send succeeds. Before send,
                // use a short in-flight lease so a crash cannot permanently
                // suppress an XAUTOCLAIM retry.
                let delivery_key = format!("delivery:committed:{stream}:{}", message.id);
                match app.state.dedup_exists(&delivery_key).await {
                    Ok(true) => {
                        if app.state.ack(stream, GROUP, &message.id).await.is_err() {
                            return error_response(
                                StatusCode::SERVICE_UNAVAILABLE,
                                "service_unavailable",
                                true,
                            );
                        }
                        continue;
                    }
                    Ok(false) => {}
                    Err(_) => {
                        return error_response(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "service_unavailable",
                            true,
                        )
                    }
                }
                let inflight_key = format!("delivery:inflight:{stream}:{}", message.id);
                match app
                    .state
                    .claim_dedup(&inflight_key, ttl::DELIVERY_INFLIGHT_SECONDS)
                    .await
                {
                    Ok(false) => continue,
                    Err(_) => {
                        return error_response(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "service_unavailable",
                            true,
                        )
                    }
                    Ok(true) => {}
                }
                if app.worker.process(stream, &message.payload).await.is_ok() {
                    if app
                        .state
                        .claim_dedup(&delivery_key, ttl::DELIVERY_COMMITTED_SECONDS)
                        .await
                        .is_err()
                    {
                        let _ = app.state.release_dedup(&inflight_key).await;
                        return error_response(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "service_unavailable",
                            true,
                        );
                    }
                    let _ = app.state.release_dedup(&inflight_key).await;
                    if app.state.ack(stream, GROUP, &message.id).await.is_err() {
                        return error_response(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "service_unavailable",
                            true,
                        );
                    }
                } else {
                    let _ = app.state.release_dedup(&inflight_key).await;
                    if app
                        .state
                        .retry_or_dlq(stream, &format!("{stream}:dlq"), GROUP, &message, 3)
                        .await
                        .is_err()
                    {
                        return error_response(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "service_unavailable",
                            true,
                        );
                    }
                }
            }
        }
        StatusCode::NO_CONTENT.into_response()
    }
}

pub(crate) fn worker_authorized(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(|value| constant_time_eq(value.as_bytes(), token.as_bytes()))
        .unwrap_or(false)
}

async fn config_authorized(app: &AppState, headers: &HeaderMap) -> bool {
    let (_, worker_token, _) = auth_snapshot(app);
    if worker_authorized(headers, worker_token.expose_secret()) {
        return true;
    }
    let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return false;
    };
    app.state
        .admin_session_valid(&session_digest(token))
        .await
        .unwrap_or(false)
}

async fn get_config(State(app): State<AppState>, headers: HeaderMap) -> Response {
    refresh_business_config(&app).await;
    if !config_authorized(&app, &headers).await {
        return error_response(StatusCode::UNAUTHORIZED, "unauthorized", false);
    }
    match app.state.get_outbound_config().await {
        Ok(config) => axum::Json(config).into_response(),
        Err(_) => error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true),
    }
}

async fn put_config(State(app): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    refresh_business_config(&app).await;
    if !config_authorized(&app, &headers).await {
        return error_response(StatusCode::UNAUTHORIZED, "unauthorized", false);
    }
    let Ok(config) = serde_json::from_slice::<OutboundConfig>(&body) else {
        return error_response(StatusCode::BAD_REQUEST, "invalid_request", false);
    };
    if !(100..=300_000).contains(&config.jmap_timeout_ms)
        || !(100..=300_000).contains(&config.telegram_timeout_ms)
        || !(100..=300_000).contains(&config.llm_timeout_ms)
        || config.max_retries > 5
    {
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_configuration",
            false,
        );
    }
    match app.state.set_outbound_config(&config).await {
        Ok(()) => {
            if let Ok(mut current) = app.runtime.write() {
                *current = config.clone();
            }
            axum::Json(config).into_response()
        }
        Err(_) => error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true),
    }
}

async fn get_enabled(State(app): State<AppState>, headers: HeaderMap) -> Response {
    if !config_authorized(&app, &headers).await {
        return error_response(StatusCode::UNAUTHORIZED, "unauthorized", false);
    }
    axum::Json(serde_json::json!({"enabled": business_enabled(&app).await})).into_response()
}

async fn put_enabled(State(app): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if !config_authorized(&app, &headers).await {
        return error_response(StatusCode::UNAUTHORIZED, "unauthorized", false);
    }
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return error_response(StatusCode::BAD_REQUEST, "invalid_request", false);
    };
    let Some(enabled) = payload.get("enabled").and_then(serde_json::Value::as_bool) else {
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_configuration",
            false,
        );
    };
    if app.state.set_enabled(enabled).await.is_err() {
        return error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true);
    }
    axum::Json(serde_json::json!({"enabled": enabled})).into_response()
}

async fn business_enabled(app: &AppState) -> bool {
    app.state.is_enabled().await.unwrap_or(false)
}

/// What the next business-config write needs to know about what is stored.
///
/// `Unreachable` and `Invalid` are kept distinct from `Absent`: both read as "nothing there",
/// but a patch merged over `Absent` would overwrite a configuration that is only unreachable or
/// unreadable, so either one refuses the write instead of silently truncating it.
enum StoredBusinessConfig {
    /// Nothing has ever been saved, so a patch has nothing to fall back onto.
    Absent,
    /// A stored configuration a patch can merge over.
    Loaded(Box<BusinessConfigWire>),
    /// The store could not be read: a partial write would destroy what is there.
    Unreachable,
    /// Something is stored but no longer parses as a wire: report it, never overwrite it as empty.
    Invalid,
}

async fn read_stored_business_config(state: &Arc<dyn ReliableState>) -> StoredBusinessConfig {
    match state.get_business_config().await {
        Ok(None) => StoredBusinessConfig::Absent,
        Ok(Some(value)) => match serde_json::from_value::<BusinessConfigWire>(value) {
            Ok(wire) => StoredBusinessConfig::Loaded(Box::new(wire)),
            Err(_) => StoredBusinessConfig::Invalid,
        },
        Err(_) => StoredBusinessConfig::Unreachable,
    }
}

/// Read the stored business configuration back to the SPA: the plaintext fields it needs to
/// change one thing without retyping the rest, plus a presence flag per secret. Never 404 — an
/// operator with nothing saved still gets 200 with `configured: false`, so the SPA needs one code
/// path instead of two.
async fn get_business_config(State(app): State<AppState>, headers: HeaderMap) -> Response {
    refresh_business_config(&app).await;
    if !config_authorized(&app, &headers).await {
        return error_response(StatusCode::UNAUTHORIZED, "unauthorized", false);
    }
    let revision = match app.state.business_config_revision().await {
        Ok(revision) => revision,
        Err(_) => {
            return error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true)
        }
    };
    match read_stored_business_config(&app.state).await {
        StoredBusinessConfig::Unreachable => {
            error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true)
        }
        StoredBusinessConfig::Invalid => error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_configuration",
            false,
        ),
        StoredBusinessConfig::Absent => {
            (StatusCode::OK, Json(BusinessConfigReadback::absent())).into_response()
        }
        StoredBusinessConfig::Loaded(wire) => (
            StatusCode::OK,
            Json(BusinessConfigReadback::from_wire(revision, &wire)),
        )
            .into_response(),
    }
}

/// Submit the business configuration, either in full or as a partial patch: the fields the client
/// names replace the stored ones and everything else is kept, so an operator can change one value
/// without retyping the whole configuration. Secrets are replace-only, which is also what lets the
/// SPA keep the stored value by sending nothing at all.
///
/// Persistence is gated by *validation*, not by connectivity. A configuration that is
/// syntactically valid is always written, even if no client can currently be built for it:
/// an operator must be able to save a half-migrated target, or one whose JMAP/LLM endpoint is
/// not reachable yet, and finish the wiring when the endpoint comes back. Requiring a live
/// connection for the write to succeed turns an unreachable dependency into a data-loss trap —
/// nothing could be saved while it is down, and there would be no log line saying why.
///
/// Validation failure and a genuine persistence failure still reject, because both would
/// contradict the write that was requested. A failed rebuild is reported in the response body
/// and in a redacted log line, and the previous runtime keeps serving: `business_config` is
/// updated either way (so `/api/config` and `/debug/config` can show what was saved) while
/// `business_runtime` — the configuration the live worker actually speaks — is replaced only
/// on success. The revision is advanced in both cases so `refresh_business_config` does not
/// rebuild on every request until the dependency recovers.
async fn put_business_config(
    State(app): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    refresh_business_config(&app).await;
    if !config_authorized(&app, &headers).await {
        return error_response(StatusCode::UNAUTHORIZED, "unauthorized", false);
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return error_response(StatusCode::BAD_REQUEST, "invalid_request", false);
    };
    let Ok(patch) = serde_json::from_value::<BusinessConfigPatch>(value) else {
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_configuration",
            false,
        );
    };
    // The client echoes back the revision it read before editing. A mismatch means another tab
    // stored a newer configuration in the meantime; merging this patch would silently revert the
    // fields it names, so refuse and make the operator re-read and retry. The compare-then-write
    // is not atomic: two requests that race inside the same tick both pass and the later one
    // wins. That residual is what an optimistic lock without a Lua script costs, and it stays
    // small because a patch carries only the fields the operator actually changed.
    if let Some(expected) = patch.revision {
        match app.state.business_config_revision().await {
            Ok(current) if current != expected => {
                return error_response(StatusCode::CONFLICT, "conflict", false);
            }
            Ok(_) => {}
            Err(_) => {
                return error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true)
            }
        }
    }

    // Merge first, then validate the result. A patch is never validated on its own: a
    // one-field edit must be rejected when the configuration it becomes is invalid.
    let merged = match read_stored_business_config(&app.state).await {
        StoredBusinessConfig::Unreachable => {
            return error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true)
        }
        StoredBusinessConfig::Invalid => {
            return error_response(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_configuration",
                false,
            )
        }
        StoredBusinessConfig::Loaded(stored) => patch.apply(Some(&stored)),
        StoredBusinessConfig::Absent => patch.apply(None),
    };
    let Some(wire) = merged else {
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_configuration",
            false,
        );
    };
    if validate_business_wire(wire.clone()).is_err() {
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_configuration",
            false,
        );
    }
    // Persist the merged wire, not the request body: a patch carries only the fields it changed,
    // so writing the body back would silently drop the rest of the configuration. `try_into`
    // moves `wire`, so the value is taken first.
    let Ok(value) = serde_json::to_value(&wire) else {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", true);
    };
    let Ok(config): Result<BusinessConfig, _> = wire.try_into() else {
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_configuration",
            false,
        );
    };
    if app.state.set_business_config(&value).await.is_err() {
        return error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true);
    }
    let (warnings, runtime_applied) = match build_worker_report(config.clone(), app.clone()).await {
        Ok(worker) => match app.reload.commit(config.clone(), worker) {
            Ok(()) => (Vec::new(), true),
            Err(()) => {
                return error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true)
            }
        },
        Err(failures) => {
            for failure in &failures {
                tracing::warn!(
                    component = %failure.component,
                    step = %failure.step,
                    detail = %failure.detail,
                    "business configuration persisted but its runtime could not be rebuilt; the previous runtime keeps serving"
                );
            }
            (failures, false)
        }
    };
    if let Ok(mut current) = app.business_config.write() {
        *current = Some(value);
    }
    if runtime_applied {
        if let Ok(mut current) = app.business_runtime.write() {
            *current = Some(config);
        }
    }
    let revision = match app.state.business_config_revision().await {
        Ok(revision) => revision,
        Err(_) => {
            return error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true)
        }
    };
    if let Ok(mut current) = app.business_revision.write() {
        *current = revision;
    }
    (
        StatusCode::OK,
        [("x-business-config-revision", revision.to_string())],
        Json(json!({
            "persisted": true,
            "revision": revision,
            "runtime_applied": runtime_applied,
            "warnings": warnings,
        })),
    )
        .into_response()
}

/// One-shot business-config bootstrap. The admin credential is only compared in constant time
/// and is never included in the response; SET-NX in ReliableState closes the init race.
///
/// Like `put_business_config`, persistence is gated by validation rather than connectivity.
/// SET-NX consumes the one-shot slot regardless of whether a live worker was built, which is
/// deliberate: the configuration is saved, the failure is reported and logged, and the same
/// body can be retried through `PUT /api/business-config` — which has no one-shot limit.
#[allow(clippy::too_many_lines)]
async fn bootstrap(State(app): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if !worker_authorized(&headers, app.admin_token.expose_secret()) {
        return error_response(StatusCode::UNAUTHORIZED, "unauthorized", false);
    }
    let Ok(config) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return error_response(StatusCode::BAD_REQUEST, "invalid_request", false);
    };
    let Ok(wire) = serde_json::from_value::<BusinessConfigWire>(config.clone()) else {
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_configuration",
            false,
        );
    };
    if validate_business_wire(wire.clone()).is_err() {
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_configuration",
            false,
        );
    }
    let Ok(next_config): Result<BusinessConfig, _> = wire.try_into() else {
        return error_response(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_configuration",
            false,
        );
    };
    match app.state.initialize_business_config(&config).await {
        Ok(true) => {
            let (warnings, runtime_applied) = match build_worker_report(
                next_config.clone(),
                app.clone(),
            )
            .await
            {
                Ok(next_worker) => match app.reload.commit(next_config.clone(), next_worker) {
                    Ok(()) => (Vec::new(), true),
                    Err(()) => {
                        return error_response(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "service_unavailable",
                            true,
                        )
                    }
                },
                Err(failures) => {
                    for failure in &failures {
                        tracing::warn!(
                            component = %failure.component,
                            step = %failure.step,
                            detail = %failure.detail,
                            "business configuration persisted but its runtime could not be rebuilt; the previous runtime keeps serving"
                        );
                    }
                    (failures, false)
                }
            };
            if let Ok(mut current) = app.business_config.write() {
                *current = Some(config);
            }
            if runtime_applied {
                if let Ok(mut current) = app.business_runtime.write() {
                    *current = Some(next_config);
                }
            }
            let revision = match app.state.business_config_revision().await {
                Ok(revision) => revision,
                Err(_) => {
                    return error_response(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "service_unavailable",
                        true,
                    )
                }
            };
            if let Ok(mut current) = app.business_revision.write() {
                *current = revision;
            }
            (
                StatusCode::OK,
                [("x-business-config-revision", revision.to_string())],
                Json(json!({
                    "persisted": true,
                    "runtime_applied": runtime_applied,
                    "warnings": warnings,
                })),
            )
                .into_response()
        }
        Ok(false) => error_response(StatusCode::CONFLICT, "conflict", false),
        Err(_) => error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true),
    }
}

/// Test a business configuration without saving it.
///
/// Same admin-session gate as the write endpoint, but nothing is persisted and no worker is
/// installed. It answers "why would saving this fail?" by running the exact validation and the
/// exact client construction the write path uses, and reports the result per component so one
/// broken endpoint does not hide the other. `components` is `null` when validation already
/// failed — building clients from an invalid wire format is meaningless.
///
/// This endpoint never fails the whole request for a configuration problem: a preflight whose
/// answer is "the configuration is bad" is a successful preflight, so the SPA can render the
/// reason instead of a generic HTTP error.
async fn preflight_business_config(
    State(app): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !config_authorized(&app, &headers).await {
        return error_response(StatusCode::UNAUTHORIZED, "unauthorized", false);
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return Json(json!({
            "persisted": false,
            "validation": { "ok": false, "errors": ["请求体不是有效的 JSON"] },
            "components": Value::Null,
        }))
        .into_response();
    };
    let Ok(wire) = serde_json::from_value::<BusinessConfigWire>(value.clone()) else {
        return Json(json!({
            "persisted": false,
            "validation": { "ok": false, "errors": ["JSON 不是有效的业务配置对象"] },
            "components": Value::Null,
        }))
        .into_response();
    };
    let errors = match validate_business_wire(wire.clone()) {
        Ok(()) => Vec::new(),
        Err(BotError::Config(message)) => vec![message],
        Err(error) => vec![error.to_string()],
    };
    if !errors.is_empty() {
        return Json(json!({
            "persisted": false,
            "validation": { "ok": false, "errors": errors },
            "components": Value::Null,
        }))
        .into_response();
    }
    let Ok(config): Result<BusinessConfig, _> = wire.try_into() else {
        return Json(json!({
            "persisted": false,
            "validation": { "ok": false, "errors": ["配置转换失败"] },
            "components": Value::Null,
        }))
        .into_response();
    };
    let llm_enabled = config.llm_enabled && config.llm_allow_net;
    let failures = match build_worker_report(config, app.clone()).await {
        Ok(_) => Vec::new(),
        Err(failures) => failures,
    };
    Json(json!({
        "persisted": false,
        "validation": { "ok": true, "errors": [] },
        "components": {
            "jmap": component_result(&failures, "jmap"),
            "llm": if llm_enabled {
                Some(component_result(&failures, "llm"))
            } else {
                None
            },
        },
    }))
    .into_response()
}

/// Projects the collected build failures onto one component. An empty projection means that
/// component built successfully, which is the only honest way to say "this one is fine".
fn component_result(failures: &[WorkerBuildFailure], component: &str) -> Value {
    let own: Vec<&WorkerBuildFailure> = failures
        .iter()
        .filter(|failure| failure.component == component)
        .collect();
    if own.is_empty() {
        json!({ "ok": true })
    } else {
        json!({ "ok": false, "errors": own })
    }
}

/// Builds the runtime backend chain, reporting *which components and steps* failed.
///
/// JMAP and LLM are probed independently rather than short-circuiting on the first failure:
/// the two clients share nothing, and an operator who saved a new JMAP endpoint wants to know
/// that the LLM endpoint was also rejected at the same time. `None` in the result therefore
/// means "no failure", and an empty vector in the error means "all components built".
///
/// `build_worker` is the compatibility wrapper for callers that do not need the detail.
async fn build_worker_report(
    config: crate::config::BusinessConfig,
    app: AppState,
) -> Result<Arc<dyn WorkerHandler>, Vec<WorkerBuildFailure>> {
    let mut failures = Vec::new();
    let jmap = match JmapClientBackend::connect_with_runtime(
        &config.jmap_session_url,
        &config.jmap_username,
        config.jmap_password.expose_secret(),
        config.account_id.as_deref(),
        app.runtime.clone(),
    )
    .await
    {
        Ok(backend) => {
            let account = backend.account_id().to_owned();
            match JmapService::new(backend, account) {
                Ok(service) => Some(service),
                Err(error) => {
                    failures.push(WorkerBuildFailure::new(
                        "jmap",
                        "account",
                        format!("{error:?}"),
                    ));
                    None
                }
            }
        }
        Err(error) => {
            failures.push(WorkerBuildFailure::new(
                "jmap",
                "connect",
                format!("{error:?}"),
            ));
            None
        }
    };
    let llm = if config.llm_enabled && config.llm_allow_net {
        match build_llm_client(config.clone(), app.runtime.clone()) {
            Ok(client) => Some(client),
            Err(failure) => {
                failures.push(failure);
                None
            }
        }
    } else {
        None
    };
    let Some(jmap) = jmap else {
        return Err(failures);
    };
    let telegram = TelegramClient::with_runtime(config.bot_token, app.runtime.clone());
    Ok(Arc::new(MetadataWorker::new(
        jmap,
        telegram,
        config.telegram_chat_id,
        app.state,
        llm,
        crate::config::timezone_offset_of(&config.timezone),
    )))
}

/// Builds the optional LLM client, separating its own configuration problems from its build
/// problems so the caller can report the exact gap.
fn build_llm_client(
    config: crate::config::BusinessConfig,
    runtime: RuntimeConfigProvider,
) -> Result<Arc<LlmClient>, WorkerBuildFailure> {
    let key = config
        .llm_api_key
        .ok_or_else(|| WorkerBuildFailure::new("llm", "config", "llm_api_key 缺失".to_owned()))?;
    let base_url = config
        .llm_base_url
        .ok_or_else(|| WorkerBuildFailure::new("llm", "config", "llm_base_url 缺失".to_owned()))?;
    let model = config
        .llm_model
        .ok_or_else(|| WorkerBuildFailure::new("llm", "config", "llm_model 缺失".to_owned()))?;
    LlmClient::with_runtime(base_url, key, model, 300, runtime)
        .map(Arc::new)
        .map_err(|error| WorkerBuildFailure::new("llm", "build", format!("{error:?}")))
}

/// Compatibility wrapper: rebuild failures here are not surfaced to the caller.
async fn build_worker(
    config: crate::config::BusinessConfig,
    app: AppState,
) -> Result<Arc<dyn WorkerHandler>, ()> {
    build_worker_report(config, app).await.map_err(|_| ())
}

/// Request-boundary refresh for multi-instance deployments. A failed remote rebuild advances the
/// observed revision to avoid a hot retry loop while retaining the active worker/config.
/// `REQ-DEBUG-ENDPOINTS`: the single place a business-configured Telegram client is built from the
/// live `business_runtime` snapshot. `refresh_business_config` and `/debug/notify` both go through
/// it, so there is never a second Telegram client implementation to drift apart.
pub(crate) fn business_client(app: &AppState) -> Option<TelegramClient> {
    app.business_runtime
        .read()
        .ok()
        .and_then(|guard| guard.clone())
        .map(|config| TelegramClient::with_runtime(config.bot_token, app.runtime.clone()))
}

pub(crate) async fn refresh_business_config(app: &AppState) {
    let Ok(remote_revision) = app.state.business_config_revision().await else {
        return;
    };
    let local_revision = app.business_revision.read().map(|v| *v).unwrap_or(0);
    if remote_revision <= local_revision {
        return;
    }
    let Ok(Some(value)) = app.state.get_business_config().await else {
        return;
    };
    let Ok(wire) = serde_json::from_value::<BusinessConfigWire>(value.clone()) else {
        return;
    };
    if validate_business_wire(wire.clone()).is_err() {
        return;
    }
    let Ok(config): Result<BusinessConfig, _> = wire.try_into() else {
        return;
    };
    let Ok(worker) = build_worker(config.clone(), app.clone()).await else {
        if let Ok(mut revision) = app.business_revision.write() {
            *revision = remote_revision;
        }
        return;
    };
    if app.reload.commit(config.clone(), worker).is_err() {
        return;
    }
    if let Ok(mut snapshot) = app.business_config.write() {
        *snapshot = Some(value);
    }
    if let Ok(mut snapshot) = app.business_runtime.write() {
        *snapshot = Some(config);
    }
    if let Ok(mut revision) = app.business_revision.write() {
        *revision = remote_revision;
    }
}

/// Session lifecycle endpoint. The caller supplies the opaque session bearer; digesting is
/// intentionally delegated to the pending SHA-256 adapter, while Redis TTL semantics are live.
async fn revoke_admin_session(State(app): State<AppState>, headers: HeaderMap) -> Response {
    let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return error_response(StatusCode::UNAUTHORIZED, "unauthorized", false);
    };
    let digest = session_digest(token);
    match app.state.revoke_admin_session(&digest).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true),
    }
}

async fn create_admin_session(State(app): State<AppState>, headers: HeaderMap) -> Response {
    if !worker_authorized(&headers, app.admin_token.expose_secret()) {
        return error_response(StatusCode::UNAUTHORIZED, "unauthorized", false);
    }
    let mut bytes = [0_u8; 32];
    if ring::rand::SystemRandom::new().fill(&mut bytes).is_err() {
        return error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal_error", false);
    }
    let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let digest = session_digest(&token);
    if app
        .state
        .put_admin_session(&digest, ttl::ADMIN_SESSION_SECONDS)
        .await
        .is_err()
    {
        return error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true);
    }
    axum::Json(serde_json::json!({
        "session": token,
        "expires_in": ttl::ADMIN_SESSION_SECONDS
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
struct WorkerRequest {
    batch: Option<usize>,
}

async fn jmap_push(State(app): State<AppState>, body: Bytes) -> Response {
    refresh_business_config(&app).await;
    if !business_enabled(&app).await {
        return error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true);
    }
    let Ok(push) = serde_json::from_slice::<JmapPush>(&body) else {
        return error_response(StatusCode::BAD_REQUEST, "invalid_request", false);
    };
    let (Some(subscription), Some(code)) = (
        push.subscription_id.as_deref(),
        push.verification_code.as_deref(),
    ) else {
        return error_response(StatusCode::UNAUTHORIZED, "unauthorized", false);
    };
    if !matches!(
        app.state
            .push_subscription_verified(subscription, code)
            .await,
        Ok(true)
    ) {
        let limit_key = format!("ratelimit:push-verify:{subscription}");
        if matches!(
            app.state
                .claim_dedup(&limit_key, ttl::PUSH_VERIFY_LIMIT_SECONDS)
                .await,
            Ok(false)
        ) {
            return error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "push_verify_rate_limited",
                true,
            );
        }
        if app
            .worker
            .verify_push_subscription(subscription, code)
            .await
            .is_err()
        {
            return error_response(StatusCode::SERVICE_UNAVAILABLE, "push_verify_failed", true);
        }
        if app
            .state
            .remember_push_subscription(subscription, code, ttl::PUSH_SUBSCRIPTION_SECONDS)
            .await
            .is_err()
        {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "push_state_unavailable",
                true,
            );
        }
        let _ = app
            .state
            .set_push_subscription_status(
                subscription,
                "verified",
                ttl::PUSH_STATUS_VERIFIED_SECONDS,
            )
            .await;
    }
    if push.account_id.is_none() && push.email_id.is_none() {
        return StatusCode::NO_CONTENT.into_response();
    }
    let (Some(account), Some(email)) = (push.account_id, push.email_id) else {
        return error_response(StatusCode::BAD_REQUEST, "invalid_request", false);
    };
    let key = format!("dedup:jmap:{account}:{email}");
    match app.state.claim_dedup(&key, ttl::DEDUP_JMAP_SECONDS).await {
        Ok(false) => StatusCode::NO_CONTENT.into_response(),
        Ok(true) => match app
            .state
            .enqueue("stalwart:jmap", &String::from_utf8_lossy(&body))
            .await
        {
            Ok(_) => StatusCode::NO_CONTENT.into_response(),
            Err(_) => {
                let _ = app.state.release_dedup(&key).await;
                error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true)
            }
        },
        Err(_) => error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true),
    }
}

#[derive(serde::Deserialize)]
struct PushRegistration {
    callback_url: String,
}

#[derive(serde::Deserialize)]
struct PushDisable {
    callback_url: String,
}

/// Registers the callback through JMAP; the verification code is generated by
/// Stalwart and is never accepted from SPA configuration.
async fn register_push(State(app): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if !config_authorized(&app, &headers).await {
        return error_response(StatusCode::UNAUTHORIZED, "unauthorized", false);
    }
    let Ok(request) = serde_json::from_slice::<PushRegistration>(&body) else {
        return error_response(StatusCode::BAD_REQUEST, "invalid_request", false);
    };
    let Ok(url) = url::Url::parse(&request.callback_url) else {
        return error_response(StatusCode::BAD_REQUEST, "invalid_request", false);
    };
    if url.scheme() != "https" || url.username() != "" || url.password().is_some() {
        return error_response(StatusCode::BAD_REQUEST, "invalid_request", false);
    }
    let registration_lock = format!(
        "lock:push-register:{}",
        session_digest(&request.callback_url)
    );
    let registration_owner = reconcile_lock_owner();
    match app
        .state
        // The lock must outlive the configured 300s maximum JMAP request
        // timeout; this prevents a slow create from admitting a duplicate.
        .acquire_lock(
            &registration_lock,
            &registration_owner,
            ttl::PUSH_REGISTER_LOCK_SECONDS,
        )
        .await
    {
        Ok(false) => return error_response(StatusCode::CONFLICT, "conflict", false),
        Err(_) => {
            return error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true)
        }
        Ok(true) => {}
    }
    if let Ok(Some(subscription_id)) = app
        .state
        .get_push_subscription_for_callback(&request.callback_url)
        .await
    {
        let _ = app
            .state
            .release_lock(&registration_lock, &registration_owner)
            .await;
        return axum::Json(serde_json::json!({
            "push_subscription_id": subscription_id,
            "idempotent": true
        }))
        .into_response();
    }
    let Ok(subscription_id) = app
        .worker
        .create_push_subscription(&request.callback_url)
        .await
    else {
        let _ = app
            .state
            .release_lock(&registration_lock, &registration_owner)
            .await;
        return error_response(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable", true);
    };
    if app
        .state
        .remember_push_subscription_id(&subscription_id)
        .await
        .is_err()
    {
        let request_id = cleanup_push_orphan(&app, &subscription_id).await;
        let _ = app
            .state
            .release_lock(&registration_lock, &registration_owner)
            .await;
        return error_response_with_id(
            StatusCode::SERVICE_UNAVAILABLE,
            "push_state_unavailable",
            request_id,
        );
    }
    if app
        .state
        .set_push_subscription_status(
            &subscription_id,
            "pending",
            ttl::PUSH_STATUS_PENDING_SECONDS,
        )
        .await
        .is_err()
    {
        let request_id = cleanup_push_orphan(&app, &subscription_id).await;
        let _ = app
            .state
            .release_lock(&registration_lock, &registration_owner)
            .await;
        return error_response_with_id(
            StatusCode::SERVICE_UNAVAILABLE,
            "push_state_unavailable",
            request_id,
        );
    }
    if app
        .state
        .remember_push_subscription_for_callback(&request.callback_url, &subscription_id)
        .await
        .is_err()
    {
        let request_id = cleanup_push_orphan(&app, &subscription_id).await;
        let _ = app
            .state
            .release_lock(&registration_lock, &registration_owner)
            .await;
        return error_response_with_id(
            StatusCode::SERVICE_UNAVAILABLE,
            "push_state_unavailable",
            request_id,
        );
    }
    let _ = app
        .state
        .release_lock(&registration_lock, &registration_owner)
        .await;
    axum::Json(serde_json::json!({"push_subscription_id": subscription_id})).into_response()
}

/// Disable and remove a persisted push registration. Destruction is attempted
/// before deleting the callback mapping so a transient JMAP failure is
/// retryable and cannot silently orphan a live subscription.
async fn disable_push(State(app): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if !config_authorized(&app, &headers).await {
        return error_response(StatusCode::UNAUTHORIZED, "unauthorized", false);
    }
    let Ok(request) = serde_json::from_slice::<PushDisable>(&body) else {
        return error_response(StatusCode::BAD_REQUEST, "invalid_request", false);
    };
    let Ok(Some(subscription_id)) = app
        .state
        .get_push_subscription_for_callback(&request.callback_url)
        .await
    else {
        return error_response(StatusCode::NOT_FOUND, "push_subscription_not_found", false);
    };
    if app
        .worker
        .destroy_push_subscription(&subscription_id)
        .await
        .is_err()
    {
        let request_id = cleanup_push_orphan(&app, &subscription_id).await;
        return error_response_with_id(
            StatusCode::SERVICE_UNAVAILABLE,
            "push_destroy_failed",
            request_id,
        );
    }
    if app
        .state
        .set_push_subscription_status(&subscription_id, "disabled", ttl::PUSH_DISABLED_SECONDS)
        .await
        .is_err()
    {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "push_state_unavailable",
            true,
        );
    }
    // Drop the verification digest as well: otherwise a push arriving within the
    // residual TTL of the last verify call still passes
    // `push_subscription_verified` and gets enqueued after disable.
    if app
        .state
        .forget_push_subscription(&subscription_id)
        .await
        .is_err()
    {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "push_state_unavailable",
            true,
        );
    }
    if app
        .state
        .remove_push_subscription_for_callback(&request.callback_url)
        .await
        .is_err()
    {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "push_state_unavailable",
            true,
        );
    }
    axum::Json(serde_json::json!({"disabled": true})).into_response()
}

async fn cleanup_push_orphan(app: &AppState, subscription_id: &str) -> String {
    let request_id = reconcile_lock_owner();
    if app
        .worker
        .destroy_push_subscription(subscription_id)
        .await
        .is_err()
    {
        let _ = app
            .state
            .record_push_orphan(subscription_id, &request_id)
            .await;
    }
    request_id
}

fn error_response_with_id(status: StatusCode, code: &'static str, request_id: String) -> Response {
    (
        status,
        [(header::RETRY_AFTER, "30")],
        axum::Json(serde_json::json!({"error": code, "request_id": request_id})),
    )
        .into_response()
}

#[derive(serde::Deserialize)]
struct TelegramUpdate {
    update_id: u64,
    #[serde(default)]
    message: Option<TelegramMessage>,
}
#[derive(serde::Deserialize)]
struct TelegramMessage {
    chat: TelegramChat,
}
#[derive(serde::Deserialize)]
struct TelegramChat {
    id: i64,
}
impl TelegramUpdate {
    fn chat_id(&self) -> Option<i64> {
        self.message.as_ref().map(|m| m.chat.id)
    }
}
#[derive(serde::Deserialize)]
struct JmapPush {
    #[serde(rename = "pushSubscriptionId")]
    subscription_id: Option<String>,
    #[serde(rename = "verificationCode")]
    verification_code: Option<String>,
    #[serde(rename = "accountId")]
    account_id: Option<String>,
    #[serde(rename = "emailId")]
    email_id: Option<String>,
}

fn header_value_matches(headers: &HeaderMap, name: &str, expected: &str) -> bool {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(|actual| constant_time_eq(actual.as_bytes(), expected.as_bytes()))
        .unwrap_or(false)
}

pub(crate) fn constant_time_eq(actual: &[u8], expected: &[u8]) -> bool {
    !expected.is_empty() && actual.len() == expected.len() && actual.ct_eq(expected).into()
}

/// Effective chat allowlist (runtime business config if present, else the static startup
/// value). Exposed to `MOD-DEBUG` without leaking `AuthState` itself.
pub(crate) fn chat_allowlist_snapshot(app: &AppState) -> HashSet<i64> {
    auth_snapshot(app).2
}

fn auth_snapshot(app: &AppState) -> (AuthState, SecretString, HashSet<i64>) {
    if let Ok(snapshot) = app.business_runtime.read() {
        if let Some(config) = snapshot.as_ref() {
            return (
                AuthState {
                    reconcile_token: config.reconcile_token.clone(),
                    telegram_webhook_secret: config.telegram_webhook_secret.clone(),
                },
                config.worker_token.clone(),
                config.chat_allowlist.iter().copied().collect(),
            );
        }
    }
    (
        app.auth.clone(),
        app.worker_token.clone(),
        (*app.allowlist).clone(),
    )
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "稳定ID+阶段0兼容测试路由；生产使用 router_with_worker_state"
    )
)]
pub fn router_with_state<S: ReliableState + 'static>(
    secrets: AuthSecrets,
    worker_token: SecretString,
    state: S,
    allowlist: HashSet<i64>,
) -> Router {
    router_with_worker_state(
        secrets,
        worker_token,
        state,
        allowlist,
        Arc::new(NoopWorker),
    )
}

/// Configuration-only listener used when bootstrap credentials are absent.
///
/// It deliberately mounts only the configuration-status surface and the two probes
/// (`/api/status`, `/ready`, `/healthz`). The full listener's business and admin routes are
/// absent on purpose rather than present-but-denied: setup mode has no Redis and no
/// `CONFIG_ENCRYPTION_KEY`, so `admin_token` is empty and `constant_time_eq` refuses an empty
/// expected value — every admin route would return 401 forever, and `MemoryState` could not
/// persist a bootstrap write anyway. Advertising those routes with 401 read as "retry with a
/// better credential" when the truth is "these endpoints do not exist in this mode"; returning
/// 404 removes that ambiguity for clients, probes and the SPA alike. The SPA bootstrap card is
/// driven by `/api/status` alone (`web/config.js` reveals the admin-session card only when
/// `ready === true`), so nothing the status page renders is lost.
pub fn router_configuration_setup(missing: Vec<String>) -> Router {
    let worker_handle = Arc::new(WorkerHandle::new(Arc::new(NoopWorker)));
    let app_state = AppState {
        auth: AuthState {
            reconcile_token: SecretString::new(String::new()),
            telegram_webhook_secret: SecretString::new(String::new()),
        },
        worker_token: SecretString::new(String::new()),
        state: Arc::new(MemoryState::default()),
        allowlist: Arc::new(HashSet::new()),
        worker: worker_handle.clone(),
        runtime: runtime_provider(OutboundConfig::default()),
        admin_token: SecretString::new(String::new()),
        debug_token: None,
        business_config: Arc::new(RwLock::new(None)),
        business_runtime: Arc::new(RwLock::new(None)),
        business_revision: Arc::new(RwLock::new(0)),
        setup_missing: Arc::new(missing),
        reload: Arc::new(ReloadCoordinator::new(worker_handle, None)),
    };
    Router::new()
        .route("/api/status", get(setup_status))
        .route("/ready", get(ready))
        .route("/healthz", get(healthz))
        .with_state(app_state)
}

pub fn router_with_worker_state<S: ReliableState + 'static>(
    secrets: AuthSecrets,
    worker_token: SecretString,
    state: S,
    allowlist: HashSet<i64>,
    worker_handler: Arc<dyn WorkerHandler>,
) -> Router {
    router_with_worker_state_and_runtime(
        secrets,
        worker_token,
        state,
        allowlist,
        worker_handler,
        runtime_provider(OutboundConfig::default()),
    )
}

pub fn router_with_worker_state_and_runtime<S: ReliableState + 'static>(
    secrets: AuthSecrets,
    worker_token: SecretString,
    state: S,
    allowlist: HashSet<i64>,
    worker_handler: Arc<dyn WorkerHandler>,
    runtime: RuntimeConfigProvider,
) -> Router {
    router_with_worker_state_runtime_bootstrap(
        secrets,
        worker_token,
        state,
        allowlist,
        worker_handler,
        runtime,
        SecretString::new(String::new()),
        None,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "router wiring keeps state dependencies explicit"
)]
pub fn router_with_worker_state_runtime_bootstrap<S: ReliableState + 'static>(
    secrets: AuthSecrets,
    worker_token: SecretString,
    state: S,
    allowlist: HashSet<i64>,
    worker_handler: Arc<dyn WorkerHandler>,
    runtime: RuntimeConfigProvider,
    admin_token: SecretString,
    debug_token: Option<SecretString>,
) -> Router {
    router_with_worker_state_runtime_bootstrap_config(
        secrets,
        worker_token,
        state,
        allowlist,
        worker_handler,
        runtime,
        admin_token,
        debug_token,
        Vec::new(),
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "router wiring keeps state dependencies explicit"
)]
fn router_with_worker_state_runtime_bootstrap_config<S: ReliableState + 'static>(
    secrets: AuthSecrets,
    worker_token: SecretString,
    state: S,
    allowlist: HashSet<i64>,
    worker_handler: Arc<dyn WorkerHandler>,
    runtime: RuntimeConfigProvider,
    admin_token: SecretString,
    debug_token: Option<SecretString>,
    setup_missing: Vec<String>,
) -> Router {
    let worker_handle = Arc::new(WorkerHandle::new(worker_handler));
    let reload = Arc::new(ReloadCoordinator::new(worker_handle.clone(), None));
    let app_state = AppState {
        auth: AuthState::from(secrets),
        worker_token,
        state: Arc::new(state),
        allowlist: Arc::new(allowlist),
        worker: worker_handle,
        runtime,
        admin_token,
        debug_token,
        business_config: Arc::new(RwLock::new(None)),
        business_runtime: Arc::new(RwLock::new(None)),
        business_revision: Arc::new(RwLock::new(0)),
        setup_missing: Arc::new(setup_missing),
        reload,
    };
    let mut router = Router::new()
        .route("/webhook/tg", post(telegram_webhook))
        .route("/push/jmap", post(jmap_push))
        .route("/api/push/register", post(register_push))
        .route("/api/push/disable", post(disable_push))
        .route("/reconcile", post(reconcile))
        .route("/worker", post(worker))
        .route("/api/config", get(get_config).put(put_config))
        .route("/api/enabled", get(get_enabled).put(put_enabled))
        .route(
            "/api/business-config",
            get(get_business_config).put(put_business_config),
        )
        .route(
            "/api/business-config/preflight",
            post(preflight_business_config),
        )
        .route("/api/bootstrap", post(bootstrap))
        .route("/api/admin/session/revoke", post(revoke_admin_session))
        .route("/api/admin/session", post(create_admin_session))
        .route("/healthz", get(healthz))
        .route("/ready", get(ready))
        .route("/api/status", get(setup_status));
    // SAF-DEBUG-GATE: /debug/* only exists when main.rs passed a real DEBUG_TOKEN under --debug.
    if app_state.debug_token.is_some() {
        router = router.merge(crate::debug::debug_router());
    }
    router.with_state(app_state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::MemoryState;
    use axum::{
        body::Body,
        http::{self, Request},
    };
    use tower::ServiceExt;

    fn test_router() -> Router {
        router_with_state(
            AuthSecrets {
                reconcile_token: SecretString::new("reconcile-secret".into()),
                telegram_webhook_secret: SecretString::new("telegram-secret".into()),
            },
            SecretString::new("worker-secret".into()),
            MemoryState::enabled_for_tests(),
            HashSet::new(),
        )
    }

    async fn request(request: Request<Body>) -> StatusCode {
        test_router().oneshot(request).await.unwrap().status()
    }

    async fn configuration_setup_call(
        router: Router,
        method: &str,
        uri: &str,
        authorization: &str,
    ) -> StatusCode {
        router
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("authorization", authorization)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn reconcile_requires_bearer_token() {
        assert_eq!(
            request(
                Request::builder()
                    .method("POST")
                    .uri("/reconcile")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(
                Request::builder()
                    .method("POST")
                    .uri("/reconcile")
                    .header("authorization", "Bearer ")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(
                Request::builder()
                    .method("POST")
                    .uri("/reconcile")
                    .header("authorization", "Bearer wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(
                Request::builder()
                    .method("POST")
                    .uri("/reconcile")
                    .header("authorization", "Bearer reconcile-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn bootstrap_rejects_missing_admin_credential() {
        assert_eq!(
            request(
                Request::builder()
                    .method("POST")
                    .uri("/api/bootstrap")
                    .header("authorization", "Bearer worker-secret")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn configuration_setup_status_lists_only_missing_names() {
        let response = router_configuration_setup(vec!["REDIS_URL".into()])
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["mode"], "configuration-setup");
        assert_eq!(value["missing"][0], "REDIS_URL");
        assert!(!body.windows(4).any(|part| part == b"pass"));
    }

    #[tokio::test]
    async fn configuration_setup_status_lists_both_bootstrap_requirements() {
        let response =
            router_configuration_setup(vec!["REDIS_URL".into(), "CONFIG_ENCRYPTION_KEY".into()])
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri("/api/status")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["missing"].as_array().unwrap().len(), 2);
        assert_eq!(value["missing"][0], "REDIS_URL");
        assert_eq!(value["missing"][1], "CONFIG_ENCRYPTION_KEY");
    }

    #[tokio::test]
    async fn configuration_setup_mounts_only_status_and_probes() {
        let router =
            router_configuration_setup(vec!["REDIS_URL".into(), "CONFIG_ENCRYPTION_KEY".into()]);
        let token = "Bearer CONFIG_ENCRYPTION_KEY-candidate";

        // The status surface and both probes stay reachable: the SPA bootstrap card is driven
        // by /api/status, and liveness/readiness still have to answer.
        assert_eq!(
            configuration_setup_call(router.clone(), "GET", "/healthz", token).await,
            StatusCode::OK
        );
        assert_eq!(
            configuration_setup_call(router.clone(), "GET", "/api/status", token).await,
            StatusCode::OK
        );
        assert_eq!(
            configuration_setup_call(router.clone(), "GET", "/ready", token).await,
            StatusCode::SERVICE_UNAVAILABLE
        );

        // The business and admin surface is absent rather than present-but-denied. An empty
        // CONFIG_ENCRYPTION_KEY makes constant_time_eq reject every candidate forever, so 401
        // here read as "retry the credential" when the truth was "route does not exist".
        for (method, uri) in [
            ("GET", "/api/config"),
            ("GET", "/api/enabled"),
            ("POST", "/api/business-config"),
            ("GET", "/api/business-config"),
            ("POST", "/api/bootstrap"),
            ("POST", "/api/admin/session"),
            ("POST", "/api/admin/session/revoke"),
            ("POST", "/webhook/tg"),
            ("POST", "/push/jmap"),
            ("POST", "/api/push/register"),
            ("POST", "/api/push/disable"),
            ("POST", "/reconcile"),
            ("POST", "/worker"),
        ] {
            assert_eq!(
                configuration_setup_call(router.clone(), method, uri, token).await,
                StatusCode::NOT_FOUND,
                "{method} {uri} must be absent in configuration-setup mode"
            );
        }
    }

    #[tokio::test]
    async fn failed_bootstrap_persists_and_reports_instead_of_silently_failing() {
        let app = router_with_worker_state_runtime_bootstrap(
            AuthSecrets {
                reconcile_token: SecretString::new("r".into()),
                telegram_webhook_secret: SecretString::new("t".into()),
            },
            SecretString::new("w".into()),
            MemoryState::default(),
            HashSet::new(),
            Arc::new(NoopWorker),
            runtime_provider(OutboundConfig::default()),
            SecretString::new("acl-root".into()),
            None,
        );
        // 本用例验证的是"失败的 bootstrap 仍会持久化并如实上报"，失败原因应是
        // JMAP 不可达（127.0.0.1:1 → warning），因此 allowlist 必须合法，避免被
        // SAF-CHAT-ALLOWLIST 的非空校验提前拦成 422。
        let payload = r#"{"bot_token":"bot","telegram_chat_id":1,"chat_allowlist":[1],"telegram_webhook_secret":"hook","jmap_session_url":"https://127.0.0.1:1","jmap_username":"u","jmap_password":"p","account_id":null,"llm_enabled":false,"llm_allow_net":false,"llm_api_key":null,"llm_base_url":null,"llm_model":null,"reconcile_token":"r","worker_token":"w"}"#;
        let persisted = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/bootstrap")
                    .header("authorization", "Bearer acl-root")
                    .body(Body::from(payload))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(persisted.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(persisted.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["persisted"], true);
        assert_eq!(value["runtime_applied"], false);
        let warnings = value["warnings"].as_array().expect("warnings array");
        assert!(
            !warnings.is_empty(),
            "an unreachable JMAP must be reported, not swallowed"
        );
        assert_eq!(warnings[0]["component"], "jmap");
        assert_eq!(warnings[0]["step"], "connect");
        assert!(warnings[0]["detail"].is_string());
        // The one-shot slot is now consumed by the write itself, so a retry cannot silently
        // overwrite a configuration whose runtime never came up.
        let conflicting = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/bootstrap")
                    .header("authorization", "Bearer acl-root")
                    .body(Body::from(payload))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(conflicting.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn telegram_webhook_requires_secret_header() {
        assert_eq!(
            request(
                Request::builder()
                    .method("POST")
                    .uri("/webhook/tg")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(
                Request::builder()
                    .method("POST")
                    .uri("/webhook/tg")
                    .header("x-telegram-bot-api-secret-token", "")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(
                Request::builder()
                    .method("POST")
                    .uri("/webhook/tg")
                    .header("x-telegram-bot-api-secret-token", "wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(
                Request::builder()
                    .method("POST")
                    .uri("/webhook/tg")
                    .header("x-telegram-bot-api-secret-token", "telegram-secret")
                    .body(Body::from(r#"{"update_id":1}"#))
                    .unwrap(),
            )
            .await,
            StatusCode::NO_CONTENT
        );
    }

    #[tokio::test]
    async fn jmap_push_requires_verification_code() {
        assert_eq!(
            request(
                Request::builder()
                    .method("POST")
                    .uri("/push/jmap")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(
                Request::builder()
                    .method("POST")
                    .uri("/push/jmap")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"verificationCode":""}"#))
                    .unwrap(),
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(
                Request::builder()
                    .method("POST")
                    .uri("/push/jmap")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"verificationCode":"wrong"}"#))
                    .unwrap(),
            )
            .await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(
                Request::builder()
                    .method("POST")
                    .uri("/push/jmap")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"pushSubscriptionId":"push-1","verificationCode":"code","accountId":"a","emailId":"e"}"#
                    ))
                    .unwrap(),
            )
            .await,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn worker_requires_bearer_token() {
        let status = request(
            Request::builder()
                .method("POST")
                .uri("/worker")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn disabled_gate_rejects_business_entry_without_side_effects() {
        let app = router_with_state(
            AuthSecrets {
                reconcile_token: SecretString::new("r".into()),
                telegram_webhook_secret: SecretString::new("t".into()),
            },
            SecretString::new("w".into()),
            MemoryState::default(),
            HashSet::new(),
        );
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhook/tg")
                    .header("x-telegram-bot-api-secret-token", "t")
                    .body(Body::from(r#"{"update_id":1}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn enabled_api_persists_toggle() {
        let app = test_router();
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/enabled")
                    .header("authorization", "Bearer worker-secret")
                    .body(Body::from(r#"{"enabled":false}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/enabled")
                    .header("authorization", "Bearer worker-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn outbound_config_api_is_authenticated_and_validated() {
        let app = test_router();
        let unauthorized = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        let invalid = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/config")
                    .header("authorization", "Bearer worker-secret")
                    .body(Body::from(
                        r#"{"jmap_timeout_ms":1,"telegram_timeout_ms":2000,"llm_timeout_ms":3000,"max_retries":2}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let updated = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/config")
                    .header("authorization", "Bearer worker-secret")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"jmap_timeout_ms":2000,"telegram_timeout_ms":2000,"llm_timeout_ms":3000,"max_retries":2}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(updated.status(), StatusCode::OK);
        let fetched = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/config")
                    .header("authorization", "Bearer worker-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(fetched.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn business_config_put_requires_full_validated_wire() {
        let app = test_router();
        let unauthorized = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/business-config")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
        let invalid = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/business-config")
                    .header("authorization", "Bearer worker-secret")
                    .body(Body::from(r#"{"jmapSessionUrl":"http://invalid"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    /// Preflight is a diagnostic endpoint, not a write: it must never persist, never install a
    /// worker, and must never turn a configuration problem into a transport error. "The
    /// configuration is bad" is a successful preflight, because that is the answer the caller
    /// asked for.
    #[tokio::test]
    async fn business_config_preflight_reports_validation_failure_without_writing() {
        let app = test_router();
        let unauthenticated = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/business-config/preflight")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
        // A complete wire so that deserialization succeeds and only the validator rejects
        // it: that is the case preflight has to answer rather than echoing a generic 422.
        let body = r#"{"bot_token":"bot","telegram_chat_id":1,"chat_allowlist":[1],"telegram_webhook_secret":"hook","jmap_session_url":"http://not-https","jmap_username":"u","jmap_password":"p","llm_enabled":false,"llm_allow_net":false,"llm_api_key":null,"llm_base_url":null,"llm_model":null,"reconcile_token":"r","worker_token":"w"}"#;
        let invalid = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/business-config/preflight")
                    .header("authorization", "Bearer worker-secret")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(invalid.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["persisted"], false);
        assert_eq!(value["validation"]["ok"], false);
        assert!(value["validation"]["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|error| { error.as_str().unwrap_or_default().contains("HTTPS") }));
        // Components are null: there is no meaningful way to probe clients from a wire
        // that validation already rejected.
        assert!(value["components"].is_null());
        // "Not written" is proven by the live config snapshot: it must still be the timeout-only
        // default, carrying none of the submitted wire. (/api/bootstrap cannot be used as
        // evidence here — test_router has no admin credential, so it answers 401 unconditionally.)
        assert_preflight_wrote_nothing(app).await;
    }

    /// Router wired for admin calls, plus a handle on the same state so a test can seed or inspect
    /// the stored configuration without another HTTP round trip.
    fn business_config_router() -> (Router, MemoryState) {
        let state = MemoryState::enabled_for_tests();
        let router = router_with_state(
            AuthSecrets {
                reconcile_token: SecretString::new("reconcile-secret".into()),
                telegram_webhook_secret: SecretString::new("telegram-secret".into()),
            },
            SecretString::new("worker-secret".into()),
            state.clone(),
            HashSet::new(),
        );
        (router, state)
    }

    /// A full, valid configuration as it lands on the wire. The JMAP URL is refused immediately,
    /// so a save that reaches `build_worker_report` fails there instead of waiting on a timeout.
    fn stored_business_wire_value() -> serde_json::Value {
        json!({
            "bot_token": "bot-secret-value",
            "telegram_chat_id": -5260770881i64,
            "chat_allowlist": [1, 2],
            "telegram_webhook_secret": "hook-secret-value",
            "jmap_session_url": "https://mail.example.invalid/session",
            "account_id": "account-7",
            "jmap_username": "user@example.invalid",
            "jmap_password": "jmap-secret-value",
            "llm_enabled": false,
            "llm_allow_net": false,
            "llm_api_key": null,
            "llm_base_url": null,
            "llm_model": null,
            "reconcile_token": "reconcile-secret-value",
            "worker_token": "worker-secret-value",
            "timezone": "Etc/UTC",
        })
    }

    async fn seed_business_config(state: &MemoryState, wire: serde_json::Value) {
        state.set_business_config(&wire).await.unwrap();
    }

    /// The stored configuration with a JMAP URL that fails the instant it is dialed, for tests
    /// whose save proceeds far enough to reach `build_worker_report`.
    fn immediate_fail_business_wire_value() -> serde_json::Value {
        let mut wire = stored_business_wire_value();
        wire["jmap_session_url"] = json!("https://127.0.0.1:1");
        wire
    }

    #[tokio::test]
    async fn business_config_get_without_session_returns_401_without_any_field() {
        let (router, _) = business_config_router();
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/business-config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = json_body(response).await;
        assert_eq!(body["error"], json!("unauthorized"));
        // `request_id` is minted per request, so assert its shape rather than its value.
        let request_id = body["request_id"].as_str().unwrap_or("");
        assert_eq!(request_id.len(), 32);
        assert!(request_id.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(body.get("values").is_none());
        assert!(body.get("secrets_present").is_none());
        assert!(body.get("configured").is_none());
        assert_eq!(body.as_object().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn business_config_get_echoes_plaintext_fields_and_secrets_as_presence_only() {
        let (router, state) = business_config_router();
        let mut wire = stored_business_wire_value();
        // Populate the whole LLM block, so its plaintext fields and its secret flag are both
        // exercised; this is the fully valid shape a real save can produce.
        wire["timezone"] = json!("Asia/Tokyo");
        wire["llm_enabled"] = json!(true);
        wire["llm_base_url"] = json!("https://llm.example.invalid/v1");
        wire["llm_model"] = json!("local-model");
        wire["llm_api_key"] = json!("llm-secret-value");
        seed_business_config(&state, wire).await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/business-config")
                    .header(http::header::AUTHORIZATION, "Bearer worker-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["configured"], json!(true));
        assert_eq!(body["revision"], json!(1));

        let values = &body["values"];
        assert_eq!(values["timezone"], json!("Asia/Tokyo"));
        assert_eq!(
            values["jmap_session_url"],
            json!("https://mail.example.invalid/session")
        );
        assert_eq!(values["jmap_username"], json!("user@example.invalid"));
        assert_eq!(values["account_id"], json!("account-7"));
        assert_eq!(values["telegram_chat_id"], json!(-5260770881i64));
        assert_eq!(values["chat_allowlist"], json!([1, 2]));
        assert_eq!(values["llm_enabled"], json!(true));
        assert_eq!(values["llm_allow_net"], json!(false));
        assert_eq!(
            values["llm_base_url"],
            json!("https://llm.example.invalid/v1")
        );
        assert_eq!(values["llm_model"], json!("local-model"));

        assert_eq!(body["secrets_present"]["bot_token"], json!(true));
        assert_eq!(body["secrets_present"]["jmap_password"], json!(true));
        assert_eq!(
            body["secrets_present"]["telegram_webhook_secret"],
            json!(true)
        );
        assert_eq!(body["secrets_present"]["reconcile_token"], json!(true));
        assert_eq!(body["secrets_present"]["worker_token"], json!(true));
        assert_eq!(body["secrets_present"]["llm_api_key"], json!(true));

        // SAF-NO-SECRET-ECHO: presence is allowed, the credential value never is.
        let body_text = serde_json::to_string(&body).unwrap();
        for secret in [
            "bot-secret-value",
            "jmap-secret-value",
            "hook-secret-value",
            "reconcile-secret-value",
            "worker-secret-value",
            "llm-secret-value",
        ] {
            assert!(
                !body_text.contains(secret),
                "{secret} leaked into the response"
            );
        }
    }

    #[tokio::test]
    async fn business_config_get_without_any_stored_config_reports_absent_instead_of_404() {
        let (router, _) = business_config_router();

        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/business-config")
                    .header(http::header::AUTHORIZATION, "Bearer worker-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["configured"], json!(false));
        assert_eq!(body["revision"], json!(0));
        assert_eq!(body["values"]["chat_allowlist"], json!([]));
        assert_eq!(body["values"]["timezone"], json!("Asia/Shanghai"));
        assert_eq!(body["values"]["jmap_session_url"], json!(""));
        assert!(body["values"]["account_id"].is_null());
        assert!(body["values"]["llm_base_url"].is_null());
        assert!(body["values"]["llm_model"].is_null());
        assert_eq!(body["values"]["llm_enabled"], json!(false));
        for key in [
            "bot_token",
            "jmap_password",
            "telegram_webhook_secret",
            "reconcile_token",
            "worker_token",
            "llm_api_key",
        ] {
            assert_eq!(body["secrets_present"][key], json!(false), "{key}");
        }
    }

    #[tokio::test]
    async fn business_config_first_save_rejects_a_patch_that_is_missing_fields() {
        let (router, state) = business_config_router();

        let response = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/business-config")
                    .header(http::header::AUTHORIZATION, "Bearer worker-secret")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_string(&json!({"timezone": "Asia/Tokyo"})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = json_body(response).await;
        assert_eq!(body["error"], json!("invalid_configuration"));
        // The partial submit must not have been recorded as a one-field configuration.
        assert!(state.get_business_config().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn business_config_partial_submit_replaces_only_the_fields_it_carries() {
        let (router, state) = business_config_router();
        seed_business_config(&state, immediate_fail_business_wire_value()).await;

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/business-config")
                    .header(http::header::AUTHORIZATION, "Bearer worker-secret")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_string(&json!({"timezone": "Asia/Tokyo"})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let saved = json_body(response).await;
        assert_eq!(saved["persisted"], json!(true));
        assert_eq!(saved["revision"], json!(2));

        // Read it back over the wire rather than from the saved response: this is what the SPA does.
        let response = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/business-config")
                    .header(http::header::AUTHORIZATION, "Bearer worker-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = json_body(response).await;
        assert_eq!(body["configured"], json!(true));
        assert_eq!(body["revision"], json!(2));

        let values = &body["values"];
        assert_eq!(values["timezone"], json!("Asia/Tokyo"));
        // Every field the patch did not carry must still hold its stored value.
        assert_eq!(values["jmap_session_url"], json!("https://127.0.0.1:1"));
        assert_eq!(values["jmap_username"], json!("user@example.invalid"));
        assert_eq!(values["account_id"], json!("account-7"));
        assert_eq!(values["telegram_chat_id"], json!(-5260770881i64));
        assert_eq!(values["chat_allowlist"], json!([1, 2]));
        assert_eq!(values["llm_enabled"], json!(false));
        assert_eq!(values["llm_allow_net"], json!(false));
        assert!(values["llm_base_url"].is_null());
        assert!(values["llm_model"].is_null());
        for key in [
            "bot_token",
            "jmap_password",
            "telegram_webhook_secret",
            "reconcile_token",
            "worker_token",
        ] {
            assert_eq!(body["secrets_present"][key], json!(true), "{key}");
        }
        assert_eq!(body["secrets_present"]["llm_api_key"], json!(false));
    }

    /// A patch carries the revision the client read before editing. The first write moves it, so a
    /// second patch still carrying the old revision lands nowhere and a stale tab cannot silently
    /// revert a field another tab just saved.
    #[tokio::test]
    async fn business_config_put_rejects_a_patch_carried_by_a_stale_revision() {
        let (router, state) = business_config_router();
        seed_business_config(&state, immediate_fail_business_wire_value()).await;

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/business-config")
                    .header(http::header::AUTHORIZATION, "Bearer worker-secret")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_string(&json!({
                            "timezone": "Asia/Tokyo",
                            "revision": 1,
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let saved = json_body(response).await;
        assert_eq!(saved["revision"], json!(2));

        // The second tab still holds revision 1.
        let response = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/business-config")
                    .header(http::header::AUTHORIZATION, "Bearer worker-secret")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_string(&json!({
                            "timezone": "Etc/UTC",
                            "revision": 1,
                        }))
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert!(response.headers().get(http::header::RETRY_AFTER).is_none());
        let body = json_body(response).await;
        assert_eq!(body["error"], json!("conflict"));

        // Neither the configuration nor the revision moved.
        assert_eq!(state.business_config_revision().await.unwrap(), 2);
        let stored = state.get_business_config().await.unwrap().unwrap();
        assert_eq!(stored["timezone"], json!("Asia/Tokyo"));
        // The control field never becomes a stored setting.
        assert!(stored.get("revision").is_none());
    }

    #[tokio::test]
    async fn business_config_partial_submit_cannot_skip_validation_of_the_merged_result() {
        let (router, state) = business_config_router();
        seed_business_config(&state, stored_business_wire_value()).await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/business-config")
                    .header(http::header::AUTHORIZATION, "Bearer worker-secret")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_string(&json!({"timezone": "Europe/London"})).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = json_body(response).await;
        assert_eq!(body["error"], json!("invalid_configuration"));

        // Rejected submissions leave the stored configuration and its revision untouched.
        let stored = state.get_business_config().await.unwrap().unwrap();
        assert_eq!(stored["timezone"], json!("Etc/UTC"));
        assert_eq!(state.business_config_revision().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn business_config_accepts_a_chat_id_submitted_as_a_number_or_a_string() {
        let (router, state) = business_config_router();
        seed_business_config(&state, immediate_fail_business_wire_value()).await;

        let submitted = json!({"telegram_chat_id": "-987654321"});
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/business-config")
                    .header(http::header::AUTHORIZATION, "Bearer worker-secret")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(serde_json::to_string(&submitted).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_body(response).await["persisted"], json!(true));

        let stored = state.get_business_config().await.unwrap().unwrap();
        assert_eq!(stored["telegram_chat_id"], json!(-987654321i64));

        // A chat id that parses to no integer is still rejected rather than coerced.
        let response = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/business-config")
                    .header(http::header::AUTHORIZATION, "Bearer worker-secret")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_string(&json!({"telegram_chat_id": "not-a-chat-id"}))
                            .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            json_body(response).await["error"],
            json!("invalid_configuration")
        );
        assert_eq!(
            state.get_business_config().await.unwrap().unwrap()["telegram_chat_id"],
            json!(-987654321i64)
        );
    }

    #[tokio::test]
    async fn business_config_put_rejects_a_field_the_patch_type_does_not_declare() {
        let (router, state) = business_config_router();
        seed_business_config(&state, stored_business_wire_value()).await;

        let response = router
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/business-config")
                    .header(http::header::AUTHORIZATION, "Bearer worker-secret")
                    .header(http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_string(&json!({"timezone": "Etc/UTC", "typo_field": 1}))
                            .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            json_body(response).await["error"],
            json!("invalid_configuration")
        );
        assert_eq!(state.business_config_revision().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn business_config_preflight_reports_component_failure_and_writes_nothing() {
        let app = test_router();
        // account_id is deliberately absent: it is optional and defaults to the session
        // primary account, which is the behaviour the SPA now relies on.
        let body = r#"{"bot_token":"bot","telegram_chat_id":1,"chat_allowlist":[1],"telegram_webhook_secret":"hook","jmap_session_url":"https://127.0.0.1:1","jmap_username":"u","jmap_password":"p","llm_enabled":false,"llm_allow_net":false,"llm_api_key":null,"llm_base_url":null,"llm_model":null,"reconcile_token":"r","worker_token":"w"}"#;
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/business-config/preflight")
                    .header("authorization", "Bearer worker-secret")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["persisted"], false);
        assert_eq!(value["validation"]["ok"], true);
        assert_eq!(value["components"]["jmap"]["ok"], false);
        let errors = value["components"]["jmap"]["errors"]
            .as_array()
            .expect("jmap failure list");
        assert_eq!(errors[0]["component"], "jmap");
        assert_eq!(errors[0]["step"], "connect");
        assert!(errors[0]["detail"]
            .as_str()
            .map(|detail| !detail.is_empty())
            .unwrap_or(false));
        // LLM was not requested, so it is absent rather than vacuously "ok".
        assert!(value["components"]["llm"].is_null());
        // "Not written" is proven by the live config snapshot, not by the preflight echo.
        assert_preflight_wrote_nothing(app).await;
    }

    /// Preflight must leave the live configuration untouched. `test_router` has no admin
    /// credential, so `/api/bootstrap` cannot be used as a free-slot probe; the authoritative
    /// evidence is the snapshot the router actually serves, which must still be the timeout-only
    /// default rather than whatever the preflight body proposed.
    async fn assert_preflight_wrote_nothing(app: Router) {
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/config")
                    .header("authorization", "Bearer worker-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(
            value.get("jmap_session_url").is_none(),
            "preflight persisted a JMAP URL: {}",
            value["jmap_session_url"]
        );
        assert!(value.get("telegram_chat_id").is_none());
    }

    // ── /api/push/register 与 /api/push/disable（handoff #51）──
    //
    // 这两条路由此前只有实现、没有 HTTP 层测试。这里用可控 Worker 覆盖
    // 鉴权、入参校验、注册锁冲突、幂等、以及失败时的 retry 语义。

    /// 可控 Worker：`create_id` 为 None 时模拟创建失败；`destroy_ok=false` 模拟销毁失败。
    struct PushMockWorker {
        create_id: Option<&'static str>,
        destroy_ok: bool,
    }

    #[async_trait::async_trait]
    impl WorkerHandler for PushMockWorker {
        async fn process(&self, _stream: &str, _payload: &str) -> Result<(), ()> {
            Err(())
        }

        async fn create_push_subscription(&self, _callback_url: &str) -> Result<String, ()> {
            self.create_id.map(str::to_owned).ok_or(())
        }

        async fn destroy_push_subscription(&self, _subscription_id: &str) -> Result<(), ()> {
            if self.destroy_ok {
                Ok(())
            } else {
                Err(())
            }
        }
    }

    fn push_router(worker: PushMockWorker) -> (Router, MemoryState) {
        let state = MemoryState::enabled_for_tests();
        let router = router_with_worker_state(
            AuthSecrets {
                reconcile_token: SecretString::new("reconcile-secret".into()),
                telegram_webhook_secret: SecretString::new("telegram-secret".into()),
            },
            SecretString::new("worker-secret".into()),
            state.clone(),
            HashSet::new(),
            Arc::new(worker),
        );
        (router, state)
    }

    fn push_req(uri: &str, body: &str, bearer: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(token) = bearer {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        builder.body(Body::from(body.to_owned())).unwrap()
    }

    async fn json_body(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn push_register_requires_bearer_token() {
        let (router, _state) = push_router(PushMockWorker {
            create_id: Some("sub-1"),
            destroy_ok: true,
        });
        let body = r#"{"callback_url":"https://example.com/push"}"#;
        for bearer in [None, Some("wrong-token")] {
            let response = router
                .clone()
                .oneshot(push_req("/api/push/register", body, bearer))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn push_register_rejects_malformed_or_insecure_payload() {
        let (router, _state) = push_router(PushMockWorker {
            create_id: Some("sub-1"),
            destroy_ok: true,
        });
        // 缺字段 / 非 JSON / 非 https / 带 URL 凭据，一律 400 且不触达 Worker。
        let bodies = [
            r#"{}"#,
            r#"not-json"#,
            r#"{"callback_url":"http://insecure.example/push"}"#,
            r#"{"callback_url":"https://user:pw@example.com/push"}"#,
        ];
        for body in bodies {
            let response = router
                .clone()
                .oneshot(push_req("/api/push/register", body, Some("worker-secret")))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "body: {body}");
        }
    }

    #[tokio::test]
    async fn push_register_conflicts_when_registration_lock_is_held() {
        let (router, state) = push_router(PushMockWorker {
            create_id: Some("sub-1"),
            destroy_ok: true,
        });
        let url = "https://example.com/push";
        let lock = format!("lock:push-register:{}", session_digest(url));
        assert!(state
            .acquire_lock(&lock, "another-owner", 360)
            .await
            .unwrap());
        let body = format!(r#"{{"callback_url":"{url}"}}"#);
        let response = router
            .oneshot(push_req("/api/push/register", &body, Some("worker-secret")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn push_register_persists_subscription_then_is_idempotent() {
        let (router, state) = push_router(PushMockWorker {
            create_id: Some("sub-abc"),
            destroy_ok: true,
        });
        let url = "https://example.com/push";
        let body = format!(r#"{{"callback_url":"{url}"}}"#);

        let first = router
            .clone()
            .oneshot(push_req("/api/push/register", &body, Some("worker-secret")))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let first_json = json_body(first).await;
        assert_eq!(first_json["push_subscription_id"], "sub-abc");
        assert!(first_json.get("idempotent").is_none());
        assert_eq!(
            state
                .get_push_subscription_for_callback(url)
                .await
                .unwrap()
                .as_deref(),
            Some("sub-abc")
        );

        // 同一 callback_url 再次注册：命中已存映射，返回 idempotent 标记。
        let second = router
            .oneshot(push_req("/api/push/register", &body, Some("worker-secret")))
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::OK);
        let second_json = json_body(second).await;
        assert_eq!(second_json["push_subscription_id"], "sub-abc");
        assert_eq!(second_json["idempotent"], true);
    }

    #[tokio::test]
    async fn push_register_returns_503_and_releases_lock_when_worker_fails() {
        let (router, state) = push_router(PushMockWorker {
            create_id: None,
            destroy_ok: true,
        });
        let url = "https://example.com/push";
        let body = format!(r#"{{"callback_url":"{url}"}}"#);
        let response = router
            .oneshot(push_req("/api/push/register", &body, Some("worker-secret")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        // 失败路径必须释放注册锁，否则同一 callback 会被 409 永久挡住。
        let lock = format!("lock:push-register:{}", session_digest(url));
        assert!(state.acquire_lock(&lock, "retry-owner", 360).await.unwrap());
    }

    #[tokio::test]
    async fn push_disable_requires_bearer_token_and_uses_error_envelope() {
        let (router, _state) = push_router(PushMockWorker {
            create_id: Some("sub-1"),
            destroy_ok: true,
        });
        let body = r#"{"callback_url":"https://example.com/push"}"#;
        let response = router
            .oneshot(push_req("/api/push/disable", body, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let json = json_body(response).await;
        assert_eq!(json["error"], "unauthorized");
        assert!(json["request_id"].is_string());
    }

    #[tokio::test]
    async fn push_disable_returns_404_for_unregistered_callback() {
        let (router, _state) = push_router(PushMockWorker {
            create_id: Some("sub-1"),
            destroy_ok: true,
        });
        let body = r#"{"callback_url":"https://example.com/unknown"}"#;
        let response = router
            .oneshot(push_req("/api/push/disable", body, Some("worker-secret")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let json = json_body(response).await;
        assert_eq!(json["error"], "push_subscription_not_found");
    }

    #[tokio::test]
    async fn push_disable_removes_registration_on_success() {
        let (router, state) = push_router(PushMockWorker {
            create_id: Some("sub-1"),
            destroy_ok: true,
        });
        let url = "https://example.com/push";
        state
            .remember_push_subscription_for_callback(url, "sub-1")
            .await
            .unwrap();
        // 预先用验证码哈希登记：disable 后该摘要键也应被清除，避免残存 TTL
        // 窗口内同一订阅的 push 仍能通过校验被入队（建议-5b）。
        state
            .remember_push_subscription("sub-1", "verify-code", 300)
            .await
            .unwrap();
        let body = format!(r#"{{"callback_url":"{url}"}}"#);
        let response = router
            .oneshot(push_req("/api/push/disable", &body, Some("worker-secret")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_body(response).await["disabled"], true);
        assert!(state
            .get_push_subscription_for_callback(url)
            .await
            .unwrap()
            .is_none());
        // 摘要键已被清除：同样的验证码不再能通过校验。
        assert!(!state
            .push_subscription_verified("sub-1", "verify-code")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn push_disable_keeps_mapping_and_reports_503_when_destroy_fails() {
        let (router, state) = push_router(PushMockWorker {
            create_id: Some("sub-1"),
            destroy_ok: false,
        });
        let url = "https://example.com/push";
        state
            .remember_push_subscription_for_callback(url, "sub-1")
            .await
            .unwrap();
        let body = format!(r#"{{"callback_url":"{url}"}}"#);
        let response = router
            .oneshot(push_req("/api/push/disable", &body, Some("worker-secret")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let json = json_body(response).await;
        assert_eq!(json["error"], "push_destroy_failed");
        assert!(json["request_id"].is_string());
        // 销毁失败时保留映射，使这次 disable 可重试而不静默丢失订阅。
        assert_eq!(
            state
                .get_push_subscription_for_callback(url)
                .await
                .unwrap()
                .as_deref(),
            Some("sub-1")
        );
    }
}
