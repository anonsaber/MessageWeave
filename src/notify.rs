use axum::{
    body::Bytes,
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Router,
};
use ring::rand::SecureRandom;
use secrecy::{ExposeSecret, SecretString};
use std::{
    collections::HashSet,
    sync::{Arc, RwLock},
};
use subtle::ConstantTimeEq;

use crate::ai::LlmClient;
use crate::channel::telegram::TelegramClient;
use crate::config::{
    session_digest, validate_business_wire, AuthSecrets, BusinessConfig, BusinessConfigWire,
};
use crate::domain::jmap::{client::JmapClientBackend, JmapService};
use crate::state::MemoryState;
use crate::state::{runtime_provider, OutboundConfig, ReliableState, RuntimeConfigProvider};
pub use crate::worker::{
    MetadataWorker, NoopWorker, ReloadCoordinator, WorkerHandle, WorkerHandler,
};

#[derive(Clone)]
struct AuthState {
    reconcile_token: SecretString,
    telegram_webhook_secret: SecretString,
    jmap_push_verification: SecretString,
}

#[derive(Clone)]
struct AppState {
    auth: AuthState,
    worker_token: SecretString,
    state: Arc<dyn ReliableState>,
    allowlist: Arc<HashSet<i64>>,
    worker: Arc<dyn WorkerHandler>,
    runtime: RuntimeConfigProvider,
    bootstrap_token: SecretString,
    business_config: Arc<RwLock<Option<serde_json::Value>>>,
    business_runtime: Arc<RwLock<Option<BusinessConfig>>>,
    business_revision: Arc<RwLock<u64>>,
    setup_missing: Arc<Vec<String>>,
    reload: Arc<ReloadCoordinator>,
}

