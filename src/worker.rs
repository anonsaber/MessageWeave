//! Bounded metadata notification worker (MOD-TELEGRAM-NOTIFY).
use crate::ai::LlmClient;
use crate::channel::telegram::TelegramClient;
use crate::config::BusinessConfig;
use crate::domain::jmap::{JmapBackend, JmapService};
use crate::domain::Notification;
use crate::state::ReliableState;
use async_trait::async_trait;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, RwLock},
};

#[async_trait]
pub trait WorkerHandler: Send + Sync {
    async fn process(&self, stream: &str, payload: &str) -> Result<(), ()>;
}

/// Atomically replaceable worker handle. In-flight requests keep their old worker;
/// subsequent bounded requests observe the newest successfully-built instance.
pub struct WorkerHandle {
    current: RwLock<Arc<dyn WorkerHandler>>,
}

impl WorkerHandle {
    pub fn new(worker: Arc<dyn WorkerHandler>) -> Self {
        Self {
            current: RwLock::new(worker),
        }
    }

    pub fn replace(&self, worker: Arc<dyn WorkerHandler>) -> Result<(), ()> {
        self.current
            .write()
            .map(|mut current| *current = worker)
            .map_err(|_| ())
    }
}

/// Coordinates an all-or-nothing worker/config swap. The caller builds every client before
/// invoking `reload`; a failed factory therefore leaves both the old worker and snapshot intact.
pub struct ReloadCoordinator {
    worker: Arc<WorkerHandle>,
    config: RwLock<Option<BusinessConfig>>,
}

impl ReloadCoordinator {
    pub fn new(worker: Arc<WorkerHandle>, config: Option<BusinessConfig>) -> Self {
        Self {
            worker,
            config: RwLock::new(config),
        }
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "stable reload API retained for coordinator tests")
    )]
    pub async fn reload_async<F>(&self, config: BusinessConfig, factory: F) -> Result<(), ()>
    where
        F: FnOnce(
            &BusinessConfig,
        )
            -> Pin<Box<dyn Future<Output = Result<Arc<dyn WorkerHandler>, ()>> + Send>>,
    {
        let next = factory(&config).await?;
        self.worker.replace(next)?;
        *self.config.write().map_err(|_| ())? = Some(config);
        Ok(())
    }

    /// Commit an already-built worker and its matching snapshot. Construction must happen
    /// before this method so failed client creation cannot disturb the active instance.
    pub fn commit(&self, config: BusinessConfig, worker: Arc<dyn WorkerHandler>) -> Result<(), ()> {
        self.worker.replace(worker)?;
        *self.config.write().map_err(|_| ())? = Some(config);
        Ok(())
    }
}

#[async_trait]
impl WorkerHandler for WorkerHandle {
    async fn process(&self, stream: &str, payload: &str) -> Result<(), ()> {
        let worker = self.current.read().map_err(|_| ())?.clone();
        worker.process(stream, payload).await
    }
}

pub struct NoopWorker;
#[async_trait]
impl WorkerHandler for NoopWorker {
    async fn process(&self, _stream: &str, _payload: &str) -> Result<(), ()> {
        Err(())
    }
}

/// JMAP bodies are read only to obtain metadata; body text never reaches Telegram.
pub struct MetadataWorker<B> {
    jmap: JmapService<B>,
    telegram: Arc<TelegramClient>,
    chat_id: i64,
    state: Arc<dyn ReliableState>,
    llm: Option<Arc<LlmClient>>,
}

impl<B: JmapBackend> MetadataWorker<B> {
    pub fn new(
        jmap: JmapService<B>,
        telegram: TelegramClient,
        chat_id: i64,
        state: Arc<dyn ReliableState>,
        llm: Option<Arc<LlmClient>>,
    ) -> Self {
        Self {
            jmap,
            telegram: Arc::new(telegram),
            chat_id,
            state,
            llm,
        }
    }
}

#[async_trait]
impl<B: JmapBackend> WorkerHandler for MetadataWorker<B> {
    async fn process(&self, stream: &str, payload: &str) -> Result<(), ()> {
        if stream == "stalwart:telegram" {
            return self.process_telegram(payload).await;
        }
        if stream != "stalwart:jmap" {
            return Ok(());
        }
        let event: WorkerEvent = serde_json::from_str(payload).map_err(|_| ())?;
        let (Some(account), Some(email)) = (event.account_id.as_deref(), event.email_id.as_deref())
        else {
            return Err(());
        };
        if account != self.jmap.account_id() {
            return Err(());
        }
        let content = self
            .jmap
            .read_email(email)
            .await
            .map_err(|_| ())?
            .ok_or(())?;
        let metadata = content.metadata;
        self.telegram
            .send_notification(
                self.chat_id,
                &Notification {
                    sender: metadata.sender.unwrap_or_else(|| "unknown".into()),
                    subject: metadata.subject.unwrap_or_else(|| "(no subject)".into()),
                    received_at: metadata
                        .received_at
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "unknown".into()),
                },
            )
            .await
            .map_err(|_| ())
    }
}

