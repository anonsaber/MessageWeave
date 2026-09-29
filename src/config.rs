// SPLIT-EVAL: 已评估暂缓拆分——仅略超软上限约 20 行，结构体定义与 Redis 配置解析路径相互引用，拆出只增加跨文件跳转而不消除任何重复。
use secrecy::{ExposeSecret, SecretString};

use crate::error::BotError;

/// Returns the SHA-256 digest used for Redis admin-session records. The bearer token itself
/// never enters Redis (SAF-ADMIN-SESSION, C-REDIS-ONLY-STATE).
pub(crate) fn session_digest(token: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, token.as_bytes());
    digest
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn encryption_key_from_env() -> Result<[u8; 32], BotError> {
    let value = std::env::var("CONFIG_ENCRYPTION_KEY")
        .map_err(|_| BotError::Config("missing CONFIG_ENCRYPTION_KEY".into()))?;
    if value.len() != 64 {
        return Err(BotError::Config(
            "CONFIG_ENCRYPTION_KEY must be 32-byte hex".into(),
        ));
    }
    let mut key = [0_u8; 32];
    for (index, slot) in key.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| BotError::Config("CONFIG_ENCRYPTION_KEY must be hex".into()))?;
    }
    Ok(key)
}

/// Default display zone for notification timestamps.
pub fn default_timezone() -> String {
    "Asia/Shanghai".into()
}

/// The only zones this build can render. Each is a fixed-offset zone with no
/// daylight-saving transitions, so a stored offset is correct year round.
///
/// Only zones with a permanent offset are listed. The IANA database is not
/// available in this build (no `chrono-tz`), so a DST zone would silently
/// report the wrong wall clock time for part of the year. Adding one here is
/// therefore a silent-correctness bug, not a feature.
pub(crate) const SUPPORTED_TIMEZONES: &[(&str, &str, &str)] = &[
    ("Etc/UTC", "UTC", "+00:00"),
    ("Africa/Cairo", "埃及开罗", "+02:00"),
    ("Europe/Istanbul", "土耳其伊斯坦布尔", "+03:00"),
    ("Africa/Nairobi", "肯尼亚内罗毕", "+03:00"),
    ("Asia/Dubai", "阿联酋迪拜", "+04:00"),
    ("Asia/Karachi", "巴基斯坦卡拉奇", "+05:00"),
    ("Asia/Kolkata", "印度加尔各答", "+05:30"),
    ("Asia/Bangkok", "泰国曼谷", "+07:00"),
    ("Asia/Ho_Chi_Minh", "越南河内", "+07:00"),
    ("Asia/Shanghai", "中国上海", "+08:00"),
    ("Asia/Hong_Kong", "中国香港", "+08:00"),
    ("Asia/Taipei", "中国台北", "+08:00"),
    ("Asia/Singapore", "新加坡", "+08:00"),
    ("Asia/Manila", "菲律宾马尼拉", "+08:00"),
    ("Asia/Tokyo", "日本东京", "+09:00"),
    ("Asia/Seoul", "韩国首尔", "+09:00"),
];

/// Resolve one supported zone to its fixed offset, in seconds east of UTC.
/// Returns `None` when the name is not in the table.
pub(crate) fn resolve_timezone(iana: &str) -> Option<i32> {
    SUPPORTED_TIMEZONES
        .iter()
        .find(|(name, _, _)| *name == iana)
        .map(|(_, _, offset)| {
            parse_offset_seconds(offset)
                .expect("static offsets in SUPPORTED_TIMEZONES parse as ±HH:MM")
        })
}

/// Parse a `±HH:MM` offset into seconds east of UTC. `None` for a malformed
/// string; the table entries are all well formed, so a failure here is a bug.
pub(crate) fn parse_offset_seconds(offset: &str) -> Option<i32> {
    let sign = if offset.starts_with('-') {
        -1_i32
    } else if offset.starts_with('+') {
        1_i32
    } else {
        return None;
    };
    let rest = &offset[1..];
    let (hours, minutes) = rest.split_once(':')?;
    let hours: i32 = hours.parse().ok()?;
    let minutes: i32 = minutes.parse().ok()?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some(sign * (hours * 3600 + minutes * 60))
}

/// Default notification display zone, as a fixed offset in seconds east of UTC.
pub(crate) fn default_timezone_offset() -> i32 {
    resolve_timezone(&default_timezone()).expect("the default zone is in the table")
}

