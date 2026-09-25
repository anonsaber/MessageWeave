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
    pub port: u16,
    pub run_mode: String,
    pub telegram: TelegramConfig,
    pub jmap: JmapConfig,
    pub redis_url: SecretString,
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
    pub jmap_push_verification: SecretString,
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
    pub jmap_push_verification: SecretString,
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
    pub jmap_push_verification: String,
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
            jmap_push_verification: value.jmap_push_verification.expose_secret().to_owned(),
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
        validate_nonblank("JMAP_PUSH_VERIFICATION", &value.jmap_push_verification)?;
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
            jmap_push_verification: SecretString::new(value.jmap_push_verification),
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
    pub fn redis_only(redis_url: SecretString) -> Self {
        let empty = || SecretString::new(String::new());
        Self {
            port: 8080,
            run_mode: "webhook".into(),
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
            redis_url,
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
                jmap_push_verification: empty(),
            },
            worker_token: empty(),
        }
    }

    /// Builds the runtime adapter configuration from the Redis business namespace.
    /// This path is used after bootstrap and intentionally does not consult process env.
    pub fn from_business(redis_url: SecretString, value: BusinessConfig) -> Self {
        Self {
            port: 8080,
            run_mode: "webhook".into(),
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
            redis_url,
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
                jmap_push_verification: value.jmap_push_verification,
            },
            worker_token: value.worker_token,
        }
    }

    pub fn from_business_json(
        redis_url: SecretString,
        value: serde_json::Value,
    ) -> Result<Self, BotError> {
        let wire: BusinessConfigWire = serde_json::from_value(value)
            .map_err(|_| BotError::Config("invalid Redis business configuration".into()))?;
        Self::from_business_value(redis_url, wire)
    }

    fn from_business_value(
        redis_url: SecretString,
        wire: BusinessConfigWire,
    ) -> Result<Self, BotError> {
        Ok(Self::from_business(redis_url, wire.try_into()?))
    }

    pub fn from_env() -> Result<Self, BotError> {
        let port = std::env::var("PORT")
            .unwrap_or_else(|_| "8080".into())
            .parse()
            .map_err(|_| BotError::Config("PORT must be a valid u16".into()))?;
        let allowlist = std::env::var("CHAT_ALLOWLIST")
            .or_else(|_| std::env::var("TELEGRAM_CHAT_ID"))
            .unwrap_or_default()
            .split(',')
            .filter(|v| !v.trim().is_empty())
            .map(|v| v.trim().parse::<i64>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| BotError::Config("CHAT_ALLOWLIST must contain integer IDs".into()))?;
        let enabled = env_bool("LLM_ENABLED", false)?;
        let llm = LlmConfig {
            enabled,
            allow_net: env_bool("LLM_ALLOW_NET", false)?,
            api_key: std::env::var("LLM_API_KEY").ok().map(SecretString::new),
            base_url: std::env::var("LLM_BASE_URL").ok(),
            model: std::env::var("LLM_MODEL").ok(),
            summary_target_chars: std::env::var("LLM_SUMMARY_TARGET_CHARS")
                .unwrap_or_else(|_| "300".into())
                .parse()
                .map_err(|_| {
                    BotError::Config("LLM_SUMMARY_TARGET_CHARS must be an integer".into())
                })?,
        };
        if enabled && (llm.api_key.is_none() || llm.base_url.is_none() || llm.model.is_none()) {
            return Err(BotError::Config(
                "LLM_ENABLED requires LLM_API_KEY, LLM_BASE_URL and LLM_MODEL".into(),
            ));
        }
        Ok(Self {
            port,
            run_mode: std::env::var("RUN_MODE").unwrap_or_else(|_| "webhook".into()),
            telegram: TelegramConfig {
                bot_token: required_secret("BOT_TOKEN")?,
                chat_allowlist: allowlist,
                chat_id: std::env::var("TELEGRAM_CHAT_ID")
                    .map_err(|_| BotError::Config("missing TELEGRAM_CHAT_ID".into()))?
                    .parse()
                    .map_err(|_| BotError::Config("TELEGRAM_CHAT_ID must be an integer".into()))?,
            },
            jmap: JmapConfig {
                session_url: required_nonblank("JMAP_SESSION_URL")?,
                username: required_nonblank("JMAP_USERNAME")?,
                app_password: required_secret("JMAP_PASSWORD")?,
            },
            redis_url: required_secret("REDIS_URL")?,
            account_id: std::env::var("ACCOUNT_ID").ok().filter(|v| !v.is_empty()),
            llm,
            auth: AuthSecrets {
                reconcile_token: required_secret("RECONCILE_TOKEN")?,
                telegram_webhook_secret: required_secret("TG_WEBHOOK_SECRET")?,
                jmap_push_verification: required_secret("JMAP_PUSH_VERIFICATION")?,
            },
            worker_token: required_secret("WORKER_TOKEN")?,
        })
    }

    /// Exposes a secret only to the client adapter at the point of use; never log this value.
    #[expect(
        dead_code,
        reason = "稳定ID+阶段0占位：JMAP adapter 后续使用密钥访问器"
    )]
    pub fn jmap_password(&self) -> &str {
        self.jmap.app_password.expose_secret()
    }
}

