//! SPLIT-EVAL: 已评估暂缓拆分——jmap-client 0.4.2 的适配、请求建模与降级策略必须贴在一起才能对照 crate 文档逐字段验证，拆开会切断这条对照链。
//! Concrete read-only adapter (MOD-JMAP-CLIENT, GATE-G1-JMAP-READONLY).

use super::{
    EmailChanges, EmailContent, EmailMetadata, Folder, JmapBackend, JmapError, SearchResult,
    SearchSnippet,
};
use crate::state::{runtime_provider, OutboundConfig, RuntimeConfigProvider};
use async_trait::async_trait;
use jmap_client::{email, mailbox, DataType, Get};

pub struct JmapClientBackend {
    client: jmap_client::client::Client,
    account_id: String,
    runtime: RuntimeConfigProvider,
}

impl JmapClientBackend {
    pub async fn connect(
        session_url: &str,
        username: &str,
        password: &str,
        account_id: Option<&str>,
    ) -> Result<Self, JmapError> {
        Self::connect_with_runtime(
            session_url,
            username,
            password,
            account_id,
            runtime_provider(OutboundConfig::default()),
        )
        .await
    }

    pub async fn connect_with_runtime(
        session_url: &str,
        username: &str,
        password: &str,
        account_id: Option<&str>,
        runtime: RuntimeConfigProvider,
    ) -> Result<Self, JmapError> {
        let session_base = normalize_session_url(session_url)?;
        let timeout_ms = runtime
            .read()
            .map(|config| config.jmap_timeout_ms)
            .unwrap_or(15_000);
        let timeout = std::time::Duration::from_millis(timeout_ms.max(100));
        let connect = jmap_client::client::Client::new()
            .credentials((username, password))
            .follow_redirects([trusted_redirect_host(&session_base)])
            .connect(&session_base);
        let mut client = tokio::time::timeout(timeout, connect)
            .await
            .map_err(|_| JmapError::Timeout)??;
        let selected = select_account(
            account_id,
            client.default_account_id(),
            client.session().accounts(),
        )?;
        client.set_default_account_id(selected.clone());
        Ok(Self {
            client,
            account_id: selected,
            runtime,
        })
    }

    pub fn account_id(&self) -> &str {
        &self.account_id
    }
}

#[async_trait]
impl JmapBackend for JmapClientBackend {
    async fn destroy_push_subscription(&self, subscription_id: &str) -> Result<(), JmapError> {
        if subscription_id.trim().is_empty() {
            return Err(JmapError::InvalidRequest("push subscription id"));
        }
        tokio::time::timeout(
            self.timeout(),
            self.client.push_subscription_destroy(subscription_id),
        )
        .await
        .map_err(|_| JmapError::Timeout)??;
        Ok(())
    }

    async fn create_push_subscription(&self, callback_url: &str) -> Result<String, JmapError> {
        let parsed = url::Url::parse(callback_url)
            .map_err(|_| JmapError::InvalidRequest("push callback URL"))?;
        if parsed.scheme() != "https" || parsed.username() != "" || parsed.password().is_some() {
            return Err(JmapError::InvalidRequest("push callback URL"));
        }
        let subscription = tokio::time::timeout(
            self.timeout(),
            self.client.push_subscription_create(
                format!("message-weave-{}", self.account_id),
                callback_url,
                None,
            ),
        )
        .await
        .map_err(|_| JmapError::Timeout)??;
        let subscription_id = subscription
            .id()
            .map(ToOwned::to_owned)
            .ok_or(JmapError::MissingField("push subscription id"))?;
        // JMAP PushSubscription/set create has no `types` argument in
        // jmap-client 0.4.2. Restrict the newly-created subscription with the
        // follow-up update before exposing its id (REQ-PUSH-TYPES).
        if tokio::time::timeout(
            self.timeout(),
            self.client.push_subscription_update_types(
                &subscription_id,
                Some([DataType::Email, DataType::EmailDelivery]),
            ),
        )
        .await
        .map_err(|_| JmapError::Timeout)?
        .is_err()
        {
            let _ = self
                .client
                .push_subscription_destroy(&subscription_id)
                .await;
            return Err(JmapError::InvalidRequest("push subscription types"));
        }
        Ok(subscription_id)
    }

    async fn verify_push_subscription(
        &self,
        subscription_id: &str,
        verification_code: &str,
    ) -> Result<(), JmapError> {
        if subscription_id.trim().is_empty() || verification_code.trim().is_empty() {
            return Err(JmapError::InvalidRequest("push verification"));
        }
        let verified = tokio::time::timeout(
            self.timeout(),
            self.client
                .push_subscription_verify(subscription_id, verification_code),
        )
        .await
        .map_err(|_| JmapError::Timeout)??;
        if verified.is_some() {
            Ok(())
        } else {
            Err(JmapError::NotFound)
        }
    }