impl From<AuthSecrets> for AuthState {
    fn from(value: AuthSecrets) -> Self {
        Self {
            reconcile_token: value.reconcile_token,
            telegram_webhook_secret: value.telegram_webhook_secret,
            jmap_push_verification: value.jmap_push_verification,
        }
    }
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn ready() -> impl IntoResponse {
    (StatusCode::OK, "ready")
}

async fn setup_status(State(app): State<AppState>) -> Response {
    let ready = app.setup_missing.is_empty();
    axum::Json(serde_json::json!({
        "ready": ready,
        "mode": if ready { "configured" } else { "configuration-setup" },
        "missing": app.setup_missing.as_ref(),
    }))
    .into_response()
}

async fn telegram_webhook(
    State(app): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    refresh_business_config(&app).await;
    let (auth, _, allowlist) = auth_snapshot(&app);
    if !header_value_matches(
        &headers,
        "x-telegram-bot-api-secret-token",
        auth.telegram_webhook_secret.expose_secret(),
    ) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(update) = serde_json::from_slice::<TelegramUpdate>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if !allowlist.is_empty() && update.chat_id().is_some_and(|id| !allowlist.contains(&id)) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let key = format!("dedup:tg:{}", update.update_id);
    match app.state.claim_dedup(&key, 86_400).await {
        Ok(false) => StatusCode::NO_CONTENT.into_response(),
        Ok(true) => match app
            .state
            .enqueue("stalwart:telegram", &String::from_utf8_lossy(&body))
            .await
        {
            Ok(_) => StatusCode::NO_CONTENT.into_response(),
            Err(_) => {
                let _ = app.state.release_dedup(&key).await;
                StatusCode::SERVICE_UNAVAILABLE.into_response()
            }
        },
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn reconcile(State(app): State<AppState>, headers: HeaderMap) -> Response {
    refresh_business_config(&app).await;
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
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match app.state.acquire_lock("lock:reconcile", 300).await {
        Ok(true) => {
            // JMAP changes/reconciliation is intentionally not implemented in
            // this bounded HTTP slice; do not report a false successful run.
            let _ = app.state.release_lock("lock:reconcile").await;
            (StatusCode::NOT_IMPLEMENTED, "reconcile changes unavailable").into_response()
        }
        Ok(false) => StatusCode::CONFLICT.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn worker(State(app): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    refresh_business_config(&app).await;
    let (_, worker_token, _) = auth_snapshot(&app);
    let valid = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(|token| constant_time_eq(token.as_bytes(), worker_token.expose_secret().as_bytes()))
        .unwrap_or(false);
    if !valid {
        StatusCode::UNAUTHORIZED.into_response()
    } else {
        const GROUP: &str = "stalwart-workers";
        const CONSUMER: &str = "http-worker";
        const MAX: usize = 10;
        let batch = if body.is_empty() {
            MAX
        } else {
            let Ok(request) = serde_json::from_slice::<WorkerRequest>(&body) else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            request
                .batch
                .filter(|count| *count > 0)
                .map_or(MAX, |count| count.min(MAX))
        };
        for stream in ["stalwart:jmap", "stalwart:telegram"] {
            let messages = match app.state.read_batch(stream, GROUP, CONSUMER, batch).await {
                Ok(messages) => messages,
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            };
            for message in messages.into_iter().take(batch) {
                // Commit is written only after send succeeds. Before send,
                // use a short in-flight lease so a crash cannot permanently
                // suppress an XAUTOCLAIM retry.
                let delivery_key = format!("delivery:committed:{stream}:{}", message.id);
                match app.state.dedup_exists(&delivery_key).await {
                    Ok(true) => {
                        if app.state.ack(stream, GROUP, &message.id).await.is_err() {
                            return StatusCode::SERVICE_UNAVAILABLE.into_response();
                        }
                        continue;
                    }
                    Ok(false) => {}
                    Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                }
                let inflight_key = format!("delivery:inflight:{stream}:{}", message.id);
                match app.state.claim_dedup(&inflight_key, 60).await {
                    Ok(false) => continue,
                    Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                    Ok(true) => {}
                }
                if app.worker.process(stream, &message.payload).await.is_ok() {
                    if app.state.claim_dedup(&delivery_key, 604_800).await.is_err() {
                        let _ = app.state.release_dedup(&inflight_key).await;
                        return StatusCode::SERVICE_UNAVAILABLE.into_response();
                    }
                    let _ = app.state.release_dedup(&inflight_key).await;
                    if app.state.ack(stream, GROUP, &message.id).await.is_err() {
                        return StatusCode::SERVICE_UNAVAILABLE.into_response();
                    }
                } else {
                    let _ = app.state.release_dedup(&inflight_key).await;
                    if app
                        .state
                        .retry_or_dlq(stream, &format!("{stream}:dlq"), GROUP, &message, 3)
                        .await
                        .is_err()
                    {
                        return StatusCode::SERVICE_UNAVAILABLE.into_response();
                    }
                }
            }
        }
        StatusCode::NO_CONTENT.into_response()
    }
}

fn worker_authorized(headers: &HeaderMap, token: &str) -> bool {
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
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match app.state.get_outbound_config().await {
        Ok(config) => axum::Json(config).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn put_config(State(app): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    refresh_business_config(&app).await;
    if !config_authorized(&app, &headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(config) = serde_json::from_slice::<OutboundConfig>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if !(100..=300_000).contains(&config.jmap_timeout_ms)
        || !(100..=300_000).contains(&config.telegram_timeout_ms)
        || !(100..=300_000).contains(&config.llm_timeout_ms)
        || config.max_retries > 5
    {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    }
    match app.state.set_outbound_config(&config).await {
        Ok(()) => {
            if let Ok(mut current) = app.runtime.write() {
                *current = config.clone();
            }
            axum::Json(config).into_response()
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

/// Replace the complete encrypted business configuration. Client construction is performed
/// before persistence and swap, so malformed/unreachable settings leave the old worker live.
async fn put_business_config(
    State(app): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    refresh_business_config(&app).await;
    if !config_authorized(&app, &headers).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Ok(wire) = serde_json::from_value::<BusinessConfigWire>(value.clone()) else {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    };
    if validate_business_wire(wire.clone()).is_err() {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    }
    let Ok(config): Result<BusinessConfig, _> = wire.try_into() else {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    };
    let Ok(worker) = build_worker(config.clone(), app.clone()).await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if app.state.set_business_config(&value).await.is_err() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if app.reload.commit(config.clone(), worker).is_err() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if let Ok(mut current) = app.business_config.write() {
        *current = Some(value);
    }
    if let Ok(mut current) = app.business_runtime.write() {
        *current = Some(config);
    }
    if let Ok(revision) = app.state.business_config_revision().await {
        if let Ok(mut current) = app.business_revision.write() {
            *current = revision;
        }
    }
    StatusCode::NO_CONTENT.into_response()
}

/// One-shot Redis ACL bootstrap. The ACL password is only compared in constant time and is
/// never included in the response; SET-NX in ReliableState closes the initialization race.
async fn bootstrap(State(app): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if app.bootstrap_token.expose_secret().is_empty()
        || !worker_authorized(&headers, app.bootstrap_token.expose_secret())
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(config) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Ok(wire) = serde_json::from_value::<BusinessConfigWire>(config.clone()) else {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    };
    if validate_business_wire(wire.clone()).is_err() {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    }
    let Ok(next_config): Result<BusinessConfig, _> = wire.try_into() else {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    };
    // Build and connect all clients before SET-NX: an unreachable JMAP must not consume
    // the one-shot bootstrap slot or leave an unusable encrypted snapshot in Redis.
    let Ok(next_worker) = build_worker(next_config.clone(), app.clone()).await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match app.state.initialize_business_config(&config).await {
        Ok(true) => {
            if app.reload.commit(next_config.clone(), next_worker).is_err() {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            if let Ok(mut current) = app.business_config.write() {
                *current = Some(config);
            }
            if let Ok(mut current) = app.business_runtime.write() {
                *current = Some(next_config);
            }
            if let Ok(revision) = app.state.business_config_revision().await {
                if let Ok(mut current) = app.business_revision.write() {
                    *current = revision;
                }
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => StatusCode::CONFLICT.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn build_worker(
    config: crate::config::BusinessConfig,
    app: AppState,
) -> Result<Arc<dyn WorkerHandler>, ()> {
    let backend = JmapClientBackend::connect_with_runtime(
        &config.jmap_session_url,
        &config.jmap_username,
        config.jmap_password.expose_secret(),
        config.account_id.as_deref(),
        app.runtime.clone(),
    )
    .await
    .map_err(|_| ())?;
    let account = backend.account_id().to_owned();
    let jmap = JmapService::new(backend, account).map_err(|_| ())?;
    let telegram = TelegramClient::with_runtime(config.bot_token, app.runtime.clone());
    let llm = if config.llm_enabled && config.llm_allow_net {
        let key = config.llm_api_key.ok_or(())?;
        Some(Arc::new(
            LlmClient::with_runtime(
                config.llm_base_url.ok_or(())?,
                key,
                config.llm_model.ok_or(())?,
                300,
                app.runtime,
            )
            .map_err(|_| ())?,
        ))
    } else {
        None
    };
    Ok(Arc::new(MetadataWorker::new(
        jmap,
        telegram,
        config.telegram_chat_id,
        app.state,
        llm,
    )))
}

/// Request-boundary refresh for multi-instance deployments. A failed remote rebuild advances the
/// observed revision to avoid a hot retry loop while retaining the active worker/config.
async fn refresh_business_config(app: &AppState) {
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
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let digest = session_digest(token);
    match app.state.revoke_admin_session(&digest).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn create_admin_session(State(app): State<AppState>, headers: HeaderMap) -> Response {
    if app.bootstrap_token.expose_secret().is_empty()
        || !worker_authorized(&headers, app.bootstrap_token.expose_secret())
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut bytes = [0_u8; 32];
    if ring::rand::SystemRandom::new().fill(&mut bytes).is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let digest = session_digest(&token);
    if app.state.put_admin_session(&digest, 900).await.is_err() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    axum::Json(serde_json::json!({"session": token, "expires_in": 900})).into_response()
}

#[derive(serde::Deserialize)]
struct WorkerRequest {
    batch: Option<usize>,
}

async fn jmap_push(State(app): State<AppState>, body: Bytes) -> Response {
    refresh_business_config(&app).await;
    let (auth, _, _) = auth_snapshot(&app);
    // Stalwart's PushSubscription callback carries verificationCode in its
    // JSON object. Check it before any future queue/Redis side effect.
    let valid = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .get("verificationCode")
                .and_then(|code| code.as_str())
                .map(str::to_owned)
        })
        .map(|code| {
            constant_time_eq(
                code.as_bytes(),
                auth.jmap_push_verification.expose_secret().as_bytes(),
            )
        })
        .unwrap_or(false);
    if !valid {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(push) = serde_json::from_slice::<JmapPush>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let (Some(account), Some(email)) = (push.account_id, push.email_id) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let key = format!("dedup:jmap:{account}:{email}");
    match app.state.claim_dedup(&key, 86_400).await {
        Ok(false) => StatusCode::NO_CONTENT.into_response(),
        Ok(true) => match app
            .state
            .enqueue("stalwart:jmap", &String::from_utf8_lossy(&body))
            .await
        {
            Ok(_) => StatusCode::NO_CONTENT.into_response(),
            Err(_) => {
                let _ = app.state.release_dedup(&key).await;
                StatusCode::SERVICE_UNAVAILABLE.into_response()
            }
        },
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
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

fn constant_time_eq(actual: &[u8], expected: &[u8]) -> bool {
    !expected.is_empty() && actual.len() == expected.len() && actual.ct_eq(expected).into()
}

fn auth_snapshot(app: &AppState) -> (AuthState, SecretString, HashSet<i64>) {
    if let Ok(snapshot) = app.business_runtime.read() {
        if let Some(config) = snapshot.as_ref() {
            return (
                AuthState {
                    reconcile_token: config.reconcile_token.clone(),
                    telegram_webhook_secret: config.telegram_webhook_secret.clone(),
                    jmap_push_verification: config.jmap_push_verification.clone(),
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

/// Configuration-only listener used when bootstrap credentials are absent. MemoryState is
/// deliberately not advertised as persistence: all business writes remain unavailable until
/// Redis is configured.
pub fn router_configuration_setup(missing: Vec<String>) -> Router {
    router_with_worker_state_runtime_bootstrap_config(
        AuthSecrets {
            reconcile_token: SecretString::new(String::new()),
            telegram_webhook_secret: SecretString::new(String::new()),
            jmap_push_verification: SecretString::new(String::new()),
        },
        SecretString::new(String::new()),
        MemoryState::default(),
        HashSet::new(),
        Arc::new(NoopWorker),
        runtime_provider(OutboundConfig::default()),
        SecretString::new(String::new()),
        missing,
    )
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
    )
}

pub fn router_with_worker_state_runtime_bootstrap<S: ReliableState + 'static>(
    secrets: AuthSecrets,
    worker_token: SecretString,
    state: S,
    allowlist: HashSet<i64>,
    worker_handler: Arc<dyn WorkerHandler>,
    runtime: RuntimeConfigProvider,
    bootstrap_token: SecretString,
) -> Router {
    router_with_worker_state_runtime_bootstrap_config(
        secrets,
        worker_token,
        state,
        allowlist,
        worker_handler,
        runtime,
        bootstrap_token,
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
    bootstrap_token: SecretString,
    setup_missing: Vec<String>,
) -> Router {
    let worker_handle = Arc::new(WorkerHandle::new(worker_handler));
    let reload = Arc::new(ReloadCoordinator::new(worker_handle.clone(), None));
    Router::new()
        .route("/webhook/tg", post(telegram_webhook))
        .route("/push/jmap", post(jmap_push))
        .route("/reconcile", post(reconcile))
        .route("/worker", post(worker))
        .route("/api/config", get(get_config).put(put_config))
        .route("/api/business-config", put(put_business_config))
        .route("/api/bootstrap", post(bootstrap))
        .route("/api/admin/session/revoke", post(revoke_admin_session))
        .route("/api/admin/session", post(create_admin_session))
        .route("/healthz", get(healthz))
        .route("/ready", get(ready))
        .route("/api/status", get(setup_status))
        .with_state(AppState {
            auth: AuthState::from(secrets),
            worker_token,
            state: Arc::new(state),
            allowlist: Arc::new(allowlist),
            worker: worker_handle,
            runtime,
            bootstrap_token,
            business_config: Arc::new(RwLock::new(None)),
            business_runtime: Arc::new(RwLock::new(None)),
            business_revision: Arc::new(RwLock::new(0)),
            setup_missing: Arc::new(setup_missing),
            reload,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::MemoryState;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    fn test_router() -> Router {
        router_with_state(
            AuthSecrets {
                reconcile_token: SecretString::new("reconcile-secret".into()),
                telegram_webhook_secret: SecretString::new("telegram-secret".into()),
                jmap_push_verification: SecretString::new("jmap-verification".into()),
            },
            SecretString::new("worker-secret".into()),
            MemoryState::default(),
            HashSet::new(),
        )
    }

    async fn request(request: Request<Body>) -> StatusCode {
        test_router().oneshot(request).await.unwrap().status()
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
            StatusCode::NOT_IMPLEMENTED
        );
    }

    #[tokio::test]
    async fn bootstrap_rejects_without_acl_bootstrap_credential() {
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
    async fn failed_bootstrap_does_not_consume_initialization_slot() {
        let app = router_with_worker_state_runtime_bootstrap(
            AuthSecrets {
                reconcile_token: SecretString::new("r".into()),
                telegram_webhook_secret: SecretString::new("t".into()),
                jmap_push_verification: SecretString::new("p".into()),
            },
            SecretString::new("w".into()),
            MemoryState::default(),
            HashSet::new(),
            Arc::new(NoopWorker),
            runtime_provider(OutboundConfig::default()),
            SecretString::new("acl-root".into()),
        );
        let body = r#"{"bot_token":"bot","telegram_chat_id":1,"chat_allowlist":[],"telegram_webhook_secret":"hook","jmap_session_url":"https://127.0.0.1:1","jmap_username":"u","jmap_password":"p","jmap_push_verification":"v","account_id":null,"llm_enabled":false,"llm_allow_net":false,"llm_api_key":null,"llm_base_url":null,"llm_model":null,"reconcile_token":"r","worker_token":"w"}"#;
        for _ in 0..2 {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/bootstrap")
                        .header("authorization", "Bearer acl-root")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        }
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
                        r#"{"verificationCode":"jmap-verification","accountId":"a","emailId":"e"}"#
                    ))
                    .unwrap(),
            )
            .await,
            StatusCode::NO_CONTENT
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
}
