//! Redis-only reliability primitives (C-REDIS-ONLY-STATE, MOD-DEDUP, MOD-STREAMS).
//! These APIs are deliberately short-request operations; no subscriptions or polling.

use crate::config::BusinessConfig;
use async_trait::async_trait;
use ring::{
    aead,
    rand::{SecureRandom, SystemRandom},
};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, RwLock},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StateError {
    #[error("redis operation failed")]
    Redis(#[from] redis::RedisError),
    #[error("state lock poisoned")]
    Poisoned,
    #[error("configuration encryption failed")]
    Encryption,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamMessage {
    pub id: String,
    pub payload: String,
}

/// Non-secret outbound tuning persisted in Redis for SPA/API management.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
pub struct OutboundConfig {
    pub jmap_timeout_ms: u64,
    pub telegram_timeout_ms: u64,
    pub llm_timeout_ms: u64,
    pub max_retries: u8,
}

pub type RuntimeConfigProvider = Arc<RwLock<OutboundConfig>>;
pub type RuntimeBusinessConfigProvider = Arc<RwLock<Option<BusinessConfig>>>;

pub fn runtime_provider(config: OutboundConfig) -> RuntimeConfigProvider {
    Arc::new(RwLock::new(config))
}

pub fn business_runtime_provider(config: Option<BusinessConfig>) -> RuntimeBusinessConfigProvider {
    Arc::new(RwLock::new(config))
}

impl Default for OutboundConfig {
    fn default() -> Self {
        Self {
            jmap_timeout_ms: 15_000,
            telegram_timeout_ms: 10_000,
            llm_timeout_ms: 30_000,
            max_retries: 3,
        }
    }
}

/// Legacy single-event ceiling. It is the floor the derived ceiling must clear
/// and the fallback when the outbound config cannot be read.
const SINGLE_EVENT_CEILING_FLOOR_MS: u64 = 300_000;

/// Safety factor on the derived ceiling. Retries insert sleeps between
/// attempts, and the ceiling bounds a window that must not be exceeded while
/// the owning consumer is still alive.
const SINGLE_EVENT_CEILING_SAFETY: u64 = 2;

/// Upper bound on the XAUTOCLAIM idle window: a misconfigured timeout must not
/// push multi-instance recovery out by days. Six hours keeps a crashed batch
/// recoverable within the same working day.
const RECLAIM_IDLE_THRESHOLD_CAP_MS: u64 = 6 * 60 * 60 * 1000;

/// Worst-case wall clock for one event: every outbound call can burn its full
/// timeout once per attempt and `send_text` retries up to `max_retries` more
/// times, so the ceiling is
/// `(max_retries + 1) * (jmap + telegram + llm) * SAFETY`, floored at the
/// legacy constant so a zeroed-out config cannot shrink the window.
fn single_event_ceiling_ms(config: &OutboundConfig) -> u64 {
    let attempts = config.max_retries as u64 + 1;
    let per_attempt = config
        .jmap_timeout_ms
        .saturating_add(config.telegram_timeout_ms)
        .saturating_add(config.llm_timeout_ms);
    per_attempt
        .saturating_mul(attempts)
        .saturating_mul(SINGLE_EVENT_CEILING_SAFETY)
        .max(SINGLE_EVENT_CEILING_FLOOR_MS)
}

/// Idle window handed to `XAUTOCLAIM`: the worst-case *batch* duration, since a
/// worker processes up to `count` events sequentially. Floored through
/// `single_event_ceiling_ms` and capped so a bad config cannot stretch
/// recovery into days. Only duplicate-on-multi-instance is at stake, never loss.
fn reclaim_idle_threshold_ms(config: &OutboundConfig, count: usize) -> u64 {
    (count.max(1) as u64)
        .saturating_mul(single_event_ceiling_ms(config))
        .min(RECLAIM_IDLE_THRESHOLD_CAP_MS)
}

#[async_trait]
pub trait ReliableState: Send + Sync {
    /// Atomic SET NX EX claim for Telegram update or `(account,email)` dedup key.
    async fn claim_dedup(&self, key: &str, ttl_seconds: u64) -> Result<bool, StateError>;
    /// Checks a durable idempotency commit without creating one.
    async fn dedup_exists(&self, key: &str) -> Result<bool, StateError>;
    /// Roll back a dedup claim when the following enqueue operation fails.
    async fn release_dedup(&self, key: &str) -> Result<(), StateError>;
    /// XADD enqueue; consumers perform XREADGROUP in a later worker phase.
    async fn enqueue(&self, stream: &str, payload: &str) -> Result<String, StateError>;
    /// Atomically claim `dedup_key` and XADD `payload` to `stream`.
    ///
    /// The dedup key must only be written when the append succeeds: claiming
    /// first and rolling back best-effort leaves a claimed-but-not-enqueued key
    /// behind on a Redis hiccup, which silently drops the event for the whole
    /// dedup TTL (REQ-RECONCILE-IDEMPOTENCY). Returns false when already claimed.
    async fn claim_dedup_and_enqueue(
        &self,
        dedup_key: &str,
        ttl_seconds: u64,
        stream: &str,
        payload: &str,
    ) -> Result<bool, StateError>;
    /// XACK after successful processing.
    async fn ack(&self, stream: &str, group: &str, message_id: &str) -> Result<(), StateError>;
    /// XADD to a dead-letter stream after bounded retry policy decides to stop retrying.
    async fn dead_letter(&self, stream: &str, payload: &str) -> Result<String, StateError>;
    /// SET NX EX lock for reconcile single-flight.
    async fn acquire_lock(
        &self,
        key: &str,
        owner_token: &str,
        ttl_seconds: u64,
    ) -> Result<bool, StateError>;
    /// Release a lock after the bounded reconcile attempt has completed.
    async fn release_lock(&self, key: &str, owner_token: &str) -> Result<(), StateError>;
    /// Extend a lock only while the caller still owns it.
    async fn renew_lock(
        &self,
        key: &str,
        owner_token: &str,
        ttl_seconds: u64,
    ) -> Result<bool, StateError>;
    async fn read_batch(
        &self,
        stream: &str,
        group: &str,
        consumer: &str,
        count: usize,
    ) -> Result<Vec<StreamMessage>, StateError>;
    async fn retry_or_dlq(
        &self,
        stream: &str,
        dlq: &str,
        group: &str,
        message: &StreamMessage,
        max_attempts: u32,
    ) -> Result<bool, StateError>;
    async fn set_ai_consent(&self, chat_id: i64, ttl_seconds: u64) -> Result<(), StateError>;
    async fn ai_consent_until(&self, chat_id: i64) -> Result<Option<i64>, StateError>;
    async fn clear_ai_consent(&self, chat_id: i64) -> Result<(), StateError>;
    async fn get_outbound_config(&self) -> Result<OutboundConfig, StateError>;
    async fn set_outbound_config(&self, config: &OutboundConfig) -> Result<(), StateError>;
    /// Atomically read the encrypted-at-rest business configuration namespace.
    /// Callers must redact secrets before returning it to HTTP clients.
    async fn get_business_config(&self) -> Result<Option<serde_json::Value>, StateError>;
    /// Replace the business configuration namespace; bootstrap callers must enforce
    /// SET-NX semantics before invoking this operation (C-REDIS-ONLY-STATE).
    async fn set_business_config(&self, config: &serde_json::Value) -> Result<(), StateError>;
    /// Atomically initialize the business namespace; returns false when already set.
    async fn initialize_business_config(
        &self,
        config: &serde_json::Value,
    ) -> Result<bool, StateError>;
    /// Stores only the opaque session digest, never the bearer token itself.
    async fn put_admin_session(&self, digest: &str, ttl_seconds: u64) -> Result<(), StateError>;
    async fn admin_session_valid(&self, digest: &str) -> Result<bool, StateError>;
    async fn revoke_admin_session(&self, digest: &str) -> Result<(), StateError>;
    async fn business_config_revision(&self) -> Result<u64, StateError>;
    /// Global durable enable gate; missing/read failures are interpreted by callers as disabled.
    async fn is_enabled(&self) -> Result<bool, StateError>;
    async fn set_enabled(&self, enabled: bool) -> Result<(), StateError>;
    /// Durable JMAP Email/changes cursor. It is advanced only after all page
    /// events have been enqueued successfully (REQ-RECONCILE-IDEMPOTENCY).
    async fn get_reconcile_state(&self) -> Result<Option<String>, StateError>;
    async fn set_reconcile_state(&self, state: &str) -> Result<(), StateError>;
    /// Short-lived PushSubscription verification state; values are stored as
    /// digests and never retained as plaintext credentials.
    async fn remember_push_subscription(
        &self,
        subscription_id: &str,
        verification_code: &str,
        ttl_seconds: u64,
    ) -> Result<(), StateError>;
    async fn push_subscription_verified(
        &self,
        subscription_id: &str,
        verification_code: &str,
    ) -> Result<bool, StateError>;
    async fn remember_push_subscription_id(&self, subscription_id: &str) -> Result<(), StateError>;
    async fn set_push_subscription_status(
        &self,
        subscription_id: &str,
        status: &str,
        ttl_seconds: u64,
    ) -> Result<(), StateError>;
    // NOTE: `push:subscription:{id}:status` is currently write-only
    // ("pending"/"verified"/"disabled"). Authorization uses the verification
    // digest instead; the status key stays as a lightweight ops/observability
    // trail (inspectable via redis-cli), not a gate (建议-5a).
    /// Drop the verification-code digest for `subscription_id`.
    ///
    /// Callers must invoke this when disabling a subscription: otherwise a push
    /// arriving within the residual TTL of the last verify call can still pass
    /// `push_subscription_verified` and be enqueued (SAF-AUTH-JMAP-PUSH).
    async fn forget_push_subscription(&self, subscription_id: &str) -> Result<(), StateError>;
    async fn record_push_orphan(
        &self,
        subscription_id: &str,
        request_id: &str,
    ) -> Result<(), StateError>;
    async fn get_push_subscription_for_callback(
        &self,
        callback_url: &str,
    ) -> Result<Option<String>, StateError>;
    async fn remember_push_subscription_for_callback(
        &self,
        callback_url: &str,
        subscription_id: &str,
    ) -> Result<(), StateError>;
    async fn remove_push_subscription_for_callback(
        &self,
        callback_url: &str,
    ) -> Result<(), StateError>;
}

pub struct RedisState {
    connection: redis::aio::MultiplexedConnection,
    encryption_key: Option<[u8; 32]>,
}

impl RedisState {
    /// Connects to externally managed REDIS_URL; URL/password are never logged.
    pub async fn connect(redis_url: &str) -> Result<Self, StateError> {
        Self::connect_with_encryption(redis_url, None).await
    }

    pub async fn connect_with_encryption(
        redis_url: &str,
        encryption_key: Option<[u8; 32]>,
    ) -> Result<Self, StateError> {
        let client = redis::Client::open(redis_url)?;
        Ok(Self {
            connection: client.get_multiplexed_async_connection().await?,
            encryption_key,
        })
    }
}

#[async_trait]
impl ReliableState for RedisState {
    async fn claim_dedup(&self, key: &str, ttl_seconds: u64) -> Result<bool, StateError> {
        set_nx_ex(self.connection.clone(), key, ttl_seconds).await
    }

    async fn release_dedup(&self, key: &str) -> Result<(), StateError> {
        delete_key(self.connection.clone(), key).await
    }

    async fn dedup_exists(&self, key: &str) -> Result<bool, StateError> {
        let mut connection = self.connection.clone();
        let exists: i64 = redis::cmd("EXISTS")
            .arg(key)
            .query_async(&mut connection)
            .await?;
        Ok(exists != 0)
    }

    async fn enqueue(&self, stream: &str, payload: &str) -> Result<String, StateError> {
        let mut connection = self.connection.clone();
        Ok(redis::cmd("XADD")
            .arg(stream)
            .arg("*")
            .arg("payload")
            .arg(payload)
            .query_async(&mut connection)
            .await?)
    }

    async fn claim_dedup_and_enqueue(
        &self,
        dedup_key: &str,
        ttl_seconds: u64,
        stream: &str,
        payload: &str,
    ) -> Result<bool, StateError> {
        let mut connection = self.connection.clone();
        // KEYS[1] = dedup key, KEYS[2] = stream; ARGV[1] = ttl, ARGV[2] = payload.
        // `SET NX` returning false means another worker owns the event; skip the
        // append so the XADD is only attempted once per dedup window.
        let claimed: i64 = redis::Script::new(
            "if redis.call('set', KEYS[1], '1', 'NX', 'EX', ARGV[1]) then \
                 redis.call('xadd', KEYS[2], '*', 'payload', ARGV[2]) \
                 return 1 \
             else \
                 return 0 \
             end",
        )
        .key(dedup_key)
        .key(stream)
        .arg(ttl_seconds.max(1))
        .arg(payload)
        .invoke_async(&mut connection)
        .await?;
        Ok(claimed != 0)
    }

    async fn ack(&self, stream: &str, group: &str, message_id: &str) -> Result<(), StateError> {
        let mut connection = self.connection.clone();
        let _: i64 = redis::cmd("XACK")
            .arg(stream)
            .arg(group)
            .arg(message_id)
            .query_async(&mut connection)
            .await?;
        Ok(())
    }

    async fn dead_letter(&self, stream: &str, payload: &str) -> Result<String, StateError> {
        self.enqueue(stream, payload).await
    }

    async fn acquire_lock(
        &self,
        key: &str,
        owner_token: &str,
        ttl_seconds: u64,
    ) -> Result<bool, StateError> {
        let mut connection = self.connection.clone();
        let result: Option<String> = redis::cmd("SET")
            .arg(key)
            .arg(owner_token)
            .arg("NX")
            .arg("EX")
            .arg(ttl_seconds.max(1))
            .query_async(&mut connection)
            .await?;
        Ok(result.is_some())
    }

    async fn release_lock(&self, key: &str, owner_token: &str) -> Result<(), StateError> {
        let mut connection = self.connection.clone();
        redis::Script::new(
            "if redis.call('get', KEYS[1]) == ARGV[1] then return redis.call('del', KEYS[1]) else return 0 end",
        )
        .key(key)
        .arg(owner_token)
        .invoke_async::<i64>(&mut connection)
        .await?;
        Ok(())
    }

    async fn renew_lock(
        &self,
        key: &str,
        owner_token: &str,
        ttl_seconds: u64,
    ) -> Result<bool, StateError> {
        let mut connection = self.connection.clone();
        let renewed: i64 = redis::Script::new(
            "if redis.call('get', KEYS[1]) == ARGV[1] then return redis.call('expire', KEYS[1], ARGV[2]) else return 0 end",
        )
        .key(key)
        .arg(owner_token)
        .arg(ttl_seconds.max(1))
        .invoke_async(&mut connection)
        .await?;
        Ok(renewed != 0)
    }

    async fn read_batch(
        &self,
        stream: &str,
        group: &str,
        consumer: &str,
        count: usize,
    ) -> Result<Vec<StreamMessage>, StateError> {
        let mut connection = self.connection.clone();
        // The worker is deployable from an empty Redis: create its group lazily.
        // BUSYGROUP is harmless when another instance created it first.
        let _: Result<String, redis::RedisError> = redis::cmd("XGROUP")
            .arg("CREATE")
            .arg(stream)
            .arg(group)
            .arg("0-0")
            .arg("MKSTREAM")
            .query_async(&mut connection)
            .await;
        // Recover messages left in the consumer group's PEL after a crash or
        // restart before reading new entries. The idle window has to clear the
        // worst-case *batch* duration: a worker processes up to `count` events
        // sequentially, each bounded by the single-event ceiling derived from
        // the live outbound config. If the window is shorter than that, a
        // second instance reclaims events that are still being processed and
        // delivers them twice (建议-6). Single-instance deployments are
        // unaffected; only duplicate-on-multi-instance is at stake, never loss.
        let idle_threshold_ms = match self.get_outbound_config().await {
            Ok(config) => reclaim_idle_threshold_ms(&config, count),
            // A Redis error here must not break the delivery loop: fall back to
            // the legacy constant instead of propagating the failure.
            Err(_) => count.max(1) as u64 * SINGLE_EVENT_CEILING_FLOOR_MS,
        };
        let reclaimed: redis::streams::StreamAutoClaimReply = redis::cmd("XAUTOCLAIM")
            .arg(stream)
            .arg(group)
            .arg(consumer)
            .arg(idle_threshold_ms)
            .arg("0-0")
            .arg("COUNT")
            .arg(count.max(1))
            .query_async(&mut connection)
            .await?;
        let mut reclaimed_messages = Vec::new();
        let mut malformed_ids = Vec::new();
        for item in reclaimed.claimed {
            let id = item.id.clone();
            match stream_id_to_message(item) {
                Some(message) => reclaimed_messages.push(message),
                None => malformed_ids.push(id),
            }
        }
        // A record without a `payload` field can never be processed. ACK it so
        // it stops being re-claimed every cycle and squatting in the PEL
        // (建议-7); a payload that is present but not valid JSON still routes
        // through the normal Err -> DLQ path.
        ack_malformed(&mut connection, stream, group, &malformed_ids).await;
        if !reclaimed_messages.is_empty() {
            return Ok(reclaimed_messages);
        }
        let reply: redis::streams::StreamReadReply = redis::cmd("XREADGROUP")
            .arg("GROUP")
            .arg(group)
            .arg(consumer)
            .arg("COUNT")
            .arg(count.max(1))
            .arg("STREAMS")
            .arg(stream)
            .arg(">")
            .query_async(&mut connection)
            .await?;
        let mut messages = Vec::new();
        let mut malformed_ids = Vec::new();
        for key in reply.keys {
            for item in key.ids {
                let id = item.id.clone();
                match stream_id_to_message(item) {
                    Some(message) => messages.push(message),
                    None => malformed_ids.push(id),
                }
            }
        }
        // Same handling as the XAUTOCLAIM path: drop unprocessable records from
        // the PEL instead of letting them spin forever (建议-7).
        ack_malformed(&mut connection, stream, group, &malformed_ids).await;
        Ok(messages)
    }

    async fn retry_or_dlq(
        &self,
        stream: &str,
        dlq: &str,
        group: &str,
        message: &StreamMessage,
        max_attempts: u32,
    ) -> Result<bool, StateError> {
        let mut connection = self.connection.clone();
        let retry_key = format!("retry:{stream}:{}", message.id);
        // Keep increment, DLQ append, and source ACK in one Redis script so a
        // restart cannot leave an acknowledged event out of both queues.
        let script = redis::Script::new(
            r#"local n = redis.call('INCR', KEYS[1])
               redis.call('EXPIRE', KEYS[1], 86400)
               if n < tonumber(ARGV[1]) then return 0 end
               redis.call('XADD', ARGV[2], '*', 'payload', ARGV[3])
               redis.call('XACK', ARGV[4], ARGV[5], ARGV[6])
               return 1"#,
        );
        let moved: i64 = script
            .key(retry_key)
            .arg(max_attempts.max(1))
            .arg(dlq)
            .arg(&message.payload)
            .arg(stream)
            .arg(group)
            .arg(&message.id)
            .invoke_async(&mut connection)
            .await?;
        Ok(moved != 0)
    }

    async fn set_ai_consent(&self, chat_id: i64, ttl_seconds: u64) -> Result<(), StateError> {
        let mut connection = self.connection.clone();
        let key = format!("consent:ai:{chat_id}");
        let expiry = unix_now().saturating_add(ttl_seconds as i64);
        redis::cmd("SET")
            .arg(key)
            .arg(expiry)
            .arg("EX")
            .arg(ttl_seconds.max(1))
            .query_async::<()>(&mut connection)
            .await?;
        Ok(())
    }

    async fn ai_consent_until(&self, chat_id: i64) -> Result<Option<i64>, StateError> {
        let mut connection = self.connection.clone();
        let value: Option<i64> = redis::cmd("GET")
            .arg(format!("consent:ai:{chat_id}"))
            .query_async(&mut connection)
            .await?;
        Ok(value.filter(|expiry| *expiry > unix_now()))
    }

    async fn clear_ai_consent(&self, chat_id: i64) -> Result<(), StateError> {
        let mut connection = self.connection.clone();
        redis::cmd("DEL")
            .arg(format!("consent:ai:{chat_id}"))
            .query_async::<()>(&mut connection)
            .await?;
        Ok(())
    }

    async fn get_outbound_config(&self) -> Result<OutboundConfig, StateError> {
        let mut connection = self.connection.clone();
        let raw: Option<String> = redis::cmd("GET")
            .arg("config:outbound")
            .query_async(&mut connection)
            .await?;
        raw.map(|value| {
            serde_json::from_str(&value).map_err(|_| {
                redis::RedisError::from((redis::ErrorKind::TypeError, "invalid outbound config"))
            })
        })
        .transpose()
        .map(|config| config.unwrap_or_default())
        .map_err(StateError::Redis)
    }

    async fn set_outbound_config(&self, config: &OutboundConfig) -> Result<(), StateError> {
        let mut connection = self.connection.clone();
        let value = serde_json::to_string(config).map_err(|_| {
            redis::RedisError::from((redis::ErrorKind::TypeError, "serialize outbound config"))
        })?;
        redis::cmd("SET")
            .arg("config:outbound")
            .arg(value)
            .query_async::<()>(&mut connection)
            .await?;
        Ok(())
    }

    async fn get_business_config(&self) -> Result<Option<serde_json::Value>, StateError> {
        let mut connection = self.connection.clone();
        let raw: Option<String> = redis::cmd("GET")
            .arg("config:business")
            .query_async(&mut connection)
            .await?;
        raw.map(|value| decrypt_config(self.encryption_key.as_ref(), &value))
            .transpose()
            .map_err(|_| StateError::Encryption)
    }

    async fn set_business_config(&self, config: &serde_json::Value) -> Result<(), StateError> {
        let mut connection = self.connection.clone();
        let value = encrypt_config(self.encryption_key.as_ref(), config)?;
        redis::cmd("SET")
            .arg("config:business")
            .arg(value)
            .query_async::<()>(&mut connection)
            .await?;
        redis::cmd("INCR")
            .arg("config:business:revision")
            .query_async::<u64>(&mut connection)
            .await?;
        Ok(())
    }

    async fn initialize_business_config(
        &self,
        config: &serde_json::Value,
    ) -> Result<bool, StateError> {
        let mut connection = self.connection.clone();
        let value = encrypt_config(self.encryption_key.as_ref(), config)?;
        let result: Option<String> = redis::cmd("SET")
            .arg("config:business")
            .arg(value)
            .arg("NX")
            .query_async(&mut connection)
            .await?;
        if result.is_some() {
            redis::cmd("SET")
                .arg("config:business:revision")
                .arg(1_u64)
                .arg("NX")
                .query_async::<()>(&mut connection)
                .await?;
        }
        Ok(result.is_some())
    }

    async fn put_admin_session(&self, digest: &str, ttl_seconds: u64) -> Result<(), StateError> {
        let mut connection = self.connection.clone();
        redis::cmd("SET")
            .arg("config:admin_session")
            .arg(digest)
            .arg("EX")
            .arg(ttl_seconds.max(1))
            .query_async::<()>(&mut connection)
            .await?;
        Ok(())
    }

    async fn admin_session_valid(&self, digest: &str) -> Result<bool, StateError> {
        let mut connection = self.connection.clone();
        let current: Option<String> = redis::cmd("GET")
            .arg("config:admin_session")
            .query_async(&mut connection)
            .await?;
        Ok(current.as_deref() == Some(digest))
    }

    async fn revoke_admin_session(&self, _digest: &str) -> Result<(), StateError> {
        delete_key(self.connection.clone(), "config:admin_session").await
    }

    async fn business_config_revision(&self) -> Result<u64, StateError> {
        let mut connection = self.connection.clone();
        let value: Option<u64> = redis::cmd("GET")
            .arg("config:business:revision")
            .query_async(&mut connection)
            .await?;
        Ok(value.unwrap_or(0))
    }

    async fn is_enabled(&self) -> Result<bool, StateError> {
        let mut connection = self.connection.clone();
        let value: Option<bool> = redis::cmd("GET")
            .arg("config:enabled")
            .query_async(&mut connection)
            .await?;
        Ok(value.unwrap_or(false))
    }

    async fn set_enabled(&self, enabled: bool) -> Result<(), StateError> {
        let mut connection = self.connection.clone();
        redis::cmd("SET")
            .arg("config:enabled")
            .arg(if enabled { "1" } else { "0" })
            .query_async::<()>(&mut connection)
            .await?;
        Ok(())
    }

    async fn get_reconcile_state(&self) -> Result<Option<String>, StateError> {
        let mut connection = self.connection.clone();
        Ok(redis::cmd("GET")
            .arg("state:jmap:since")
            .query_async(&mut connection)
            .await?)
    }

    async fn set_reconcile_state(&self, state: &str) -> Result<(), StateError> {
        let mut connection = self.connection.clone();
        redis::cmd("SET")
            .arg("state:jmap:since")
            .arg(state)
            .query_async::<()>(&mut connection)
            .await?;
        Ok(())
    }

    async fn remember_push_subscription(
        &self,
        subscription_id: &str,
        verification_code: &str,
        ttl_seconds: u64,
    ) -> Result<(), StateError> {
        let mut connection = self.connection.clone();
        redis::cmd("SET")
            .arg(format!("push:subscription:{subscription_id}"))
            .arg(crate::config::session_digest(verification_code))
            .arg("EX")
            .arg(ttl_seconds.max(1))
            .query_async::<()>(&mut connection)
            .await?;
        Ok(())
    }

    async fn push_subscription_verified(
        &self,
        subscription_id: &str,
        verification_code: &str,
    ) -> Result<bool, StateError> {
        let mut connection = self.connection.clone();
        let current: Option<String> = redis::cmd("GET")
            .arg(format!("push:subscription:{subscription_id}"))
            .query_async(&mut connection)
            .await?;
        Ok(current.as_deref() == Some(crate::config::session_digest(verification_code).as_str()))
    }

    async fn remember_push_subscription_id(&self, subscription_id: &str) -> Result<(), StateError> {
        let mut connection = self.connection.clone();
        redis::cmd("SET")
            .arg("push:subscription:id")
            .arg(subscription_id)
            .query_async::<()>(&mut connection)
            .await?;
        Ok(())
    }

    async fn set_push_subscription_status(
        &self,
        subscription_id: &str,
        status: &str,
        ttl_seconds: u64,
    ) -> Result<(), StateError> {
        let mut connection = self.connection.clone();
        redis::cmd("SET")
            .arg(format!("push:subscription:{subscription_id}:status"))
            .arg(status)
            .arg("EX")
            .arg(ttl_seconds.max(1))
            .query_async::<()>(&mut connection)
            .await?;
        Ok(())
    }

    async fn forget_push_subscription(&self, subscription_id: &str) -> Result<(), StateError> {
        let mut connection = self.connection.clone();
        redis::cmd("DEL")
            .arg(format!("push:subscription:{subscription_id}"))
            .query_async::<()>(&mut connection)
            .await?;
        Ok(())
    }

    async fn record_push_orphan(
        &self,
        subscription_id: &str,
        request_id: &str,
    ) -> Result<(), StateError> {
        let mut connection = self.connection.clone();
        redis::cmd("SET")
            .arg(format!("push:orphan:{subscription_id}"))
            .arg(request_id)
            .arg("EX")
            .arg(604_800_u64)
            .query_async::<()>(&mut connection)
            .await?;
        Ok(())
    }

    async fn get_push_subscription_for_callback(
        &self,
        callback_url: &str,
    ) -> Result<Option<String>, StateError> {
        let mut connection = self.connection.clone();
        Ok(redis::cmd("GET")
            .arg(push_registration_key(callback_url))
            .query_async(&mut connection)
            .await?)
    }

    async fn remember_push_subscription_for_callback(
        &self,
        callback_url: &str,
        subscription_id: &str,
    ) -> Result<(), StateError> {
        let mut connection = self.connection.clone();
        redis::cmd("SET")
            .arg(push_registration_key(callback_url))
            .arg(subscription_id)
            .arg("EX")
            .arg(604_800_u64)
            .query_async::<()>(&mut connection)
            .await?;
        Ok(())
    }

    async fn remove_push_subscription_for_callback(
        &self,
        callback_url: &str,
    ) -> Result<(), StateError> {
        let mut connection = self.connection.clone();
        redis::cmd("DEL")
            .arg(push_registration_key(callback_url))
            .query_async::<()>(&mut connection)
            .await?;
        Ok(())
    }
}

fn push_registration_key(callback_url: &str) -> String {
    format!(
        "push:registration:{}",
        crate::config::session_digest(callback_url)
    )
}

fn stream_id_to_message(item: redis::streams::StreamId) -> Option<StreamMessage> {
    let payload = item
        .map
        .get("payload")
        .and_then(|value| redis::from_redis_value(value).ok());
    payload.map(|payload| StreamMessage {
        id: item.id,
        payload,
    })
}

/// XACK stream records that arrived without a parseable `payload` field. These
/// can never be processed, so acknowledging them keeps the consumer group's PEL
/// from filling up with unclaimable junk; the failure is logged for operators.
async fn ack_malformed(
    connection: &mut redis::aio::MultiplexedConnection,
    stream: &str,
    group: &str,
    ids: &[String],
) {
    if ids.is_empty() {
        return;
    }
    let ack = redis::cmd("XACK")
        .arg(stream)
        .arg(group)
        .arg(ids)
        .query_async::<i64>(connection)
        .await;
    if ack
        .as_ref()
        .map(|acked| *acked != ids.len() as i64)
        .unwrap_or(true)
    {
        eprintln!(
            "WARN malformed stream records ack incomplete (stream={stream}, group={group}, expected={}, result={ack:?})",
            ids.len()
        );
    }
}

async fn set_nx_ex(
    mut connection: redis::aio::MultiplexedConnection,
    key: &str,
    ttl_seconds: u64,
) -> Result<bool, StateError> {
    let result: Option<String> = redis::cmd("SET")
        .arg(key)
        .arg("1")
        .arg("NX")
        .arg("EX")
        // Guard against a ttl of 0, which Redis rejects with "invalid expire
        // time" (same normalization as acquire_lock).
        .arg(ttl_seconds.max(1))
        .query_async(&mut connection)
        .await?;
    Ok(result.is_some())
}

#[derive(Clone, Default)]
pub struct MemoryState {
    inner: Arc<Mutex<MemoryInner>>,
}

#[cfg(test)]
impl MemoryState {
    pub fn enabled_for_tests() -> Self {
        let state = Self::default();
        state.inner.lock().expect("new state lock").enabled = true;
        state
    }
}

#[derive(Default)]
struct MemoryInner {
    dedup: HashSet<String>,
    locks: HashMap<String, String>,
    push_registrations: HashMap<String, String>,
    streams: HashMap<String, Vec<(String, String)>>,
    retries: HashMap<(String, String), u32>,
    consent: HashMap<i64, i64>,
    next_id: u64,
    outbound: OutboundConfig,
    business_config: Option<serde_json::Value>,
    admin_session: Option<(String, i64)>,
    business_revision: u64,
    enabled: bool,
    reconcile_state: Option<String>,
    push_subscriptions: HashMap<String, (String, i64)>,
    push_subscription_id: Option<String>,
    push_status: HashMap<String, (String, i64)>,
    push_orphans: HashMap<String, String>,
}

#[async_trait]
impl ReliableState for MemoryState {
    async fn claim_dedup(&self, key: &str, _ttl_seconds: u64) -> Result<bool, StateError> {
        let mut inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        Ok(inner.dedup.insert(key.to_owned()))
    }
    async fn release_dedup(&self, key: &str) -> Result<(), StateError> {
        let mut inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        inner.dedup.remove(key);
        Ok(())
    }
    async fn dedup_exists(&self, key: &str) -> Result<bool, StateError> {
        let inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        Ok(inner.dedup.contains(key))
    }
    async fn enqueue(&self, stream: &str, payload: &str) -> Result<String, StateError> {
        let mut inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        inner.next_id += 1;
        let id = format!("{}-0", inner.next_id);
        inner
            .streams
            .entry(stream.to_owned())
            .or_default()
            .push((id.clone(), payload.to_owned()));
        Ok(id)
    }
    async fn claim_dedup_and_enqueue(
        &self,
        dedup_key: &str,
        _ttl_seconds: u64,
        stream: &str,
        payload: &str,
    ) -> Result<bool, StateError> {
        // Hold the lock across claim + append so MemoryState honors the same
        // atomicity contract as the Lua-backed RedisState implementation.
        let mut inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        if !inner.dedup.insert(dedup_key.to_owned()) {
            return Ok(false);
        }
        inner.next_id += 1;
        let id = format!("{}-0", inner.next_id);
        inner
            .streams
            .entry(stream.to_owned())
            .or_default()
            .push((id, payload.to_owned()));
        Ok(true)
    }
    async fn ack(&self, stream: &str, _group: &str, message_id: &str) -> Result<(), StateError> {
        let mut inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        if let Some(messages) = inner.streams.get_mut(stream) {
            messages.retain(|(id, _)| id != message_id);
        }
        Ok(())
    }
    async fn dead_letter(&self, stream: &str, payload: &str) -> Result<String, StateError> {
        self.enqueue(stream, payload).await
    }
    async fn acquire_lock(
        &self,
        key: &str,
        owner_token: &str,
        _ttl_seconds: u64,
    ) -> Result<bool, StateError> {
        let mut inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        if inner.locks.contains_key(key) {
            return Ok(false);
        }
        inner.locks.insert(key.to_owned(), owner_token.to_owned());
        Ok(true)
    }
    async fn release_lock(&self, key: &str, owner_token: &str) -> Result<(), StateError> {
        let mut inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        if inner
            .locks
            .get(key)
            .is_some_and(|owner| owner == owner_token)
        {
            inner.locks.remove(key);
        }
        Ok(())
    }

    async fn renew_lock(
        &self,
        key: &str,
        owner_token: &str,
        _ttl_seconds: u64,
    ) -> Result<bool, StateError> {
        let inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        Ok(inner
            .locks
            .get(key)
            .is_some_and(|owner| owner == owner_token))
    }
    async fn read_batch(
        &self,
        stream: &str,
        _group: &str,
        _consumer: &str,
        count: usize,
    ) -> Result<Vec<StreamMessage>, StateError> {
        let inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        Ok(inner
            .streams
            .get(stream)
            .into_iter()
            .flatten()
            .take(count.max(1))
            .map(|(id, payload)| StreamMessage {
                id: id.clone(),
                payload: payload.clone(),
            })
            .collect())
    }
    async fn retry_or_dlq(
        &self,
        stream: &str,
        dlq: &str,
        _group: &str,
        message: &StreamMessage,
        max_attempts: u32,
    ) -> Result<bool, StateError> {
        let mut inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        let key = (stream.to_owned(), message.id.clone());
        let attempts = {
            let entry = inner.retries.entry(key).or_default();
            *entry += 1;
            *entry
        };
        if attempts >= max_attempts {
            inner.next_id += 1;
            let id = format!("{}-0", inner.next_id);
            inner
                .streams
                .entry(dlq.to_owned())
                .or_default()
                .push((id, message.payload.clone()));
            if let Some(messages) = inner.streams.get_mut(stream) {
                messages.retain(|(source_id, _)| source_id != &message.id);
            }
            return Ok(true);
        }
        let _ = stream;
        Ok(false)
    }

    async fn set_ai_consent(&self, chat_id: i64, ttl_seconds: u64) -> Result<(), StateError> {
        let mut inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        inner
            .consent
            .insert(chat_id, unix_now().saturating_add(ttl_seconds as i64));
        Ok(())
    }

    async fn ai_consent_until(&self, chat_id: i64) -> Result<Option<i64>, StateError> {
        let inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        Ok(inner
            .consent
            .get(&chat_id)
            .copied()
            .filter(|expiry| *expiry > unix_now()))
    }

    async fn clear_ai_consent(&self, chat_id: i64) -> Result<(), StateError> {
        let mut inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        inner.consent.remove(&chat_id);
        Ok(())
    }

    async fn get_outbound_config(&self) -> Result<OutboundConfig, StateError> {
        let inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        Ok(inner.outbound.clone())
    }

    async fn set_outbound_config(&self, config: &OutboundConfig) -> Result<(), StateError> {
        let mut inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        inner.outbound = config.clone();
        Ok(())
    }

    async fn get_business_config(&self) -> Result<Option<serde_json::Value>, StateError> {
        let inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        Ok(inner.business_config.clone())
    }

    async fn set_business_config(&self, config: &serde_json::Value) -> Result<(), StateError> {
        let mut inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        inner.business_config = Some(config.clone());
        inner.business_revision = inner.business_revision.saturating_add(1);
        Ok(())
    }

    async fn initialize_business_config(
        &self,
        config: &serde_json::Value,
    ) -> Result<bool, StateError> {
        let mut inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        if inner.business_config.is_some() {
            return Ok(false);
        }
        inner.business_config = Some(config.clone());
        inner.business_revision = 1;
        Ok(true)
    }

    async fn put_admin_session(&self, digest: &str, ttl_seconds: u64) -> Result<(), StateError> {
        let mut inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        inner.admin_session = Some((
            digest.to_owned(),
            unix_now().saturating_add(ttl_seconds as i64),
        ));
        Ok(())
    }

    async fn admin_session_valid(&self, digest: &str) -> Result<bool, StateError> {
        let inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        Ok(inner
            .admin_session
            .as_ref()
            .is_some_and(|(value, expiry)| value == digest && *expiry > unix_now()))
    }

    async fn revoke_admin_session(&self, digest: &str) -> Result<(), StateError> {
        let mut inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        if inner
            .admin_session
            .as_ref()
            .is_some_and(|(value, _)| value == digest)
        {
            inner.admin_session = None;
        }
        Ok(())
    }

    async fn business_config_revision(&self) -> Result<u64, StateError> {
        let inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        Ok(inner.business_revision)
    }

    async fn is_enabled(&self) -> Result<bool, StateError> {
        Ok(self.inner.lock().map_err(|_| StateError::Poisoned)?.enabled)
    }

    async fn set_enabled(&self, enabled: bool) -> Result<(), StateError> {
        self.inner.lock().map_err(|_| StateError::Poisoned)?.enabled = enabled;
        Ok(())
    }

    async fn get_reconcile_state(&self) -> Result<Option<String>, StateError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| StateError::Poisoned)?
            .reconcile_state
            .clone())
    }

    async fn set_reconcile_state(&self, state: &str) -> Result<(), StateError> {
        self.inner
            .lock()
            .map_err(|_| StateError::Poisoned)?
            .reconcile_state = Some(state.to_owned());
        Ok(())
    }

    async fn remember_push_subscription(
        &self,
        subscription_id: &str,
        verification_code: &str,
        ttl_seconds: u64,
    ) -> Result<(), StateError> {
        self.inner
            .lock()
            .map_err(|_| StateError::Poisoned)?
            .push_subscriptions
            .insert(
                subscription_id.to_owned(),
                (
                    crate::config::session_digest(verification_code),
                    unix_now().saturating_add(ttl_seconds as i64),
                ),
            );
        Ok(())
    }

    async fn push_subscription_verified(
        &self,
        subscription_id: &str,
        verification_code: &str,
    ) -> Result<bool, StateError> {
        let mut inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        let Some((digest, expiry)) = inner.push_subscriptions.get(subscription_id) else {
            return Ok(false);
        };
        if *expiry <= unix_now() {
            inner.push_subscriptions.remove(subscription_id);
            return Ok(false);
        }
        Ok(digest == &crate::config::session_digest(verification_code))
    }

    async fn remember_push_subscription_id(&self, subscription_id: &str) -> Result<(), StateError> {
        self.inner
            .lock()
            .map_err(|_| StateError::Poisoned)?
            .push_subscription_id = Some(subscription_id.to_owned());
        Ok(())
    }

    async fn get_push_subscription_for_callback(
        &self,
        callback_url: &str,
    ) -> Result<Option<String>, StateError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| StateError::Poisoned)?
            .push_registrations
            .get(&push_registration_key(callback_url))
            .cloned())
    }

    async fn remember_push_subscription_for_callback(
        &self,
        callback_url: &str,
        subscription_id: &str,
    ) -> Result<(), StateError> {
        self.inner
            .lock()
            .map_err(|_| StateError::Poisoned)?
            .push_registrations
            .insert(
                push_registration_key(callback_url),
                subscription_id.to_owned(),
            );
        Ok(())
    }

    async fn remove_push_subscription_for_callback(
        &self,
        callback_url: &str,
    ) -> Result<(), StateError> {
        self.inner
            .lock()
            .map_err(|_| StateError::Poisoned)?
            .push_registrations
            .remove(&push_registration_key(callback_url));
        Ok(())
    }

    async fn set_push_subscription_status(
        &self,
        subscription_id: &str,
        status: &str,
        ttl_seconds: u64,
    ) -> Result<(), StateError> {
        self.inner
            .lock()
            .map_err(|_| StateError::Poisoned)?
            .push_status
            .insert(
                subscription_id.to_owned(),
                (
                    status.to_owned(),
                    unix_now().saturating_add(ttl_seconds as i64),
                ),
            );
        Ok(())
    }

    async fn forget_push_subscription(&self, subscription_id: &str) -> Result<(), StateError> {
        let mut inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        inner.push_subscriptions.remove(subscription_id);
        Ok(())
    }

    async fn record_push_orphan(
        &self,
        subscription_id: &str,
        request_id: &str,
    ) -> Result<(), StateError> {
        self.inner
            .lock()
            .map_err(|_| StateError::Poisoned)?
            .push_orphans
            .insert(subscription_id.to_owned(), request_id.to_owned());
        Ok(())
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}