/// Fixed offset for a zone already accepted by `TryFrom<BusinessConfigWire>`.
///
/// Configs only reach a worker after the zone name was validated against the table,
/// so the fallback is unreachable; it keeps the helper infallible rather than panicking
/// in a worker construction path.
pub(crate) fn timezone_offset_of(zone: &str) -> i32 {
    resolve_timezone(zone).unwrap_or_else(default_timezone_offset)
}

/// Runtime-only settings. SecretString prevents accidental formatting/logging of credentials.
/// C-AUTH-APP-BASIC and REQ-SINGLE-ACCOUNT are intentionally represented explicitly.
pub struct Config {
    pub telegram: TelegramConfig,
    pub jmap: JmapConfig,
    pub account_id: Option<String>,
    pub llm: LlmConfig,
    pub auth: AuthSecrets,
    pub worker_token: SecretString,
    /// Display zone mirrored from `BusinessConfig`; the worker renders timestamps in it.
    pub timezone: String,
}

/// Runtime-only endpoint credentials; never format or log these values.
/// C-NO-SECRET-IN-IMAGE and the stage-1 R1 boundary require process injection.
#[derive(Clone)]
pub struct AuthSecrets {
    pub reconcile_token: SecretString,
    pub telegram_webhook_secret: SecretString,
}

pub struct TelegramConfig {
    pub bot_token: SecretString,
    pub chat_allowlist: Vec<i64>,
    pub chat_id: i64,
}

pub struct JmapConfig {
    pub session_url: String,
    pub username: String,
    pub app_password: SecretString,
}

pub struct LlmConfig {
    pub enabled: bool,
    pub allow_net: bool,
    pub api_key: Option<SecretString>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub summary_target_chars: usize,
}

/// All business configuration persisted below the Redis `config:business` namespace.
/// Secrets intentionally have no `Debug` implementation and are never returned by the
/// public configuration API (C-REDIS-ONLY-STATE, SAF-NO-SECRET-ECHO).
#[derive(Clone)]
pub struct BusinessConfig {
    pub bot_token: SecretString,
    pub telegram_chat_id: i64,
    pub chat_allowlist: Vec<i64>,
    pub telegram_webhook_secret: SecretString,
    /// Display zone for notification timestamps; always one of SUPPORTED_TIMEZONES.
    pub timezone: String,
    pub jmap_session_url: String,
    pub jmap_username: String,
    pub jmap_password: SecretString,
    pub account_id: Option<String>,
    pub llm_enabled: bool,
    pub llm_allow_net: bool,
    pub llm_api_key: Option<SecretString>,
    pub llm_base_url: Option<String>,
    pub llm_model: Option<String>,
    pub reconcile_token: SecretString,
    pub worker_token: SecretString,
}

/// Redis wire representation. This type is private to the persistence adapter; callers must
/// never serialize it into an HTTP response (SAF-NO-SECRET-ECHO).
#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub(crate) struct BusinessConfigWire {
    pub bot_token: String,
    pub telegram_chat_id: i64,
    pub chat_allowlist: Vec<i64>,
    pub telegram_webhook_secret: String,
    #[serde(default = "default_timezone")]
    pub timezone: String,
    pub jmap_session_url: String,
    pub jmap_username: String,
    pub jmap_password: String,
    #[serde(default)]
    pub account_id: Option<String>,
    pub llm_enabled: bool,
    pub llm_allow_net: bool,
    pub llm_api_key: Option<String>,
    pub llm_base_url: Option<String>,
    pub llm_model: Option<String>,
    pub reconcile_token: String,
    pub worker_token: String,
}

impl From<BusinessConfig> for BusinessConfigWire {
    fn from(value: BusinessConfig) -> Self {
        Self {
            bot_token: value.bot_token.expose_secret().to_owned(),
            telegram_chat_id: value.telegram_chat_id,
            chat_allowlist: value.chat_allowlist,
            telegram_webhook_secret: value.telegram_webhook_secret.expose_secret().to_owned(),
            timezone: value.timezone,
            jmap_session_url: value.jmap_session_url,
            jmap_username: value.jmap_username,
            jmap_password: value.jmap_password.expose_secret().to_owned(),
            account_id: value.account_id,
            llm_enabled: value.llm_enabled,
            llm_allow_net: value.llm_allow_net,
            llm_api_key: value.llm_api_key.map(|s| s.expose_secret().to_owned()),
            llm_base_url: value.llm_base_url,
            llm_model: value.llm_model,
            reconcile_token: value.reconcile_token.expose_secret().to_owned(),
            worker_token: value.worker_token.expose_secret().to_owned(),
        }
    }
}