    async fn session_state(&self) -> Result<String, JmapError> {
        Ok(self.client.session().state().to_owned())
    }

    async fn email_changes(
        &self,
        account_id: &str,
        since_state: &str,
        max_changes: usize,
    ) -> Result<EmailChanges, JmapError> {
        if account_id != self.account_id || since_state.trim().is_empty() || max_changes == 0 {
            return Err(JmapError::InvalidRequest("email changes"));
        }
        let mut request = self.client.build();
        request.changes_email(since_state).max_changes(max_changes);
        let response = tokio::time::timeout(self.timeout(), request.send_changes_email())
            .await
            .map_err(|_| JmapError::Timeout)??;
        Ok(EmailChanges {
            new_state: response.new_state().to_owned(),
            created: response.created().to_owned(),
            updated: response.updated().to_owned(),
            has_more_changes: response.has_more_changes(),
        })
    }

    async fn list_folders(&self, account_id: &str) -> Result<Vec<Folder>, JmapError> {
        if account_id != self.account_id {
            return Err(JmapError::InvalidRequest("account_id"));
        }
        let ids = tokio::time::timeout(
            self.timeout(),
            self.client.mailbox_query(
                None::<mailbox::query::Filter>,
                None::<Vec<jmap_client::core::query::Comparator<mailbox::query::Comparator>>>,
            ),
        )
        .await
        .map_err(|_| JmapError::Timeout)??
        .take_ids();
        let mut folders = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(mailbox) = tokio::time::timeout(
                self.timeout(),
                self.client.mailbox_get(&id, None::<Vec<mailbox::Property>>),
            )
            .await
            .map_err(|_| JmapError::Timeout)??
            {
                folders.push(Folder {
                    id: mailbox
                        .id()
                        .ok_or(JmapError::MissingField("mailbox id"))?
                        .to_owned(),
                    name: mailbox.name().unwrap_or_default().to_owned(),
                    role: role_name(mailbox.role()),
                    total_emails: mailbox.total_emails(),
                    unread_emails: mailbox.unread_emails(),
                });
            }
        }
        Ok(folders)
    }

    async fn list_emails(
        &self,
        account_id: &str,
        folder_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EmailMetadata>, JmapError> {
        if account_id != self.account_id || limit == 0 {
            return Err(JmapError::InvalidRequest("email query"));
        }
        // Query in bounded pages. A large limit (used for the cold-start
        // baseline) therefore cannot silently stop at the server default page.
        let page_size = limit.clamp(1, 100);
        let mut position = 0_i32;
        let mut ids = Vec::new();
        while ids.len() < limit {
            let mut request = self.client.build();
            let query = request.query_email();
            if let Some(folder_id) = folder_id {
                query.filter(email::query::Filter::in_mailbox(folder_id));
            }
            query.position(position).limit(page_size);
            let mut response = tokio::time::timeout(self.timeout(), request.send_query_email())
                .await
                .map_err(|_| JmapError::Timeout)??;
            let page = response.take_ids();
            if page.is_empty() {
                break;
            }
            position = position.saturating_add(page.len() as i32);
            ids.extend(page);
            if ids.len() >= limit || position <= 0 {
                break;
            }
        }
        ids.truncate(limit);
        let mut out = Vec::new();
        for id in ids.into_iter().take(limit) {
            if let Some(email) = tokio::time::timeout(
                self.timeout(),
                self.client.email_get(&id, None::<Vec<email::Property>>),
            )
            .await
            .map_err(|_| JmapError::Timeout)??
            {
                out.push(metadata(&email));
            }
        }
        Ok(out)
    }

    async fn list_emails_page(
        &self,
        account_id: &str,
        folder_id: Option<&str>,
        position: usize,
        limit: usize,
    ) -> Result<Vec<EmailMetadata>, JmapError> {
        self.fetch_email_page(account_id, folder_id, position, limit)
            .await
    }

    async fn read_email(
        &self,
        account_id: &str,
        email_id: &str,
    ) -> Result<Option<EmailContent>, JmapError> {
        if account_id != self.account_id {
            return Err(JmapError::InvalidRequest("account_id"));
        }
        let mut request = self.client.build();
        let get = request.get_email().ids([email_id]).properties([
            email::Property::From,
            email::Property::Subject,
            email::Property::Preview,
            email::Property::TextBody,
            email::Property::BodyValues,
            email::Property::Size,
            email::Property::ReceivedAt,
            email::Property::HasAttachment,
        ]);
        get.arguments().fetch_text_body_values(true);
        let Some(email) = tokio::time::timeout(self.timeout(), request.send_get_email())
            .await
            .map_err(|_| JmapError::Timeout)??
            .take_list()
            .pop()
        else {
            return Ok(None);
        };
        let text = join_body_values(email.text_body().into_iter().flatten().map(|part| {
            part.part_id()
                .and_then(|id| email.body_value(id))
                .map(|body| body.value())
        }))?;
        Ok(Some(EmailContent {
            metadata: metadata(&email),
            character_count: text.chars().count(),
            is_long: false,
            text,
        }))
    }

    async fn search_emails(
        &self,
        account_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<SearchResult, JmapError> {
        if account_id != self.account_id || limit == 0 || query.trim().is_empty() {
            return Err(JmapError::InvalidRequest("email search"));
        }
        // JMAP caps a single Email/query page; a bounded search never pages,
        // so clamp instead of issuing a follow-up position request.
        let limit = limit.min(100);
        let filter = email::query::Filter::text(query);
        let mut request = self.client.build();
        let email_query = request.query_email();
        email_query.filter(filter.clone()).position(0).limit(limit);
        let mut response = tokio::time::timeout(self.timeout(), request.send_query_email())
            .await
            .map_err(|_| JmapError::Timeout)??;
        let mut ids = response.take_ids();
        ids.truncate(limit);
        // Snippets are display-only: when the server cannot answer
        // SearchSnippet/get (e.g. `unknownMethod`) we still return the matched
        // ids so the caller can degrade to a result list without highlights.
        let snippets = match tokio::time::timeout(
            self.timeout(),
            self.client
                .search_snippet_get(Some(filter), ids.iter().cloned()),
        )
        .await
        {
            Ok(Ok(response)) => response
                .list()
                .iter()
                .map(|snippet| SearchSnippet {
                    email_id: snippet.email_id().to_owned(),
                    subject: snippet.subject().map(str::to_owned),
                    preview: snippet.preview().map(str::to_owned),
                })
                .collect(),
            Ok(Err(_)) | Err(_) => Vec::new(),
        };
        Ok(SearchResult { ids, snippets })
    }
}

impl JmapClientBackend {
    async fn fetch_email_page(
        &self,
        account_id: &str,
        folder_id: Option<&str>,
        position: usize,
        limit: usize,
    ) -> Result<Vec<EmailMetadata>, JmapError> {
        if account_id != self.account_id || limit == 0 {
            return Err(JmapError::InvalidRequest("email query"));
        }
        let mut request = self.client.build();
        let query = request.query_email();
        if let Some(folder_id) = folder_id {
            query.filter(email::query::Filter::in_mailbox(folder_id));
        }
        query.position(position as i32).limit(limit.min(100));
        let mut response = tokio::time::timeout(self.timeout(), request.send_query_email())
            .await
            .map_err(|_| JmapError::Timeout)??;
        let ids = response.take_ids();
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(email) = tokio::time::timeout(
                self.timeout(),
                self.client.email_get(&id, None::<Vec<email::Property>>),
            )
            .await
            .map_err(|_| JmapError::Timeout)??
            {
                out.push(metadata(&email));
            }
        }
        Ok(out)
    }

    fn timeout(&self) -> std::time::Duration {
        std::time::Duration::from_millis(
            self.runtime
                .read()
                .map(|config| config.jmap_timeout_ms)
                .unwrap_or(15_000)
                .max(100),
        )
    }
}

