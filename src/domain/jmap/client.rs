//! Concrete read-only adapter (MOD-JMAP-CLIENT, GATE-G1-JMAP-READONLY).

use super::{EmailContent, EmailMetadata, Folder, JmapBackend, JmapError};
use crate::state::{runtime_provider, OutboundConfig, RuntimeConfigProvider};
use async_trait::async_trait;
use jmap_client::{email, mailbox, Get};

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
        let filter: Option<email::query::Filter> = folder_id.map(email::query::Filter::in_mailbox);
        let ids = tokio::time::timeout(
            self.timeout(),
            self.client.email_query(
                filter,
                None::<Vec<jmap_client::core::query::Comparator<email::query::Comparator>>>,
            ),
        )
        .await
        .map_err(|_| JmapError::Timeout)??
        .take_ids();
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
}

impl JmapClientBackend {
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

fn normalize_session_url(value: &str) -> Result<String, JmapError> {
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

fn metadata(email: &jmap_client::email::Email<Get>) -> EmailMetadata {
    EmailMetadata {
        id: email.id().unwrap_or_default().to_owned(),
        subject: email.subject().map(str::to_owned),
        sender: email
            .from()
            .and_then(|addresses| addresses.first())
            .map(|address| address.email().to_owned()),
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
}