/// Partial business configuration submitted by `PUT /api/business-config`: a field the client
/// does not name keeps the stored value, so an operator can change one thing without retyping
/// the whole configuration.
///
/// The field list mirrors `BusinessConfigWire` field for field — `apply` builds a wire from these,
/// so adding a field to the wire without adding it here fails to compile instead of silently
/// freezing the new setting at its stored value. Secrets are replace-only: `validate_business_wire`
/// rejects a blank secret, so there is deliberately no way to clear one and no way for the SPA to
/// learn what it is holding.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BusinessConfigPatch {
    #[serde(default)]
    pub bot_token: Option<String>,
    #[serde(default, deserialize_with = "de_i64_from_number_or_string")]
    pub telegram_chat_id: Option<i64>,
    #[serde(default)]
    pub chat_allowlist: Option<Vec<i64>>,
    #[serde(default)]
    pub telegram_webhook_secret: Option<String>,
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub jmap_session_url: Option<String>,
    #[serde(default)]
    pub jmap_username: Option<String>,
    #[serde(default)]
    pub jmap_password: Option<String>,
    #[serde(default)]
    pub account_id: Option<Option<String>>,
    #[serde(default)]
    pub llm_enabled: Option<bool>,
    #[serde(default)]
    pub llm_allow_net: Option<bool>,
    #[serde(default)]
    pub llm_api_key: Option<Option<String>>,
    #[serde(default)]
    pub llm_base_url: Option<Option<String>>,
    #[serde(default)]
    pub llm_model: Option<Option<String>>,
    #[serde(default)]
    pub reconcile_token: Option<String>,
    #[serde(default)]
    pub worker_token: Option<String>,
}

/// `telegram_chat_id` arrives as a JSON number on a full submit but as a numeric string on a
/// partial one (`web/config.js` stringifies the `BigInt` to keep the value exact). Both shapes are
/// accepted; anything else is rejected rather than coerced.
fn de_i64_from_number_or_string<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    use serde::Deserialize;
    match Option::<serde_json::Value>::deserialize(deserializer)? {
        None => Ok(None),
        Some(serde_json::Value::Number(number)) => number
            .as_i64()
            .map(Some)
            .ok_or_else(|| D::Error::invalid_value(serde::de::Unexpected::Other("number"), &"i64")),
        Some(serde_json::Value::String(text)) => text
            .trim()
            .parse()
            .map(Some)
            .map_err(|_| D::Error::invalid_value(serde::de::Unexpected::Str(&text), &"i64")),
        Some(_) => Err(D::Error::invalid_type(
            serde::de::Unexpected::Other("not a chat id"),
            &"i64",
        )),
    }
}

/// Everything the SPA may need to display as already configured: only the plaintext fields.
#[derive(Debug, serde::Serialize)]
pub(crate) struct BusinessConfigValues {
    pub jmap_session_url: String,
    pub jmap_username: String,
    pub account_id: Option<String>,
    pub timezone: String,
    pub telegram_chat_id: Option<i64>,
    pub chat_allowlist: Vec<i64>,
    pub llm_enabled: bool,
    pub llm_allow_net: bool,
    pub llm_base_url: Option<String>,
    pub llm_model: Option<String>,
}

/// One presence flag per secret: `true` means the SPA can hide the field and show "saved, click to
/// replace" without ever receiving the value (SAF-NO-SECRET-ECHO). This type is response-only —
/// it is deliberately not deserializable, so a client-supplied `secrets_present` block can never
/// influence what gets stored.
#[derive(Debug, Default, serde::Serialize)]
pub(crate) struct BusinessConfigSecrets {
    pub bot_token: bool,
    pub jmap_password: bool,
    pub telegram_webhook_secret: bool,
    pub reconcile_token: bool,
    pub worker_token: bool,
    pub llm_api_key: bool,
}

/// Read-back shape for `GET /api/business-config`. The response is built from the private wire one
/// field at a time, so no `SecretString` is ever in reach of the JSON encoder — the same pattern the
/// debug surface uses for its `*_configured` flags.
#[derive(Debug, serde::Serialize)]
pub(crate) struct BusinessConfigReadback {
    pub configured: bool,
    pub revision: u64,
    pub values: BusinessConfigValues,
    pub secrets_present: BusinessConfigSecrets,
}

