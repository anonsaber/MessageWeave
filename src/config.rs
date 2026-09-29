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

/// Runtime-only settings. SecretString prevents accidental formatting/logging of credentials.
/// C-AUTH-APP-BASIC and REQ-SINGLE-ACCOUNT are intentionally represented explicitly.
pub struct Config {
    pub telegram: TelegramConfig,
    pub jmap: JmapConfig,
    pub account_id: Option<String>,
    pub llm: LlmConfig,
    pub auth: AuthSecrets,
    pub worker_token: SecretString,
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
}
