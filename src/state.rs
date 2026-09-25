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
    /// XACK after successful processing.
    async fn ack(&self, stream: &str, group: &str, message_id: &str) -> Result<(), StateError>;
    /// XADD to a dead-letter stream after bounded retry policy decides to stop retrying.
    async fn dead_letter(&self, stream: &str, payload: &str) -> Result<String, StateError>;
    /// SET NX EX lock for reconcile single-flight.
    async fn acquire_lock(&self, key: &str, ttl_seconds: u64) -> Result<bool, StateError>;
    /// Release a lock after the bounded reconcile attempt has completed.
    async fn release_lock(&self, key: &str) -> Result<(), StateError>;
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
            .arg("MAXLEN")
            .arg("~")
            .arg(10_000)
            .arg("*")
            .arg("payload")
            .arg(payload)
            .query_async(&mut connection)
            .await?)
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

    async fn acquire_lock(&self, key: &str, ttl_seconds: u64) -> Result<bool, StateError> {
        set_nx_ex(self.connection.clone(), key, ttl_seconds).await
    }

    async fn release_lock(&self, key: &str) -> Result<(), StateError> {
        delete_key(self.connection.clone(), key).await
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
        // restart before reading new entries. A bounded idle window prevents
        // two short-lived workers from stealing active work.
        let reclaimed: redis::streams::StreamAutoClaimReply = redis::cmd("XAUTOCLAIM")
            .arg(stream)
            .arg(group)
            .arg(consumer)
            // A worker handles at most ten events per request; keep this
            // above the worst bounded JMAP/Telegram batch duration.
            .arg(300_000)
            .arg("0-0")
            .arg("COUNT")
            .arg(count.max(1))
            .query_async(&mut connection)
            .await?;
        let reclaimed_messages = reclaimed
            .claimed
            .into_iter()
            .filter_map(stream_id_to_message)
            .collect::<Vec<_>>();
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
        Ok(reply
            .keys
            .into_iter()
            .flat_map(|key| key.ids.into_iter().filter_map(stream_id_to_message))
            .collect())
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
               redis.call('XADD', ARGV[2], 'MAXLEN', '~', 10000, '*', 'payload', ARGV[3])
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
        .arg(ttl_seconds)
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
    locks: HashSet<String>,
    streams: HashMap<String, Vec<(String, String)>>,
    retries: HashMap<(String, String), u32>,
    consent: HashMap<i64, i64>,
    next_id: u64,
    outbound: OutboundConfig,
    business_config: Option<serde_json::Value>,
    admin_session: Option<(String, i64)>,
    business_revision: u64,
    enabled: bool,
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
        if let Some(messages) = inner.streams.get_mut(stream) {
            let excess = messages.len().saturating_sub(10_000);
            if excess > 0 {
                messages.drain(..excess);
            }
        }
        Ok(id)
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
    async fn acquire_lock(&self, key: &str, _ttl_seconds: u64) -> Result<bool, StateError> {
        let mut inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        Ok(inner.locks.insert(key.to_owned()))
    }
    async fn release_lock(&self, key: &str) -> Result<(), StateError> {
        let mut inner = self.inner.lock().map_err(|_| StateError::Poisoned)?;
        inner.locks.remove(key);
        Ok(())
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
        assert!(state.acquire_lock("reconcile", 30).await.unwrap());
        assert!(!state.acquire_lock("reconcile", 30).await.unwrap());
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