impl BusinessConfigValues {
    /// Empty values reported when nothing has been saved yet. `chat_allowlist` is `[]` and the
    /// optional strings are `null` so the SPA can treat both the same; `timezone` carries the
    /// default because that is the zone the backend would actually render.
    fn empty() -> Self {
        Self {
            jmap_session_url: String::new(),
            jmap_username: String::new(),
            account_id: None,
            timezone: default_timezone(),
            telegram_chat_id: None,
            chat_allowlist: Vec::new(),
            llm_enabled: false,
            llm_allow_net: false,
            llm_base_url: None,
            llm_model: None,
        }
    }
}

impl BusinessConfigReadback {
    /// Read-back for a stored configuration.
    pub(crate) fn from_wire(revision: u64, wire: &BusinessConfigWire) -> Self {
        Self {
            configured: true,
            revision,
            values: BusinessConfigValues {
                jmap_session_url: wire.jmap_session_url.clone(),
                jmap_username: wire.jmap_username.clone(),
                account_id: wire.account_id.clone(),
                timezone: wire.timezone.clone(),
                telegram_chat_id: Some(wire.telegram_chat_id),
                chat_allowlist: wire.chat_allowlist.clone(),
                llm_enabled: wire.llm_enabled,
                llm_allow_net: wire.llm_allow_net,
                llm_base_url: wire.llm_base_url.clone(),
                llm_model: wire.llm_model.clone(),
            },
            secrets_present: BusinessConfigSecrets {
                bot_token: !wire.bot_token.trim().is_empty(),
                jmap_password: !wire.jmap_password.trim().is_empty(),
                telegram_webhook_secret: !wire.telegram_webhook_secret.trim().is_empty(),
                reconcile_token: !wire.reconcile_token.trim().is_empty(),
                worker_token: !wire.worker_token.trim().is_empty(),
                llm_api_key: wire
                    .llm_api_key
                    .as_deref()
                    .is_some_and(|key| !key.trim().is_empty()),
            },
        }
    }

    /// Read-back when nothing has been saved yet. Still 200, so the SPA has one code path.
    pub(crate) fn absent() -> Self {
        Self {
            configured: false,
            revision: 0,
            values: BusinessConfigValues::empty(),
            secrets_present: BusinessConfigSecrets::default(),
        }
    }
}

impl BusinessConfigPatch {
    /// A patch that is a complete configuration on its own, so it can be the very first one.
    ///
    /// The four `Option<Option<String>>` fields are exempt: `None` is a legal stored value for
    /// them. Everything else must be named on a first save, because `BusinessConfigWire` would
    /// either fill one in silently (`timezone`) or fail after the merge is assembled — both of
    /// which would hide that a fragment was submitted where a full configuration was required.
    fn is_complete(&self) -> bool {
        self.bot_token.is_some()
            && self.telegram_chat_id.is_some()
            && self.chat_allowlist.is_some()
            && self.telegram_webhook_secret.is_some()
            && self.timezone.is_some()
            && self.jmap_session_url.is_some()
            && self.jmap_username.is_some()
            && self.jmap_password.is_some()
            && self.llm_enabled.is_some()
            && self.llm_allow_net.is_some()
            && self.reconcile_token.is_some()
            && self.worker_token.is_some()
    }

