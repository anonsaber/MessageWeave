mod ai;
mod channel;
mod config;
pub(crate) mod debug;
pub mod domain;
mod error;
mod notify;
pub mod state;
mod web;
mod worker;

use secrecy::ExposeSecret;
use std::net::SocketAddr;

use crate::channel::telegram::TelegramClient;
use crate::domain::jmap::{client::JmapClientBackend, JmapService};
use config::{encryption_key_from_env, Config};
use notify::{
    router_configuration_setup, router_with_worker_state_runtime_bootstrap, MetadataWorker,
    WorkerHandle, WorkerHandler,
};
use state::{runtime_provider, RedisState};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), error::BotError> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    // reqwest/JMAP and Redis can activate different rustls crypto backends. Install
    // ring explicitly before any client is constructed to avoid rustls provider panic.
    let _ = install_rustls_provider();
    let port = std::env::var("PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(8080_u16);
    let addr: SocketAddr = format!("0.0.0.0:{port}")
        .parse()
        .map_err(|e| error::BotError::Config(format!("invalid PORT: {e}")))?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let mut missing = Vec::new();
    let redis_value = match std::env::var("REDIS_URL") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            missing.push("REDIS_URL".to_owned());
            String::new()
        }
    };
    // The SPA admin credential is this same Secret (see below): capture it once so the
    // validated value and the admin token can never diverge.
    let encryption_key_raw = std::env::var("CONFIG_ENCRYPTION_KEY").unwrap_or_default();
    let encryption_key = match encryption_key_from_env() {
        Ok(key) => key,
        Err(_) => {
            missing.push("CONFIG_ENCRYPTION_KEY".to_owned());
            [0_u8; 32]
        }
    };
    if !missing.is_empty() {
        tracing::warn!(
            ?missing,
            "required bootstrap environment is incomplete; serving setup mode"
        );
        let app = router_configuration_setup(missing).merge(web::router());
        axum::serve(listener, app).await?;
        return Ok(());
    }
    // The Redis URL is the only bootstrap credential in the migration path. Until
    // the bootstrap route is wired, retain the legacy parser solely to obtain it.
    let bootstrap_config = match Config::from_env() {
        Ok(config) => config,
        Err(_) => Config::redis_only(secrecy::SecretString::new(redis_value.clone())),
    };
    let redis_url = bootstrap_config.redis_url.clone();
    // RUN_MODE is intentionally explicit even while both modes share the stage-0
    // no-op router; later stages attach webhook/reconcile side effects here.
    match bootstrap_config.run_mode.as_str() {
        "webhook" => tracing::info!("RUN_MODE=webhook"),
        "reconcile" => tracing::info!("RUN_MODE=reconcile"),
        mode => {
            return Err(error::BotError::Config(format!(
                "RUN_MODE must be webhook or reconcile, got {mode}"
            )))
        }
    }
    tracing::info!(port, "starting webhook HTTP entrypoint");
    let redis =
        RedisState::connect_with_encryption(redis_url.expose_secret(), Some(encryption_key))
            .await?;
    // SPA admin credential == CONFIG_ENCRYPTION_KEY (validated as a required
    // startup Secret above). It must not be derived from REDIS_URL: a Redis ACL
    // password only gates the database connection, and passwordless managed
    // Redis would leave the admin UI permanently locked out.
    let admin_token = secrecy::SecretString::new(encryption_key_raw);
    // SAF-DEBUG-GATE: dual factor — /debug/* is mounted only when the process is launched with
    // --debug AND a non-empty DEBUG_TOKEN Secret exists. Missing either factor keeps the
    // surface absolutely closed (no route registered, so requests get a generic 404).
    let debug_token = if std::env::args().any(|arg| arg == "--debug") {
        std::env::var("DEBUG_TOKEN")
            .ok()
            .filter(|value| !value.is_empty())
            .map(secrecy::SecretString::new)
    } else {
        None
    };
    if debug_token.is_some() {
        // SAF-LOG-PURITY: record that the surface is on, never the token value.
        tracing::warn!("debug endpoints enabled for remote 联调 (SAF-DEBUG-GATE)");
    }
    let worker_state: std::sync::Arc<dyn state::ReliableState> = std::sync::Arc::new(
        RedisState::connect_with_encryption(redis_url.expose_secret(), Some(encryption_key))
            .await?,
    );
    let config = match worker_state
        .get_business_config()
        .await
        .map_err(|_| error::BotError::Config("business config unavailable".into()))?
    {
        Some(value) => Config::from_business_json(redis_url.clone(), value)?,
        None => bootstrap_config,
    };
    let outbound = worker_state
        .get_outbound_config()
        .await
        .map_err(|_| error::BotError::Config("runtime outbound config unavailable".into()))?;
    let runtime = runtime_provider(outbound.clone());
    let llm = if config.llm.enabled && config.llm.allow_net {
        Some(std::sync::Arc::new(
            ai::LlmClient::with_runtime(
                config.llm.base_url.clone().unwrap_or_default(),
                config
                    .llm
                    .api_key
                    .clone()
                    .unwrap_or_else(|| secrecy::SecretString::new("".into())),
                config.llm.model.clone().unwrap_or_default(),
                config.llm.summary_target_chars,
                runtime.clone(),
            )
            .map_err(|_| error::BotError::Config("LLM endpoint must use HTTPS".into()))?,
        ))
    } else {
        None
    };
    let allowlist: std::collections::HashSet<i64> =
        config.telegram.chat_allowlist.iter().copied().collect();
    let chat_id = config.telegram.chat_id;
    let worker: Arc<dyn WorkerHandler> = if config.telegram.bot_token.expose_secret().is_empty() {
        Arc::new(notify::NoopWorker)
    } else {
        match JmapClientBackend::connect_with_runtime(
            &config.jmap.session_url,
            &config.jmap.username,
            config.jmap.app_password.expose_secret(),
            config.account_id.as_deref(),
            runtime.clone(),
            // All subsequent JMAP calls read this provider, so SPA updates apply
            // without restarting the process.
        )
        .await
        {
            Ok(backend) => {
                let account = backend.account_id().to_owned();
                match JmapService::new(backend, account) {
                    Ok(jmap) => Arc::new(MetadataWorker::new(
                        jmap,
                        TelegramClient::with_runtime(config.telegram.bot_token, runtime.clone()),
                        chat_id,
                        worker_state,
                        llm,
                    )) as Arc<dyn WorkerHandler>,
                    Err(_) => {
                        tracing::warn!(
                            "JMAP service unavailable; starting configuration-only mode"
                        );
                        Arc::new(notify::NoopWorker)
                    }
                }
            }
            Err(_) => {
                tracing::warn!("JMAP connection unavailable; starting configuration-only mode");
                Arc::new(notify::NoopWorker)
            }
        }
    };
    let app = router_with_worker_state_runtime_bootstrap(
        config.auth,
        config.worker_token,
        redis,
        allowlist,
        Arc::new(WorkerHandle::new(worker)),
        runtime,
        admin_token,
        debug_token,
    )
    .merge(web::router());
    axum::serve(listener, app).await?;
    Ok(())
}

fn install_rustls_provider() -> bool {
    rustls::crypto::ring::default_provider()
        .install_default()
        .is_ok()
        || rustls::crypto::CryptoProvider::get_default().is_some()
}

#[cfg(test)]
mod tests {
    use super::install_rustls_provider;

    #[test]
    fn rustls_provider_is_installable_without_network() {
        assert!(install_rustls_provider());
    }
}