impl<B: JmapBackend> MetadataWorker<B> {
    async fn process_telegram(&self, payload: &str) -> Result<(), ()> {
        let update: TelegramUpdate = serde_json::from_str(payload).map_err(|_| ())?;
        let Some(message) = update.message else {
            return Ok(());
        };
        let Some(text) = message.text.as_deref() else {
            return Ok(());
        };
        let intent = parse_intent(text);
        if matches!(&intent, Intent::Help) {
            return self
                .telegram
                .send_text(message.chat.id, help_message())
                .await
                .map_err(|_| ());
        }
        if let Intent::Consent { ttl, label } = &intent {
            if *ttl == 0 {
                self.state
                    .clear_ai_consent(message.chat.id)
                    .await
                    .map_err(|_| ())?;
                return self
                    .telegram
                    .send_text(message.chat.id, "AI摘要授权已撤销")
                    .await
                    .map_err(|_| ());
            }
            self.state
                .set_ai_consent(message.chat.id, *ttl)
                .await
                .map_err(|_| ())?;
            return self
                .telegram
                .send_text(
                    message.chat.id,
                    &format!(
                        "AI摘要授权已开启：{}，到期时间 Unix {}；仍需使用 /summary <email_id> 请求",
                        label,
                        unix_now().saturating_add(*ttl as i64)
                    ),
                )
                .await
                .map_err(|_| ());
        }
        let Some(email_id) = intent.email_id() else {
            if matches!(&intent, Intent::Query) {
                self.telegram
                    .send_text(message.chat.id, "请提供邮件 ID，例如：摘要 e-123")
                    .await
                    .map_err(|_| ())?;
            }
            return Ok(());
        };
        let content = self
            .jmap
            .read_email(&email_id)
            .await
            .map_err(|_| ())?
            .ok_or(())?;
        let allowed = self
            .state
            .ai_consent_until(message.chat.id)
            .await
            .map_err(|_| ())?
            .is_some();
        let summary = if allowed {
            match &self.llm {
                Some(llm) => llm
                    .summarize(&content.text)
                    .await
                    .unwrap_or_else(|_| fallback(&content.text)),
                None => fallback(&content.text),
            }
        } else {
            format!(
                "{} — {}\n授权已到期或尚未授权；如需 AI 摘要，请重新选择授权期限。",
                content.metadata.sender.unwrap_or_default(),
                content.metadata.subject.unwrap_or_default()
            )
        };
        self.telegram
            .send_text(message.chat.id, &summary)
            .await
            .map_err(|_| ())
    }
}

fn fallback(body: &str) -> String {
    body.chars().take(300).collect()
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}

#[derive(Debug, PartialEq, Eq)]
enum Intent {
    Help,
    Consent { ttl: u64, label: &'static str },
    Summary(String),
    Query,
    Unknown,
}

impl Intent {
    fn email_id(&self) -> Option<String> {
        match self {
            Self::Summary(id) => Some(id.clone()),
            _ => None,
        }
    }
}

fn parse_intent(input: &str) -> Intent {
    let text = input.trim().to_ascii_lowercase();
    if text == "/help" || text == "help" || text.contains("帮助") || text.contains("怎么用") {
        return Intent::Help;
    }
    if text.contains("临时") || text.contains("一次") {
        return Intent::Consent {
            ttl: 3600,
            label: "临时1小时",
        };
    }
    if text.contains("今天") {
        return Intent::Consent {
            ttl: 86_400,
            label: "今天",
        };
    }
    if text.contains("7天") {
        return Intent::Consent {
            ttl: 7 * 86_400,
            label: "7天",
        };
    }
    if text.contains("直到我撤销") || text.contains("长期") {
        return Intent::Consent {
            ttl: 365 * 86_400,
            label: "直到撤销（最长365天）",
        };
    }
    if text == "/ai on"
        || text == "/ai yes"
        || text.contains("开启 ai")
        || text.contains("同意摘要")
        || text.contains("允许 ai")
    {
        return Intent::Consent {
            ttl: 3600,
            label: "1小时",
        };
    }
    if text == "/ai off"
        || text.contains("关闭 ai")
        || text.contains("撤销授权")
        || text.contains("停止摘要")
    {
        return Intent::Consent {
            ttl: 0,
            label: "已撤销",
        };
    }
    let asks_summary =
        text.starts_with("/summary") || text.contains("摘要") || text.contains("总结");
    let asks_query =
        text.contains("查询邮件") || text.contains("查看邮件") || text.contains("查邮件");
    let id = text.split_whitespace().last().filter(|id| {
        id.len() > 1
            && id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    });
    if asks_summary {
        return id
            .map(|v| Intent::Summary(v.to_owned()))
            .unwrap_or(Intent::Query);
    }
    if asks_query {
        return id
            .map(|v| Intent::Summary(v.to_owned()))
            .unwrap_or(Intent::Query);
    }
    Intent::Unknown
}

fn help_message() -> &'static str {
    "使用说明：\n邮件到达后会先发送发件人、主题和时间等元数据通知。\n\n授权示例：/ai on（1小时）、临时一次、1小时、今天、7天、直到我撤销。\n/ai off：立即撤销 AI 正文授权。\n/summary <email_id>：请求指定邮件摘要；未授权或授权到期时只返回元数据并提示重新授权。\n/help（或 help）：显示本说明。\n\n隐私：AI 默认关闭，只有你明确开启后才会发送正文；授权有期限且不会自动续期；正文和 AI 结果不会持久化，也不会写入日志。\n如果未配置 AI 或 AI 调用失败，将回退为本地截取摘要。"
}