    /// Merge the patch over the stored configuration.
    ///
    /// Only fields the patch actually names move; everything else keeps its stored value. Secrets
    /// are replace-only, so this never turns a stored secret into a blank one. Returns `None` only
    /// for a fragment submitted before anything is stored — in that case there is nothing to fall
    /// back to, so the handler reports it rather than persisting a partial configuration.
    pub(crate) fn apply(self, stored: Option<&BusinessConfigWire>) -> Option<BusinessConfigWire> {
        if stored.is_none() && !self.is_complete() {
            return None;
        }
        // `stored` is `None` only with a complete patch, where every field below is present, so the
        // fallback is only ever read when it exists.
        let fallback = stored.expect("an omitted field implies a stored configuration exists");
        Some(BusinessConfigWire {
            bot_token: self.bot_token.unwrap_or_else(|| fallback.bot_token.clone()),
            telegram_chat_id: self.telegram_chat_id.unwrap_or(fallback.telegram_chat_id),
            chat_allowlist: self
                .chat_allowlist
                .unwrap_or_else(|| fallback.chat_allowlist.clone()),
            telegram_webhook_secret: self
                .telegram_webhook_secret
                .unwrap_or_else(|| fallback.telegram_webhook_secret.clone()),
            timezone: self.timezone.unwrap_or_else(|| fallback.timezone.clone()),
            jmap_session_url: self
                .jmap_session_url
                .unwrap_or_else(|| fallback.jmap_session_url.clone()),
            jmap_username: self
                .jmap_username
                .unwrap_or_else(|| fallback.jmap_username.clone()),
            jmap_password: self
                .jmap_password
                .unwrap_or_else(|| fallback.jmap_password.clone()),
            account_id: self
                .account_id
                .unwrap_or_else(|| fallback.account_id.clone()),
            llm_enabled: self.llm_enabled.unwrap_or(fallback.llm_enabled),
            llm_allow_net: self.llm_allow_net.unwrap_or(fallback.llm_allow_net),
            llm_api_key: self
                .llm_api_key
                .unwrap_or_else(|| fallback.llm_api_key.clone()),
            llm_base_url: self
                .llm_base_url
                .unwrap_or_else(|| fallback.llm_base_url.clone()),
            llm_model: self.llm_model.unwrap_or_else(|| fallback.llm_model.clone()),
            reconcile_token: self
                .reconcile_token
                .unwrap_or_else(|| fallback.reconcile_token.clone()),
            worker_token: self
                .worker_token
                .unwrap_or_else(|| fallback.worker_token.clone()),
        })
    }
}

impl TryFrom<BusinessConfigWire> for BusinessConfig {
    type Error = BotError;

    fn try_from(value: BusinessConfigWire) -> Result<Self, Self::Error> {
        validate_nonblank("BOT_TOKEN", &value.bot_token)?;
        validate_nonblank("JMAP_SESSION_URL", &value.jmap_session_url)?;
        validate_nonblank("JMAP_USERNAME", &value.jmap_username)?;
        validate_nonblank("JMAP_PASSWORD", &value.jmap_password)?;
        validate_nonblank("TG_WEBHOOK_SECRET", &value.telegram_webhook_secret)?;
        validate_nonblank("RECONCILE_TOKEN", &value.reconcile_token)?;
        validate_nonblank("WORKER_TOKEN", &value.worker_token)?;
        if resolve_timezone(&value.timezone).is_none() {
            return Err(BotError::Config(
                "TIMEZONE must be one of the supported time zones".into(),
            ));
        }
        if let Some(key) = value.llm_api_key.as_deref() {
            validate_nonblank("LLM_API_KEY", key)?;
        }
        Ok(Self {
            bot_token: SecretString::new(value.bot_token),
            telegram_chat_id: value.telegram_chat_id,
            chat_allowlist: value.chat_allowlist,
            telegram_webhook_secret: SecretString::new(value.telegram_webhook_secret),
            jmap_session_url: value.jmap_session_url,
            jmap_username: value.jmap_username,
            jmap_password: SecretString::new(value.jmap_password),
            account_id: value.account_id,
            llm_enabled: value.llm_enabled,
            llm_allow_net: value.llm_allow_net,
            llm_api_key: value.llm_api_key.map(SecretString::new),
            llm_base_url: value.llm_base_url,
            llm_model: value.llm_model,
            reconcile_token: SecretString::new(value.reconcile_token),
            worker_token: SecretString::new(value.worker_token),
            timezone: value.timezone,
        })
    }
}

pub(crate) fn validate_business_wire(wire: BusinessConfigWire) -> Result<(), BotError> {
    let config: BusinessConfig = wire.try_into()?;
    if config.chat_allowlist.is_empty() {
        // SAF-CHAT-ALLOWLIST: 必填，处理前先拒绝非白名单；空名单是非法配置而非 fail-open。
        return Err(BotError::Config(
            "CHAT_ALLOWLIST must contain at least one chat id".into(),
        ));
    }
    let session = url::Url::parse(&config.jmap_session_url)
        .map_err(|_| BotError::Config("JMAP_SESSION_URL must be a valid URL".into()))?;
    if session.scheme() != "https"
        || session.host_str().is_none()
        || session.username() != ""
        || session.password().is_some()
        || session.query().is_some()
        || session.fragment().is_some()
    {
        return Err(BotError::Config(
            "JMAP_SESSION_URL must be HTTPS without credentials or query".into(),
        ));
    }
    if config.llm_enabled {
        let base = config
            .llm_base_url
            .as_deref()
            .ok_or_else(|| BotError::Config("LLM_BASE_URL required when LLM enabled".into()))?;
        if config.llm_api_key.is_none() || config.llm_model.as_deref().is_none() {
            return Err(BotError::Config(
                "LLM credentials required when LLM enabled".into(),
            ));
        }
        let endpoint = url::Url::parse(base)
            .map_err(|_| BotError::Config("LLM_BASE_URL must be a valid URL".into()))?;
        if endpoint.scheme() != "https" {
            return Err(BotError::Config("LLM_BASE_URL must use HTTPS".into()));
        }
    }
    Ok(())
}