fn join_body_values<'a>(
    values: impl Iterator<Item = Option<&'a str>>,
) -> Result<String, JmapError> {
    let text = values.flatten().collect::<String>();
    if text.is_empty() {
        Err(JmapError::MissingField("text body value"))
    } else {
        Ok(text)
    }
}

fn role_name(role: mailbox::Role) -> Option<String> {
    match role {
        mailbox::Role::Archive => Some("archive".into()),
        mailbox::Role::Drafts => Some("drafts".into()),
        mailbox::Role::Important => Some("important".into()),
        mailbox::Role::Inbox => Some("inbox".into()),
        mailbox::Role::Junk => Some("junk".into()),
        mailbox::Role::Sent => Some("sent".into()),
        mailbox::Role::Trash => Some("trash".into()),
        mailbox::Role::Other(value) => Some(value),
        mailbox::Role::None => None,
    }
}

pub(crate) fn normalize_session_url(value: &str) -> Result<String, JmapError> {
    let parsed =
        url::Url::parse(value).map_err(|_| JmapError::InvalidRequest("JMAP_SESSION_URL"))?;
    if parsed.scheme() != "https" {
        return Err(JmapError::InvalidRequest("JMAP_SESSION_URL must use HTTPS"));
    }
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(JmapError::InvalidRequest(
            "JMAP_SESSION_URL must not contain credentials or query",
        ));
    }
    if !matches!(parsed.path(), "" | "/" | "/.well-known/jmap") {
        return Err(JmapError::InvalidRequest("invalid JMAP_SESSION_URL path"));
    }
    Ok(parsed.origin().ascii_serialization())
}