fn required_nonblank(name: &str) -> Result<String, BotError> {
    let value = std::env::var(name).map_err(|_| BotError::Config(format!("missing {name}")))?;
    validate_nonblank(name, value.as_str())?;
    Ok(value)
}

fn required_secret(name: &str) -> Result<SecretString, BotError> {
    let value = required_nonblank(name)?;
    Ok(SecretString::new(value))
}

fn validate_nonblank(name: &str, value: &str) -> Result<(), BotError> {
    if value.trim().is_empty() {
        return Err(BotError::Config(format!("missing or blank {name}")));
    }
    Ok(())
}

fn env_bool(name: &str, default: bool) -> Result<bool, BotError> {
    match std::env::var(name).ok().as_deref() {
        None => Ok(default),
        Some("true" | "1" | "yes") => Ok(true),
        Some("false" | "0" | "no") => Ok(false),
        Some(_) => Err(BotError::Config(format!("{name} must be boolean"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_auth_secret_is_rejected_without_exposing_value() {
        let error = required_secret("S1_TEST_AUTH_SECRET").expect_err("unset must fail");
        assert!(error.to_string().contains("S1_TEST_AUTH_SECRET"));
        assert!(!error.to_string().contains("secret"));
        std::env::set_var("S1_TEST_AUTH_SECRET", " \t");
        let error = required_secret("S1_TEST_AUTH_SECRET").expect_err("blank must fail");
        assert!(error.to_string().contains("S1_TEST_AUTH_SECRET"));
        std::env::remove_var("S1_TEST_AUTH_SECRET");
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

    #[test]
    fn startup_rejects_blank_auth_secret() {
        for (name, value) in [
            ("BOT_TOKEN", "bot"),
            ("JMAP_SESSION_URL", "https://mail.invalid/.well-known/jmap"),
            ("JMAP_USERNAME", "user"),
            ("JMAP_PASSWORD", "password"),
            ("REDIS_URL", "redis://invalid"),
            ("TG_WEBHOOK_SECRET", "telegram"),
            ("JMAP_PUSH_VERIFICATION", "verification"),
            ("TELEGRAM_CHAT_ID", "123"),
        ] {
            std::env::set_var(name, value);
        }
        std::env::set_var("RECONCILE_TOKEN", " \t");
        let error = match Config::from_env() {
            Err(error) => error,
            Ok(_) => panic!("blank auth secret must fail startup"),
        };
        assert_eq!(
            error.to_string(),
            "configuration error: missing or blank RECONCILE_TOKEN"
        );
        for name in [
            "BOT_TOKEN",
            "JMAP_SESSION_URL",
            "JMAP_USERNAME",
            "JMAP_PASSWORD",
            "REDIS_URL",
            "RECONCILE_TOKEN",
            "TG_WEBHOOK_SECRET",
            "JMAP_PUSH_VERIFICATION",
            "TELEGRAM_CHAT_ID",
        ] {
            std::env::remove_var(name);
        }
    }
}