impl Config {
    /// Configuration-only startup used before Redis bootstrap. No business route can pass
    /// authentication with these empty sentinels; the process only serves SPA/bootstrap.
    pub fn redis_only() -> Self {
        let empty = || SecretString::new(String::new());
        Self {
            telegram: TelegramConfig {
                bot_token: empty(),
                chat_allowlist: Vec::new(),
                chat_id: 0,
            },
            jmap: JmapConfig {
                session_url: String::new(),
                username: String::new(),
                app_password: empty(),
            },
            account_id: None,
            llm: LlmConfig {
                enabled: false,
                allow_net: false,
                api_key: None,
                base_url: None,
                model: None,
                summary_target_chars: 300,
            },
            auth: AuthSecrets {
                reconcile_token: empty(),
                telegram_webhook_secret: empty(),
            },
            worker_token: empty(),
            timezone: default_timezone(),
        }
    }

    /// Builds the runtime adapter configuration from the Redis business namespace.
    /// This path is used after bootstrap and intentionally does not consult process env.
    pub fn from_business(value: BusinessConfig) -> Self {
        Self {
            telegram: TelegramConfig {
                bot_token: value.bot_token,
                chat_allowlist: value.chat_allowlist,
                chat_id: value.telegram_chat_id,
            },
            jmap: JmapConfig {
                session_url: value.jmap_session_url,
                username: value.jmap_username,
                app_password: value.jmap_password,
            },
            account_id: value.account_id,
            llm: LlmConfig {
                enabled: value.llm_enabled,
                allow_net: value.llm_allow_net,
                api_key: value.llm_api_key,
                base_url: value.llm_base_url,
                model: value.llm_model,
                summary_target_chars: 300,
            },
            auth: AuthSecrets {
                reconcile_token: value.reconcile_token,
                telegram_webhook_secret: value.telegram_webhook_secret,
            },
            worker_token: value.worker_token,
            timezone: value.timezone,
        }
    }

    pub fn from_business_json(value: serde_json::Value) -> Result<Self, BotError> {
        let wire: BusinessConfigWire = serde_json::from_value(value)
            .map_err(|_| BotError::Config("invalid Redis business configuration".into()))?;
        Self::from_business_value(wire)
    }

    fn from_business_value(wire: BusinessConfigWire) -> Result<Self, BotError> {
        Ok(Self::from_business(wire.try_into()?))
    }
}