const CONFIG_AAD: &[u8] = b"message-weave/config:business/v1";

fn encrypt_config(
    key: Option<&[u8; 32]>,
    config: &serde_json::Value,
) -> Result<String, StateError> {
    let key = key.ok_or(StateError::Encryption)?;
    let unbound =
        aead::UnboundKey::new(&aead::AES_256_GCM, key).map_err(|_| StateError::Encryption)?;
    let sealing = aead::LessSafeKey::new(unbound);
    let mut nonce = [0_u8; 12];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| StateError::Encryption)?;
    let mut body = serde_json::to_vec(config).map_err(|_| StateError::Encryption)?;
    sealing
        .seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(CONFIG_AAD),
            &mut body,
        )
        .map_err(|_| StateError::Encryption)?;
    Ok(format!("v1:{}:{}", hex_encode(&nonce), hex_encode(&body)))
}

fn decrypt_config(key: Option<&[u8; 32]>, value: &str) -> Result<serde_json::Value, StateError> {
    let key = key.ok_or(StateError::Encryption)?;
    let mut parts = value.split(':');
    if parts.next() != Some("v1") {
        return Err(StateError::Encryption);
    }
    let nonce = hex_decode(parts.next().ok_or(StateError::Encryption)?, 12)?;
    let mut body = hex_decode(parts.next().ok_or(StateError::Encryption)?, usize::MAX)?;
    if parts.next().is_some() {
        return Err(StateError::Encryption);
    }
    let unbound =
        aead::UnboundKey::new(&aead::AES_256_GCM, key).map_err(|_| StateError::Encryption)?;
    let opening = aead::LessSafeKey::new(unbound);
    let plain = opening
        .open_in_place(
            aead::Nonce::assume_unique_for_key(
                nonce.try_into().map_err(|_| StateError::Encryption)?,
            ),
            aead::Aad::from(CONFIG_AAD),
            &mut body,
        )
        .map_err(|_| StateError::Encryption)?;
    serde_json::from_slice(plain).map_err(|_| StateError::Encryption)
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn hex_decode(value: &str, max_len: usize) -> Result<Vec<u8>, StateError> {
    if !value.len().is_multiple_of(2) || value.len() / 2 > max_len {
        return Err(StateError::Encryption);
    }
    (0..value.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&value[index..index + 2], 16).map_err(|_| StateError::Encryption)
        })
        .collect()
}