#[derive(serde::Deserialize)]
struct TelegramUpdate {
    #[serde(default)]
    message: Option<TelegramMessage>,
}
#[derive(serde::Deserialize)]
struct TelegramMessage {
    chat: TelegramChat,
    #[serde(default)]
    text: Option<String>,
}
#[derive(serde::Deserialize)]
struct TelegramChat {
    id: i64,
}

#[cfg(test)]
mod tests {
    use super::{help_message, parse_intent, Intent};

    #[test]
    fn help_is_safe_and_actionable() {
        let text = help_message();
        for command in ["/ai on", "/ai off", "/summary <email_id>", "/help"] {
            assert!(text.contains(command));
        }
        assert!(text.contains("默认关闭"));
        assert!(text.contains("不会持久化"));
        assert!(!text.contains("BOT_TOKEN"));
    }

    #[test]
    fn natural_language_intents_keep_commands_compatible() {
        assert_eq!(parse_intent("请帮我看看怎么用"), Intent::Help);
        assert!(matches!(
            parse_intent("我同意摘要"),
            Intent::Consent { ttl: 3600, .. }
        ));
        assert!(matches!(
            parse_intent("关闭 AI"),
            Intent::Consent { ttl: 0, .. }
        ));
        assert!(matches!(
            parse_intent("今天允许 AI"),
            Intent::Consent { ttl: 86_400, .. }
        ));
        assert!(matches!(
            parse_intent("授权 7天"),
            Intent::Consent { ttl: 604_800, .. }
        ));
        assert!(matches!(
            parse_intent("直到我撤销"),
            Intent::Consent {
                ttl: 31_536_000,
                ..
            }
        ));
        assert_eq!(
            parse_intent("请总结 e-123"),
            Intent::Summary("e-123".into())
        );
        assert_eq!(parse_intent("查询邮件"), Intent::Query);
    }
}

#[derive(serde::Deserialize)]
struct WorkerEvent {
    #[serde(rename = "accountId")]
    account_id: Option<String>,
    #[serde(rename = "emailId")]
    email_id: Option<String>,
}

#[cfg(test)]
mod reload_tests {
    use super::*;
    use secrecy::SecretString;

    fn config() -> BusinessConfig {
        BusinessConfig {
            bot_token: SecretString::new("bot".into()),
            telegram_chat_id: 1,
            chat_allowlist: vec![],
            telegram_webhook_secret: SecretString::new("hook".into()),
            jmap_session_url: "https://mail.example.test".into(),
            jmap_username: "user".into(),
            jmap_password: SecretString::new("pass".into()),
            jmap_push_verification: SecretString::new("verify".into()),
            account_id: None,
            llm_enabled: false,
            llm_allow_net: false,
            llm_api_key: None,
            llm_base_url: None,
            llm_model: None,
            reconcile_token: SecretString::new("reconcile".into()),
            worker_token: SecretString::new("worker".into()),
        }
    }

    #[tokio::test]
    async fn failed_build_keeps_existing_worker() {
        let handle = Arc::new(WorkerHandle::new(Arc::new(NoopWorker)));
        let coordinator = ReloadCoordinator::new(handle.clone(), Some(config()));
        let result = coordinator
            .reload_async(config(), |_| Box::pin(async { Err(()) }))
            .await;
        assert!(result.is_err());
        assert!(handle.process("x", "y").await.is_err());
    }

    #[tokio::test]
    async fn successful_build_replaces_worker_atomically() {
        let handle = Arc::new(WorkerHandle::new(Arc::new(NoopWorker)));
        let coordinator = ReloadCoordinator::new(handle.clone(), None);
        let replacement: Arc<dyn WorkerHandler> = Arc::new(OkWorker);
        coordinator
            .reload_async(config(), move |_| {
                let replacement = replacement.clone();
                Box::pin(async move { Ok(replacement) })
            })
            .await
            .unwrap();
        assert!(handle.process("x", "y").await.is_ok());
    }

    struct OkWorker;
    #[async_trait]
    impl WorkerHandler for OkWorker {
        async fn process(&self, _stream: &str, _payload: &str) -> Result<(), ()> {
            Ok(())
        }
    }
}