fn validate_nonblank(name: &str, value: &str) -> Result<(), BotError> {
    if value.trim().is_empty() {
        return Err(BotError::Config(format!("missing or blank {name}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_auth_secret_is_rejected_without_exposing_value() {
        // 断言语义沿用原 required_secret 用例：错误信息点名字段，但绝不回显取值。
        // required_secret 已随遗留 from_env 路径一并删除，改挂到 BusinessConfigWire
        // 这条真实校验路径（validate_nonblank -> TryFrom<BusinessConfigWire>）。
        const AUTH_SECRET_VALUE: &str = "s3cr3t-value-7f3a";
        let wire = BusinessConfigWire {
            bot_token: "bot".into(),
            telegram_chat_id: 1,
            chat_allowlist: vec![1],
            telegram_webhook_secret: "hook".into(),
            timezone: "Asia/Shanghai".into(),
            jmap_session_url: "https://example.invalid".into(),
            jmap_username: "u".into(),
            jmap_password: AUTH_SECRET_VALUE.into(),
            account_id: None,
            llm_enabled: false,
            llm_allow_net: false,
            llm_api_key: None,
            llm_base_url: None,
            llm_model: None,
            reconcile_token: " \t".into(),
            worker_token: "w".into(),
        };
        let error = match BusinessConfig::try_from(wire) {
            Err(error) => error,
            Ok(_) => panic!("blank secret must fail"),
        };
        let message = error.to_string();
        assert!(message.contains("RECONCILE_TOKEN"));
        assert!(!message.contains("secret"));
        assert!(!message.contains(AUTH_SECRET_VALUE));
    }

    #[test]
    fn empty_chat_allowlist_is_rejected() {
        // SAF-CHAT-ALLOWLIST: 空名单是非法配置（Web 侧同样强制至少一项），
        // 不得让绕过 SPA 的直接 PUT 静默写入"拒绝所有 chat"的配置。
        let wire = BusinessConfigWire {
            bot_token: "bot".into(),
            telegram_chat_id: 1,
            chat_allowlist: Vec::new(),
            telegram_webhook_secret: "hook".into(),
            timezone: "Asia/Shanghai".into(),
            jmap_session_url: "https://example.invalid".into(),
            jmap_username: "u".into(),
            jmap_password: "p".into(),
            account_id: None,
            llm_enabled: false,
            llm_allow_net: false,
            llm_api_key: None,
            llm_base_url: None,
            llm_model: None,
            reconcile_token: "r".into(),
            worker_token: "w".into(),
        };
        let error = validate_business_wire(wire).expect_err("empty allowlist must fail");
        assert!(error.to_string().contains("CHAT_ALLOWLIST"));
    }

    #[test]
    fn nonempty_chat_allowlist_passes() {
        let wire = BusinessConfigWire {
            bot_token: "bot".into(),
            telegram_chat_id: 1,
            chat_allowlist: vec![1, 2],
            telegram_webhook_secret: "hook".into(),
            timezone: "Asia/Shanghai".into(),
            jmap_session_url: "https://example.invalid".into(),
            jmap_username: "u".into(),
            jmap_password: "p".into(),
            account_id: None,
            llm_enabled: false,
            llm_allow_net: false,
            llm_api_key: None,
            llm_base_url: None,
            llm_model: None,
            reconcile_token: "r".into(),
            worker_token: "w".into(),
        };
        validate_business_wire(wire).expect("non-empty allowlist must pass");
    }

    #[test]
    fn required_config_values_reject_blank_without_exposing_values() {
        for name in [
            "BOT_TOKEN",
            "JMAP_SESSION_URL",
            "JMAP_USERNAME",
            "JMAP_PASSWORD",
            "REDIS_URL",
        ] {
            let error = validate_nonblank(name, " \t").expect_err("blank must fail");
            assert_eq!(
                error.to_string(),
                format!("configuration error: missing or blank {name}")
            );
        }
    }

    /// A zone outside the table fails validation with the exact message the SPA
    /// will surface.
    #[test]
    fn unsupported_timezone_is_rejected() {
        let wire = BusinessConfigWire {
            bot_token: "bot".into(),
            telegram_chat_id: 1,
            chat_allowlist: vec![1],
            telegram_webhook_secret: "hook".into(),
            timezone: "Europe/London".into(),
            jmap_session_url: "https://example.invalid".into(),
            jmap_username: "u".into(),
            jmap_password: "p".into(),
            account_id: None,
            llm_enabled: false,
            llm_allow_net: false,
            llm_api_key: None,
            llm_base_url: None,
            llm_model: None,
            reconcile_token: "r".into(),
            worker_token: "w".into(),
        };
        let error = validate_business_wire(wire).expect_err("unsupported zone must fail");
        assert_eq!(
            error.to_string(),
            "configuration error: TIMEZONE must be one of the supported time zones"
        );
    }

    /// Every listed zone must validate.
    #[test]
    fn supported_timezones_are_accepted() {
        for (iana, _, offset) in SUPPORTED_TIMEZONES {
            let wire = BusinessConfigWire {
                bot_token: "bot".into(),
                telegram_chat_id: 1,
                chat_allowlist: vec![1],
                telegram_webhook_secret: "hook".into(),
                timezone: (*iana).to_owned(),
                jmap_session_url: "https://example.invalid".into(),
                jmap_username: "u".into(),
                jmap_password: "p".into(),
                account_id: None,
                llm_enabled: false,
                llm_allow_net: false,
                llm_api_key: None,
                llm_base_url: None,
                llm_model: None,
                reconcile_token: "r".into(),
                worker_token: "w".into(),
            };
            validate_business_wire(wire)
                .unwrap_or_else(|e| panic!("{iana} ({offset}) must validate: {e:?}"));
        }

        assert_eq!(default_timezone(), "Asia/Shanghai");
        assert_eq!(parse_offset_seconds("+08:00"), Some(8 * 3600));
        assert_eq!(parse_offset_seconds("+05:30"), Some(5 * 3600 + 30 * 60));
        assert_eq!(parse_offset_seconds("+00:00"), Some(0));
        assert_eq!(parse_offset_seconds("-04:00"), Some(-4 * 3600));
        assert!(parse_offset_seconds("not-an-offset").is_none());
    }

    /// Persisted configs written before this field existed must still load, with
    /// the default zone applied rather than a deserialization error.
    #[test]
    fn missing_timezone_defaults_to_shanghai() {
        let value: BusinessConfigWire = serde_json::from_value(serde_json::json!({
            "bot_token": "bot",
            "telegram_chat_id": 1,
            "chat_allowlist": [1],
            "telegram_webhook_secret": "hook",
            "jmap_session_url": "https://example.invalid",
            "jmap_username": "u",
            "jmap_password": "p",
            "llm_enabled": false,
            "llm_allow_net": false,
            "reconcile_token": "r",
            "worker_token": "w",
        }))
        .expect("a config without timezone must deserialize");

        assert_eq!(value.timezone, "Asia/Shanghai");
        validate_business_wire(value.clone()).expect("a defaulted zone must validate");
        let config: BusinessConfig = value.try_into().expect("a valid wire must convert");
        assert_eq!(config.timezone, "Asia/Shanghai");
        assert_eq!(
            resolve_timezone(&config.timezone),
            Some(default_timezone_offset())
        );
    }

    #[test]
    fn timezone_survives_the_wire_roundtrip() {
        let config = BusinessConfig {
            bot_token: secrecy::SecretString::new("bot".into()),
            telegram_chat_id: 1,
            chat_allowlist: vec![1],
            telegram_webhook_secret: secrecy::SecretString::new("hook".into()),
            timezone: "Asia/Kolkata".into(),
            jmap_session_url: "https://example.invalid".into(),
            jmap_username: "u".into(),
            jmap_password: secrecy::SecretString::new("p".into()),
            account_id: None,
            llm_enabled: false,
            llm_allow_net: false,
            llm_api_key: None,
            llm_base_url: None,
            llm_model: None,
            reconcile_token: secrecy::SecretString::new("r".into()),
            worker_token: secrecy::SecretString::new("w".into()),
        };

        let wire = BusinessConfigWire::from(config);
        assert_eq!(wire.timezone, "Asia/Kolkata");
        assert_eq!(
            serde_json::to_value(&wire).unwrap()["timezone"],
            "Asia/Kolkata"
        );
    }

    /// Pure deserialisation check for `de_i64_from_number_or_string`, with no router, no store and
    /// no other field present. This is the only place that can tell "the field's custom
    /// deserializer refuses the string" apart from a later validation step, which is a real
    /// distinction: the SPA submits `telegram_chat_id` as a JSON string on a partial submit while
    /// the read-back model returns it as a JSON number.
    #[test]
    fn business_config_patch_reads_a_chat_id_from_a_number_or_a_numeric_string() {
        let parse = |body: serde_json::Value| -> Result<BusinessConfigPatch, serde_json::Error> {
            serde_json::from_value(body)
        };

        assert_eq!(
            parse(serde_json::json!({"telegram_chat_id": -987654321i64}))
                .unwrap()
                .telegram_chat_id,
            Some(-987654321)
        );
        assert_eq!(
            parse(serde_json::json!({"telegram_chat_id": "-987654321"}))
                .unwrap()
                .telegram_chat_id,
            Some(-987654321)
        );
        // A string that does not parse to an i64 is rejected, not truncated to zero.
        assert!(parse(serde_json::json!({"telegram_chat_id": "not-a-chat-id"})).is_err());
        assert!(parse(serde_json::json!({"telegram_chat_id": 1.5})).is_err());
        assert!(parse(serde_json::json!({"telegram_chat_id": "true"})).is_err());

        // Absent means "keep what is stored", which the patch merge relies on.
        let missing = parse(serde_json::json!({})).unwrap();
        assert!(missing.telegram_chat_id.is_none());
        assert!(parse(serde_json::json!({"timezone": "Etc/UTC"}))
            .unwrap()
            .telegram_chat_id
            .is_none());
    }
}