/// Hosts the JMAP client is allowed to follow redirects to.
///
/// jmap-client ships with an empty trust list and aborts any redirect to a host it was not
/// told to trust. Stalwart always 307-redirects `/.well-known/jmap` to `/jmap/session`, so
/// leaving the list empty makes every Stalwart deployment fail at connect with
/// "Aborting redirect request to unknown host". We trust the single origin host we were
/// handed and nothing else — the client still refuses to be pointed at a different host.
pub(crate) fn trusted_redirect_host(session_base: &str) -> String {
    url::Url::parse(session_base)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_default()
}

fn select_account<'a>(
    requested: Option<&str>,
    primary: &'a str,
    mut accounts: impl Iterator<Item = &'a String>,
) -> Result<String, JmapError> {
    let selected = requested
        .filter(|id| !id.trim().is_empty())
        .unwrap_or(primary);
    if selected.is_empty() {
        return Err(JmapError::MissingField("primary account"));
    }
    if !accounts.any(|id| id == selected) {
        return Err(JmapError::InvalidRequest(
            "ACCOUNT_ID is not in session accounts",
        ));
    }
    Ok(selected.to_owned())
}

/// One from-address the way RFC 5322 prints it, which is what a reader expects
/// on a `From:` line. With no display name, the address is the whole thing.
fn sender_display(address: &jmap_client::email::EmailAddress) -> String {
    match address.name() {
        Some(name) => format!("{name} <{}>", address.email()),
        None => address.email().to_owned(),
    }
}

fn metadata(email: &jmap_client::email::Email<Get>) -> EmailMetadata {
    EmailMetadata {
        id: email.id().unwrap_or_default().to_owned(),
        subject: email.subject().map(str::to_owned),
        sender: email
            .from()
            .and_then(|addresses| addresses.first())
            .map(sender_display),
        received_at: email.received_at(),
        preview: email.preview().map(str::to_owned),
        size: Some(email.size()),
        has_attachment: email.has_attachment(),
    }
}

#[cfg(test)]
mod real_server_tests {
    use super::*;

