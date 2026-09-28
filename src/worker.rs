//! Bounded metadata notification worker (MOD-TELEGRAM-NOTIFY).
use crate::ai::LlmClient;
use crate::channel::telegram::TelegramClient;
use crate::config::BusinessConfig;
use crate::domain::jmap::{JmapBackend, JmapService, SearchResult};
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

    async fn create_push_subscription(&self, _callback_url: &str) -> Result<String, ()> {
        Err(())
    }

    async fn destroy_push_subscription(&self, _subscription_id: &str) -> Result<(), ()> {
        Err(())
    }

    async fn verify_push_subscription(
        &self,
        _subscription_id: &str,
        _verification_code: &str,
    ) -> Result<(), ()> {
        Err(())
    }

    /// Performs one bounded JMAP reconciliation pass. Implementations must not
    /// persist the cursor; the HTTP coordinator commits it after enqueueing.
    async fn reconcile(
        &self,
        _since_state: Option<&str>,
        _max_changes: usize,
    ) -> Result<String, ()> {
        Err(())
    }
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
    async fn destroy_push_subscription(&self, subscription_id: &str) -> Result<(), ()> {
        let worker = self.current.read().map_err(|_| ())?.clone();
        worker.destroy_push_subscription(subscription_id).await
    }

    async fn create_push_subscription(&self, callback_url: &str) -> Result<String, ()> {
        let worker = self.current.read().map_err(|_| ())?.clone();
        worker.create_push_subscription(callback_url).await
    }

    async fn verify_push_subscription(
        &self,
        subscription_id: &str,
        verification_code: &str,
    ) -> Result<(), ()> {
        let worker = self.current.read().map_err(|_| ())?.clone();
        worker
            .verify_push_subscription(subscription_id, verification_code)
            .await
    }

    async fn process(&self, stream: &str, payload: &str) -> Result<(), ()> {
        let worker = self.current.read().map_err(|_| ())?.clone();
        worker.process(stream, payload).await
    }

    async fn reconcile(&self, since_state: Option<&str>, max_changes: usize) -> Result<String, ()> {
        let worker = self.current.read().map_err(|_| ())?.clone();
        worker.reconcile(since_state, max_changes).await
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
    async fn destroy_push_subscription(&self, subscription_id: &str) -> Result<(), ()> {
        self.jmap
            .destroy_push_subscription(subscription_id)
            .await
            .map_err(|_| ())
    }

    async fn create_push_subscription(&self, callback_url: &str) -> Result<String, ()> {
        self.jmap
            .create_push_subscription(callback_url)
            .await
            .map_err(|_| ())
    }

    async fn verify_push_subscription(
        &self,
        subscription_id: &str,
        verification_code: &str,
    ) -> Result<(), ()> {
        self.jmap
            .verify_push_subscription(subscription_id, verification_code)
            .await
            .map_err(|_| ())
    }

    async fn process(&self, stream: &str, payload: &str) -> Result<(), ()> {
        if stream == "stalwart:telegram" {
            return self.process_telegram(payload).await;
        }
        if stream != "stalwart:jmap" {
            // Unknown stream: fail closed. Returning Ok would let the coordinator
            // write `delivery:committed` + XACK and silently drop an event we do
            // not understand. Falling through to Err routes it to retry/DLQ so it
            // stays observable instead of vanishing.
            return Err(());
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

    async fn reconcile(&self, since_state: Option<&str>, max_changes: usize) -> Result<String, ()> {
        let max_changes = max_changes.max(1);
        const MAX_BASELINE_PAGES: usize = 100;
        const MAX_BASELINE_EMAILS: usize = 10_000;
        const RECONCILE_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);
        let started = tokio::time::Instant::now();
        let (mut cursor, mut baseline_position) = if let Some(state) = since_state {
            if let Some((baseline, position)) = decode_baseline_cursor(state) {
                (baseline, position)
            } else {
                (state.to_owned(), usize::MAX)
            }
        } else {
            // Capture a stable baseline before paging. Changes occurring while
            // the listing is in flight are consumed below before committing.
            (self.jmap.current_state().await.map_err(|_| ())?, 0)
        };

        if baseline_position != usize::MAX {
            let mut pages = 0_usize;
            let mut emails = 0_usize;
            loop {
                if pages >= MAX_BASELINE_PAGES
                    || emails >= MAX_BASELINE_EMAILS
                    || started.elapsed() >= RECONCILE_BUDGET
                {
                    return if pages == 0 {
                        Err(())
                    } else {
                        Ok(encode_baseline_cursor(&cursor, baseline_position))
                    };
                }
                let page = self
                    .jmap
                    .list_emails_page(None, baseline_position, 100)
                    .await
                    .map_err(|_| ())?;
                if page.is_empty() {
                    break;
                }
                pages += 1;
                emails = emails.saturating_add(page.len());
                baseline_position = baseline_position.saturating_add(page.len());
                for email in page {
                    self.enqueue_reconcile_event(&email.id).await?;
                }
            }
        }

        // RFC 8620 lets a server answer `/changes` with `hasMoreChanges=true`
        // when more events remain than `maxChanges` allowed. The spec-blessed
        // continuation is `sinceState = newState`; but a server that resolves
        // `newState` to "after *all* pending changes" would then hand us a
        // cursor that silently skips the unreturned batch. We cannot pass
        // `upToId` here (jmap-client 0.4.2 does not expose it), so while more
        // changes remain we keep the *same* sinceState and widen the window
        // instead. The 24h dedup key makes re-reading the superset idempotent,
        // so this is at-most-duplicate, never-loss. Only when the window stops
        // growing (server-side cap) do we fall back to advancing to `new_state`
        // to guarantee forward progress.
        const CHANGE_WINDOW_CAP: usize = 4_096;
        let mut window = max_changes;
        let mut pages = 0_usize;
        loop {
            if pages >= 100 || started.elapsed() >= RECONCILE_BUDGET {
                // Every event from the last completed page is already enqueued.
                // Returning that cursor lets the next invocation continue from
                // it instead of retrying the same bounded window forever.
                return if pages == 0 { Err(()) } else { Ok(cursor) };
            }
            pages += 1;
            let changes = self
                .jmap
                .email_changes(&cursor, window)
                .await
                .map_err(|_| ())?;
            for email_id in changes.created.iter().chain(changes.updated.iter()) {
                self.enqueue_reconcile_event(email_id).await?;
            }
            if !changes.has_more {
                return Ok(changes.new_state);
            }
            if window < CHANGE_WINDOW_CAP {
                window = window.saturating_mul(2).min(CHANGE_WINDOW_CAP);
                continue;
            }
            // Window is already at the cap and the server still reports more:
            // advancing is the only way to make progress.
            cursor = changes.new_state;
        }
    }
}

fn encode_baseline_cursor(state: &str, position: usize) -> String {
    let encoded = state
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("baseline:{encoded}:{position}")
}

fn decode_baseline_cursor(value: &str) -> Option<(String, usize)> {
    let rest = value.strip_prefix("baseline:")?;
    let (encoded, position) = rest.rsplit_once(':')?;
    let bytes = (0..encoded.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(encoded.get(index..index + 2)?, 16).ok())
        .collect::<Option<Vec<_>>>()?;
    Some((String::from_utf8(bytes).ok()?, position.parse().ok()?))
}

impl<B: JmapBackend> MetadataWorker<B> {
    async fn enqueue_reconcile_event(&self, email_id: &str) -> Result<(), ()> {
        let key = format!("dedup:jmap:{}:{}", self.jmap.account_id(), email_id);
        let payload = serde_json::json!({
            "accountId": self.jmap.account_id(),
            "emailId": email_id,
        })
        .to_string();
        // Claim the dedup key and XADD in a single atomic step: a Redis hiccup
        // mid-way must not leave the dedup key claimed while the payload is
        // absent from the stream (which would drop the event for the whole
        // 24h dedup window). Returning false means another worker owns it.
        // `false` means another worker already owns the dedup entry for this
        // email in the current window; either way the event is handled here.
        self.state
            .claim_dedup_and_enqueue(&key, 86_400, "stalwart:jmap", &payload)
            .await
            .map_err(|_| ())?;
        Ok(())
    }

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
        if let Intent::Search(query) = &intent {
            return self.handle_search(message.chat.id, query).await;
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

    async fn handle_search(&self, chat_id: i64, query: &str) -> Result<(), ()> {
        let query = query.trim();
        if query.is_empty() {
            return self
                .telegram
                .send_text(chat_id, "请提供搜索关键词，例如：/search 发票")
                .await
                .map_err(|_| ());
        }
        let result = self.jmap.search_emails(query, SEARCH_LIMIT).await;
        match search_reply(result.as_ref().map_err(|_| ()), query, SEARCH_LIMIT) {
            SearchReply::Text(reply) => self
                .telegram
                .send_text(chat_id, &reply)
                .await
                .map_err(|_| ()),
            // Hard JMAP failure: propagate so the coordinator answers
            // 503 + Retry-After and the event is retried; never send an
            // error stack to the user.
            SearchReply::Retry => Err(()),
        }
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
    Search(String),
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
    let input = input.trim();
    let text = input.to_ascii_lowercase();
    if text == "/help" || text == "help" || text.contains("帮助") || text.contains("怎么用") {
        return Intent::Help;
    }
    // `/search <query>`: the remainder is the whole query, read back from the
    // sender's original casing so the JMAP filter and the echoed reply keep
    // their capitalisation. A bare `/search` maps to Search("") so the handler
    // can ask for keywords; `/searchx` is not a command and falls through.
    if let Some(rest) = input.strip_prefix("/search") {
        if rest.is_empty() || rest.starts_with(char::is_whitespace) {
            return Intent::Search(rest.trim().to_owned());
        }
    }
    // The Chinese equivalents are also prefix-matched: matching a keyword
    // anywhere in the sentence would turn "帮我搜一下发票" into a search for
    // "一下发票" and hijack consent and summary requests.
    for keyword in ["搜索", "查找", "检索"] {
        if let Some(rest) = input.strip_prefix(keyword) {
            return Intent::Search(rest.trim().to_owned());
        }
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
    "使用说明：\n邮件到达后会先发送发件人、主题和时间等元数据通知。\n\n授权示例（请按原文中文输入，不支持英文别名）：\n1小时：/ai on、临时一次、临时、一次\n今天：今天\n7天：7天\n直到我撤销（最长365天）：直到我撤销、长期\n撤销授权：/ai off、关闭 ai、撤销授权、停止摘要\n/summary <email_id>：请求指定邮件摘要；未授权或授权到期时只返回元数据并提示重新授权。\n/search <关键词>：搜索邮件主题与正文，返回带高亮片段的匹配列表（最多 10 条）。\n/help（或 help）：显示本说明。\n\n隐私：AI 默认关闭，只有你明确开启后才会发送正文；授权有期限且不会自动续期；正文和 AI 结果不会持久化，也不会写入日志。\n如果未配置 AI 或 AI 调用失败，将回退为本地截取摘要。"
}

/// Upper bound on how many matching emails one `/search` reply lists.
const SEARCH_LIMIT: usize = 10;
/// Upper bound on characters of a snippet subject shown per result.
const SEARCH_SUBJECT_MAX: usize = 120;
/// Upper bound on characters of a snippet preview shown per result.
const SEARCH_PREVIEW_MAX: usize = 160;

enum SearchReply {
    Text(String),
    Retry,
}

/// Decide what to do with a search outcome without touching the network, so
/// the mapping is unit-testable: any hard failure becomes `Retry` (surfaced
/// as 503 + Retry-After by the coordinator), everything else renders to text.
fn search_reply(result: Result<&SearchResult, ()>, query: &str, limit: usize) -> SearchReply {
    match result {
        Ok(result) => SearchReply::Text(render_search_results(result, query, limit)),
        Err(()) => SearchReply::Retry,
    }
}

/// Render a search outcome as a plain-text Telegram reply. Highlight tags are
/// stripped (see [`strip_mark_tags`]) and subject/preview are truncated.
fn render_search_results(result: &SearchResult, query: &str, limit: usize) -> String {
    if result.ids.is_empty() {
        return format!("没有找到匹配「{query}」的邮件。");
    }
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "搜索「{query}」：共 {} 封匹配邮件",
        result.ids.len()
    ));
    if result.snippets.is_empty() {
        // SearchSnippet/get was unavailable: degrade to ids, never invent
        // fragments.
        lines.push("（高亮片段暂不可用，以下为匹配的邮件 ID）".to_owned());
        for id in result.ids.iter().take(limit) {
            lines.push(format!("- {id}"));
        }
        return lines.join("\n");
    }
    for snippet in result.snippets.iter().take(limit) {
        let subject = truncate_chars(
            &snippet
                .subject
                .as_deref()
                .map(clean_snippet)
                .unwrap_or_else(|| "（无主题）".to_owned()),
            SEARCH_SUBJECT_MAX,
        );
        let preview = truncate_chars(
            &snippet
                .preview
                .as_deref()
                .map(clean_snippet)
                .unwrap_or_default(),
            SEARCH_PREVIEW_MAX,
        );
        let id = snippet.email_id.as_str();
        // The id is what makes a hit actionable: without it the user cannot
        // follow up with `/summary <email_id>`.
        if preview.is_empty() {
            lines.push(format!("- {subject}（{id}）"));
        } else {
            lines.push(format!("- {subject}（{id}）\n  {preview}"));
        }
    }
    lines.join("\n")
}

/// Strip RFC 8621 `<mark>`/`</mark>` highlight tags so a snippet value can be
/// sent to Telegram as plain text. Byte offsets are safe because
/// `to_ascii_lowercase` preserves string length.
fn strip_mark_tags(input: &str) -> String {
    const OPEN: &str = "<mark>";
    const CLOSE: &str = "</mark>";
    let mut output = String::with_capacity(input.len());
    let mut remainder = input;
    while let Some(start) = remainder.to_ascii_lowercase().find(OPEN) {
        output.push_str(&remainder[..start]);
        remainder = &remainder[start + OPEN.len()..];
        match remainder.to_ascii_lowercase().find(CLOSE) {
            Some(end) => {
                output.push_str(&remainder[..end]);
                remainder = &remainder[end + CLOSE.len()..];
            }
            None => {
                output.push_str(remainder);
                return output;
            }
        }
    }
    output.push_str(remainder);
    output
}

/// Decode the HTML entities RFC 8621 §5 puts around snippet text, so an email
/// containing `&&` or `a < b` is not echoed back as `&amp;&amp;`. The pass is
/// strictly left-to-right, so `&amp;lt;` stays the literal text `&lt;` instead
/// of being decoded twice.
fn unescape_html_entities(input: &str) -> String {
    const ENTITIES: [(&str, &str); 6] = [
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&apos;", "'"),
        ("&amp;", "&"),
    ];
    let mut output = String::with_capacity(input.len());
    let mut remainder = input;
    while let Some(entity) = remainder.find('&') {
        let (head, tail) = remainder.split_at(entity);
        output.push_str(head);
        let mut decoded = false;
        for (name, replacement) in ENTITIES {
            if let Some(next) = tail.strip_prefix(name) {
                output.push_str(replacement);
                remainder = next;
                decoded = true;
                break;
            }
        }
        if !decoded {
            output.push('&');
            remainder = &tail[1..];
        }
    }
    output.push_str(remainder);
    output
}

/// Clean one RFC 8621 §5 snippet value for Telegram plain text. Tags are
/// stripped before entities are decoded: the server escapes angle brackets too,
/// so a literal `<mark>` in the email arrives as `&lt;mark&gt;`, and decoding
/// first would turn that into a real tag and erase the user's text.
fn clean_snippet(input: &str) -> String {
    unescape_html_entities(&strip_mark_tags(input))
}

/// Truncate to at most `max_chars` characters (by char, not byte), adding an
/// ellipsis when anything was cut.
fn truncate_chars(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_owned();
    }
    let cut: String = text.chars().take(max_chars).collect();
    format!("{cut}…")
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
    use crate::domain::jmap::SearchSnippet;

    use super::{
        help_message, parse_intent, render_search_results, search_reply, strip_mark_tags,
        truncate_chars, unescape_html_entities, Intent, SearchReply, SearchResult,
        SEARCH_PREVIEW_MAX, SEARCH_SUBJECT_MAX,
    };

    #[test]
    fn help_is_safe_and_actionable() {
        let text = help_message();
        for command in [
            "/ai on",
            "/ai off",
            "/summary <email_id>",
            "/search <关键词>",
            "/help",
        ] {
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
        // /search keeps the sender's casing and consumes the whole remainder.
        assert_eq!(
            parse_intent("/search Invoice Q3"),
            Intent::Search("Invoice Q3".into())
        );
        assert_eq!(parse_intent("/search"), Intent::Search(String::new()));
        // Bare-word queries: the keyword must lead the sentence, so that
        // "帮我搜一下发票" is not searched as "一下发票" and consent phrases
        // containing a later keyword stay consent intents.
        assert_eq!(
            parse_intent("搜索发票 报销"),
            Intent::Search("发票 报销".into())
        );
        assert_eq!(
            parse_intent("查找 e-1 的摘要"),
            Intent::Search("e-1 的摘要".into())
        );
        assert_eq!(parse_intent("帮我搜一下发票"), Intent::Unknown);
        assert!(matches!(
            parse_intent("同意一次搜索摘要"),
            Intent::Consent { ttl: 3600, .. }
        ));
        assert_eq!(parse_intent("/searchx abc"), Intent::Unknown);
    }

    #[test]
    fn strip_mark_tags_handles_case_mixed_and_unmatched_pairs() {
        assert_eq!(
            strip_mark_tags("<mark>invoice</mark> by friday"),
            "invoice by friday"
        );
        assert_eq!(strip_mark_tags("<MARK>a</MARK> <Mark>b</Mark>"), "a b");
        // An unmatched opener is dropped rather than emitted, so no raw markup
        // can reach Telegram.
        assert_eq!(strip_mark_tags("<mark>"), "");
        assert_eq!(strip_mark_tags("<mark>hello"), "hello");
        assert_eq!(strip_mark_tags("a & <b> not a mark"), "a & <b> not a mark");
        assert_eq!(strip_mark_tags(""), "");
    }

    #[test]
    fn unescape_html_entities_decodes_a_single_pass() {
        assert_eq!(
            unescape_html_entities("A &lt; B &gt; C &quot;pay&quot; &#39;now&#39;"),
            "A < B > C \"pay\" 'now'"
        );
        assert_eq!(unescape_html_entities("Tom &amp; Jerry"), "Tom & Jerry");
        // A lone ampersand and a partially escaped entity must not mangle the text.
        assert_eq!(unescape_html_entities("100% & ok"), "100% & ok");
        assert_eq!(unescape_html_entities("&amp;lt;"), "&lt;");
        assert_eq!(unescape_html_entities(""), "");
    }

    #[test]
    fn search_reply_maps_failures_to_retry_and_renders_outcomes() {
        assert!(matches!(
            search_reply(Err(()), "发票", 10),
            SearchReply::Retry
        ));
        assert!(matches!(
            search_reply(Ok(&SearchResult { ids: vec![], snippets: vec![] }), "发票", 10),
            SearchReply::Text(text) if text.contains("没有找到匹配")
        ));
        assert!(matches!(
            search_reply(
                Ok(&SearchResult { ids: vec!["e-1".into()], snippets: vec![] }),
                "发票",
                10
            ),
            SearchReply::Text(text) if text.contains("e-1")
        ));
    }

    #[test]
    fn render_search_results_strips_highlights_and_degrades_to_ids() {
        let result = SearchResult {
            ids: vec!["e-1".into(), "e-2".into()],
            snippets: vec![
                SearchSnippet {
                    subject: Some("<mark>invoice</mark> Q3".into()),
                    preview: Some("pay &lt;100&gt; &amp; send".into()),
                    email_id: "e-1".into(),
                },
                SearchSnippet {
                    subject: None,
                    preview: None,
                    email_id: "e-2".into(),
                },
            ],
        };
        let text = render_search_results(&result, "invoice", 2);
        assert!(!text.contains("<mark>"));
        assert!(text.contains("invoice Q3"));
        assert!(text.contains("pay <100> & send"));
        assert!(text.contains("（e-2）"));
        assert!(text.contains("（无主题）"));

        let ids_only = SearchResult {
            ids: vec!["e-9".into()],
            snippets: vec![],
        };
        let ids_text = render_search_results(&ids_only, "发票", 2);
        assert!(ids_text.contains("e-9"));
        assert!(ids_text.contains("高亮片段暂不可用"));
    }

    #[test]
    fn render_search_results_truncates_oversized_fields() {
        let result = SearchResult {
            ids: vec!["e-1".into()],
            snippets: vec![SearchSnippet {
                subject: Some("s".repeat(300)),
                preview: Some("p".repeat(300)),
                email_id: "e-1".into(),
            }],
        };
        let text = render_search_results(&result, "q", 10);
        assert!(text.contains("…"));
        // Neither field may survive the cap: the raw 300-char runs are gone.
        assert!(!text.contains(&"s".repeat(SEARCH_SUBJECT_MAX + 1)));
        assert!(!text.contains(&"p".repeat(SEARCH_PREVIEW_MAX + 1)));
        assert!(text.contains(&truncate_chars(&"s".repeat(300), SEARCH_SUBJECT_MAX)));
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

    #[test]
    fn baseline_cursor_roundtrips_state_and_position() {
        let encoded = encode_baseline_cursor("state:with:punctuation", 1234);
        assert_eq!(
            decode_baseline_cursor(&encoded),
            Some(("state:with:punctuation".to_owned(), 1234))
        );
        assert!(decode_baseline_cursor("baseline:odd:hex").is_none());
    }

    struct OkWorker;
    #[async_trait]
    impl WorkerHandler for OkWorker {
        async fn process(&self, _stream: &str, _payload: &str) -> Result<(), ()> {
            Ok(())
        }
    }
}