async fn delete_key(
    mut connection: redis::aio::MultiplexedConnection,
    key: &str,
) -> Result<(), StateError> {
    let _: i64 = redis::cmd("DEL")
        .arg(key)
        .query_async(&mut connection)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rediss_url_accepts_default_acl_and_encoded_password() {
        let client = redis::Client::open("rediss://default:p%40ss%3Aword@example.invalid:6379/0");
        assert!(client.is_ok());
        let client = redis::Client::open("redis://default:p%40ss%3Aword@example.invalid:6379/0");
        assert!(client.is_ok());
    }

    #[test]
    fn reclaim_idle_threshold_is_derived_from_runtime_config() {
        // (15s + 10s + 30s) * (3 + 1) attempts * 2 safety = 440s per event.
        let default = OutboundConfig::default();
        assert_eq!(single_event_ceiling_ms(&default), 440_000);
        assert!(reclaim_idle_threshold_ms(&default, 1) >= SINGLE_EVENT_CEILING_FLOOR_MS);
        assert!(reclaim_idle_threshold_ms(&default, 16) > reclaim_idle_threshold_ms(&default, 8));

        let slow = OutboundConfig {
            jmap_timeout_ms: 60_000,
            telegram_timeout_ms: 60_000,
            llm_timeout_ms: 60_000,
            max_retries: 4,
        };
        assert!(reclaim_idle_threshold_ms(&slow, 1) > reclaim_idle_threshold_ms(&default, 1));
    }

    #[test]
    fn reclaim_idle_threshold_is_floored_and_capped() {
        let zeroed = OutboundConfig {
            jmap_timeout_ms: 0,
            telegram_timeout_ms: 0,
            llm_timeout_ms: 0,
            max_retries: 0,
        };
        assert_eq!(
            single_event_ceiling_ms(&zeroed),
            SINGLE_EVENT_CEILING_FLOOR_MS
        );

        let absurd = OutboundConfig {
            jmap_timeout_ms: 3_600_000,
            telegram_timeout_ms: 3_600_000,
            llm_timeout_ms: 3_600_000,
            max_retries: 5,
        };
        assert_eq!(
            reclaim_idle_threshold_ms(&absurd, 64),
            RECLAIM_IDLE_THRESHOLD_CAP_MS
        );
    }

    #[test]
    fn reclaim_idle_threshold_ignores_empty_batches() {
        let default = OutboundConfig::default();
        assert_eq!(
            reclaim_idle_threshold_ms(&default, 0),
            single_event_ceiling_ms(&default)
        );
    }

    #[test]
    fn encrypted_config_roundtrip_and_tamper_fail_closed() {
        let key = [7_u8; 32];
        let value = serde_json::json!({"secret": "value"});
        let encoded = encrypt_config(Some(&key), &value).unwrap();
        assert_eq!(decrypt_config(Some(&key), &encoded).unwrap(), value);
        let mut tampered = encoded.into_bytes();
        let index = tampered.len() - 1;
        tampered[index] = if tampered[index] == b'0' { b'1' } else { b'0' };
        let tampered = String::from_utf8(tampered).unwrap();
        assert!(decrypt_config(Some(&key), &tampered).is_err());
        assert!(decrypt_config(Some(&[8_u8; 32]), &tampered).is_err());
        assert!(encrypt_config(None, &value).is_err());
    }

    #[tokio::test]
    async fn memory_state_is_atomic_and_supports_stream_lifecycle() {
        let state = MemoryState::default();
        assert!(!state.is_enabled().await.unwrap());
        state.set_enabled(true).await.unwrap();
        assert!(state.is_enabled().await.unwrap());
        state.set_enabled(false).await.unwrap();
        assert!(!state.is_enabled().await.unwrap());
        assert!(state.get_reconcile_state().await.unwrap().is_none());
        state.set_reconcile_state("jmap-state-1").await.unwrap();
        assert_eq!(
            state.get_reconcile_state().await.unwrap().as_deref(),
            Some("jmap-state-1")
        );
        assert!(state.claim_dedup("tg:42", 60).await.unwrap());
        assert!(!state.claim_dedup("tg:42", 60).await.unwrap());
        assert!(state.dedup_exists("tg:42").await.unwrap());
        state.release_dedup("tg:42").await.unwrap();
        assert!(!state.dedup_exists("tg:42").await.unwrap());
        // A short in-flight lease can be cleared after a pre-send crash,
        // while the post-send commit remains durable for ACK recovery.
        assert!(state
            .claim_dedup("delivery:inflight:events:1-0", 60)
            .await
            .unwrap());
        state
            .release_dedup("delivery:inflight:events:1-0")
            .await
            .unwrap();
        assert!(!state
            .dedup_exists("delivery:inflight:events:1-0")
            .await
            .unwrap());
        assert!(state
            .claim_dedup("delivery:committed:events:1-0", 604_800)
            .await
            .unwrap());
        assert!(state
            .dedup_exists("delivery:committed:events:1-0")
            .await
            .unwrap());
        assert!(state
            .acquire_lock("reconcile", "owner-a", 30)
            .await
            .unwrap());
        assert!(!state
            .acquire_lock("reconcile", "owner-b", 30)
            .await
            .unwrap());
        state.release_lock("reconcile", "owner-b").await.unwrap();
        assert!(!state
            .acquire_lock("reconcile", "owner-b", 30)
            .await
            .unwrap());
        assert!(!state.renew_lock("reconcile", "owner-b", 90).await.unwrap());
        assert!(state.renew_lock("reconcile", "owner-a", 90).await.unwrap());
        state.release_lock("reconcile", "owner-a").await.unwrap();
        assert!(state
            .acquire_lock("reconcile", "owner-b", 30)
            .await
            .unwrap());
        state
            .remember_push_subscription_for_callback(
                "https://example.test/push/jmap",
                "subscription-1",
            )
            .await
            .unwrap();
        assert_eq!(
            state
                .get_push_subscription_for_callback("https://example.test/push/jmap")
                .await
                .unwrap()
                .as_deref(),
            Some("subscription-1")
        );
        state
            .set_push_subscription_status("subscription-1", "disabled", 60)
            .await
            .unwrap();
        // `pending/verified/disabled` 状态键是只写的运维轨迹（建议-5a）：
        // 授权语义由验证码摘要承担，故这里不对其做读取断言。
        state
            .remove_push_subscription_for_callback("https://example.test/push/jmap")
            .await
            .unwrap();
        assert!(state
            .get_push_subscription_for_callback("https://example.test/push/jmap")
            .await
            .unwrap()
            .is_none());
        assert!(state.ai_consent_until(7).await.unwrap().is_none());
        state.set_ai_consent(7, 3600).await.unwrap();
        assert!(state.ai_consent_until(7).await.unwrap().is_some());
        state.clear_ai_consent(7).await.unwrap();
        assert!(state.ai_consent_until(7).await.unwrap().is_none());
        let id = state.enqueue("events", "payload").await.unwrap();
        let message = StreamMessage {
            id: id.clone(),
            payload: "payload".into(),
        };
        assert!(!state
            .retry_or_dlq("events", "events-dlq", "workers", &message, 2)
            .await
            .unwrap());
        assert!(state
            .retry_or_dlq("events", "events-dlq", "workers", &message, 2)
            .await
            .unwrap());
        assert!(state
            .read_batch("events", "workers", "c", 10)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            state
                .read_batch("events-dlq", "workers", "c", 10)
                .await
                .unwrap()
                .len(),
            1
        );
        state.ack("events-dlq", "workers", &id).await.unwrap();
    }
}