    /// Opt-in smoke test for a real Stalwart instance; credentials are never printed.
    #[tokio::test]
    #[ignore = "requires an explicitly configured JMAP test server"]
    async fn session_list_and_read_smoke() {
        let Some(url) = std::env::var_os("JMAP_SESSION_URL") else {
            eprintln!("skipped: JMAP_SESSION_URL is not configured");
            return;
        };
        let Some(user) = std::env::var_os("JMAP_USERNAME") else {
            eprintln!("skipped: JMAP_USERNAME is not configured");
            return;
        };
        let Some(password) = std::env::var_os("JMAP_PASSWORD") else {
            eprintln!("skipped: JMAP_PASSWORD is not configured");
            return;
        };
        let account = std::env::var("ACCOUNT_ID").ok();
        let backend = JmapClientBackend::connect(
            url.to_str().unwrap_or_default(),
            user.to_str().unwrap_or_default(),
            password.to_str().unwrap_or_default(),
            account.as_deref(),
        )
        .await
        .expect("configured JMAP session must connect");
        let folders = backend.list_folders(backend.account_id()).await.unwrap();
        let emails = backend
            .list_emails(backend.account_id(), None, 1)
            .await
            .unwrap();
        if let Some(email) = emails.first() {
            let _ = backend
                .read_email(backend.account_id(), &email.id)
                .await
                .unwrap();
        }
        assert!(!folders.is_empty() || emails.is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::jmap::JmapService;

    fn accounts(values: &[&str]) -> impl Iterator<Item = &'static String> {
        let owned = values
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>();
        Box::leak(owned.into_boxed_slice()).iter()
    }

    #[test]
    fn redirect_trust_is_derived_from_the_given_origin_only() {
        assert_eq!(
            trusted_redirect_host("https://mail.example.com"),
            "mail.example.com"
        );
        // normalize_session_url collapses the accepted paths to the origin, so the trust
        // list stays a single host even when the caller typed the well-known path.
        assert_eq!(
            trusted_redirect_host(
                &normalize_session_url("https://mail.example.com/.well-known/jmap").unwrap()
            ),
            "mail.example.com"
        );
        // host_str() excludes the port, and jmap-client compares against host_str() too,
        // so a non-standard port must not leak into the trust list.
        assert_eq!(
            trusted_redirect_host("https://mail.example.com:8443"),
            "mail.example.com"
        );
        // An unparseable base yields an empty host, which matches no redirect target, so the
        // client falls back to trusting nobody — the pre-existing fail-closed behaviour.
        assert_eq!(trusted_redirect_host("not a url"), "");
    }

    #[test]
    fn account_selection_supports_primary_and_validates_explicit() {
        assert_eq!(
            select_account(None, "primary", accounts(&["primary"])).unwrap(),
            "primary"
        );
        assert_eq!(
            select_account(
                Some("secondary"),
                "primary",
                accounts(&["primary", "secondary"])
            )
            .unwrap(),
            "secondary"
        );
        assert!(select_account(Some("missing"), "primary", accounts(&["primary"])).is_err());
    }

    #[test]
    fn session_url_and_limit_boundaries_are_rejected() {
        assert!(normalize_session_url("http://mail.example").is_err());
        assert_eq!(
            normalize_session_url("https://mail.example/.well-known/jmap").unwrap(),
            "https://mail.example"
        );
        assert_eq!(
            normalize_session_url("https://mail.example").unwrap(),
            "https://mail.example"
        );
        assert!(normalize_session_url("https://u:p@mail.example").is_err());
        assert!(normalize_session_url("https://mail.example/?token=x").is_err());
        assert!(normalize_session_url("https://mail.example/#fragment").is_err());
        assert!(normalize_session_url("https://mail.example/mail/jmap").is_err());
        assert!(JmapService::with_long_email_limit((), "account", 0).is_err());
    }

    #[test]
    fn body_values_join_in_order_and_empty_is_explicit_error() {
        assert_eq!(
            join_body_values([Some("first "), None, Some("second")].into_iter()).unwrap(),
            "first second"
        );
        assert!(matches!(
            join_body_values([None, None].into_iter()),
            Err(JmapError::MissingField("text body value"))
        ));
    }

    #[test]
    fn roles_are_normalized() {
        assert_eq!(role_name(mailbox::Role::Inbox).as_deref(), Some("inbox"));
        assert_eq!(role_name(mailbox::Role::None), None);
    }

    fn email_with_from(addresses: &[serde_json::Value]) -> email::Email<Get> {
        serde_json::from_value(serde_json::json!({
            "id": "m1",
            "subject": "hello",
            "preview": "hi",
            "from": addresses,
        }))
        .expect("a minimal Email/get payload must deserialize")
    }

    #[test]
    fn sender_keeps_the_display_name_in_rfc5322_form() {
        let address = serde_json::from_value::<email::EmailAddress>(serde_json::json!({
            "email": "zhang@example.com",
            "name": "张三",
        }))
        .expect("a minimal from-address must deserialize");

        assert_eq!(sender_display(&address), "张三 <zhang@example.com>");
    }

    #[test]
    fn sender_without_a_display_name_is_the_bare_address() {
        let address = serde_json::from_value::<email::EmailAddress>(serde_json::json!({
            "email": "noreply@example.com",
        }))
        .expect("a nameless from-address must deserialize");

        assert_eq!(sender_display(&address), "noreply@example.com");
    }

    #[test]
    fn metadata_takes_the_sender_from_the_from_address() {
        // The address is only readable if Email/get actually asked for the "from"
        // property, which is why `metadata` must read it from `from` and not just
        // keep the bare address.
        let email =
            email_with_from(&[serde_json::json!({"email": "zhang@example.com", "name": "张三"})]);
        let meta = metadata(&email);
        assert_eq!(meta.sender.as_deref(), Some("张三 <zhang@example.com>"));

        // No display name, so the address stands alone.
        let email = email_with_from(&[serde_json::json!({"email": "noreply@example.com"})]);
        assert_eq!(
            metadata(&email).sender.as_deref(),
            Some("noreply@example.com")
        );

        // Multiple addresses: the notification shows the first one.
        let email = email_with_from(&[
            serde_json::json!({"email": "first@example.com"}),
            serde_json::json!({"email": "second@example.com"}),
        ]);
        assert_eq!(
            metadata(&email).sender.as_deref(),
            Some("first@example.com")
        );

        // No `from` at all means `sender` stays `None` and the caller supplies
        // its fallback, so this assertion would also hold before the fix.
        let email = email_with_from(&[]);
        assert_eq!(metadata(&email).sender, None);
    }
}
