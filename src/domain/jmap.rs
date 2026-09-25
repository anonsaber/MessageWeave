//! Read-only JMAP domain boundary (REQ-SINGLE-ACCOUNT, REQ-LONG-EMAIL).
//!
//! The adapter returns channel-neutral values.  In particular, `read_email` returns
//! the original text supplied by JMAP; truncation and Telegram formatting belong to
//! the caller.  There are deliberately no mutation, attachment, AI, or streaming APIs.

pub mod client;

use async_trait::async_trait;
use thiserror::Error;

/// Errors exposed by the domain boundary.  Client details are retained for callers,
/// while credentials are never included in these messages.
#[derive(Debug, Error)]
pub enum JmapError {
    #[error("JMAP client error")]
    Client(#[from] jmap_client::Error),
    #[error("JMAP object was not found")]
    NotFound,
    #[error("invalid JMAP request: {0}")]
    InvalidRequest(&'static str),
    #[error("JMAP field is missing: {0}")]
    MissingField(&'static str),
    #[error("JMAP request timed out")]
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub id: String,
    pub name: String,
    pub role: Option<String>,
    pub total_emails: usize,
    pub unread_emails: usize,
}

/// Metadata only: no body or attachment is loaded by this operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailMetadata {
    pub id: String,
    pub subject: Option<String>,
    pub sender: Option<String>,
    pub received_at: Option<i64>,
    pub preview: Option<String>,
    pub size: Option<usize>,
    pub has_attachment: bool,
}

/// Original message content plus length information for the presentation layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailContent {
    pub metadata: EmailMetadata,
    pub text: String,
    pub character_count: usize,
    pub is_long: bool,
}

#[async_trait]
pub trait JmapBackend: Send + Sync {
    async fn list_folders(&self, account_id: &str) -> Result<Vec<Folder>, JmapError>;
    async fn list_emails(
        &self,
        account_id: &str,
        folder_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EmailMetadata>, JmapError>;
    async fn read_email(
        &self,
        account_id: &str,
        email_id: &str,
    ) -> Result<Option<EmailContent>, JmapError>;
}

/// Single-account service. `long_email_limit` only annotates output; it never
/// truncates source content, leaving display policy to the channel adapter.
pub struct JmapService<B> {
    backend: B,
    account_id: String,
    long_email_limit: usize,
}

impl<B> JmapService<B> {
    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    pub fn new(backend: B, account_id: impl Into<String>) -> Result<Self, JmapError> {
        Self::with_long_email_limit(backend, account_id, 4_000)
    }

    pub fn with_long_email_limit(
        backend: B,
        account_id: impl Into<String>,
        long_email_limit: usize,
    ) -> Result<Self, JmapError> {
        let account_id = account_id.into();
        if account_id.trim().is_empty() {
            return Err(JmapError::InvalidRequest("account_id"));
        }
        if long_email_limit == 0 {
            return Err(JmapError::InvalidRequest("long_email_limit"));
        }
        Ok(Self {
            backend,
            account_id,
            long_email_limit,
        })
    }
}

impl<B: JmapBackend> JmapService<B> {
    pub async fn list_folders(&self) -> Result<Vec<Folder>, JmapError> {
        self.backend.list_folders(&self.account_id).await
    }

    pub async fn list_emails(
        &self,
        folder_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EmailMetadata>, JmapError> {
        if limit == 0 {
            return Err(JmapError::InvalidRequest("limit"));
        }
        self.backend
            .list_emails(&self.account_id, folder_id, limit)
            .await
    }

    pub async fn read_email(&self, email_id: &str) -> Result<Option<EmailContent>, JmapError> {
        if email_id.trim().is_empty() {
            return Err(JmapError::InvalidRequest("email_id"));
        }
        let Some(mut content) = self.backend.read_email(&self.account_id, email_id).await? else {
            return Ok(None);
        };
        content.character_count = content.text.chars().count();
        content.is_long = content.character_count > self.long_email_limit;
        Ok(Some(content))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockBackend;

    #[async_trait]
    impl JmapBackend for MockBackend {
        async fn list_folders(&self, account_id: &str) -> Result<Vec<Folder>, JmapError> {
            assert_eq!(account_id, "account-1");
            Ok(vec![Folder {
                id: "inbox".into(),
                name: "Inbox".into(),
                role: Some("inbox".into()),
                total_emails: 1,
                unread_emails: 1,
            }])
        }
        async fn list_emails(
            &self,
            account_id: &str,
            folder_id: Option<&str>,
            limit: usize,
        ) -> Result<Vec<EmailMetadata>, JmapError> {
            assert_eq!(
                (account_id, folder_id, limit),
                ("account-1", Some("inbox"), 10)
            );
            Ok(vec![EmailMetadata {
                id: "email-1".into(),
                subject: Some("hello".into()),
                sender: None,
                received_at: None,
                preview: Some("hello".into()),
                size: Some(5),
                has_attachment: false,
            }])
        }
        async fn read_email(
            &self,
            account_id: &str,
            email_id: &str,
        ) -> Result<Option<EmailContent>, JmapError> {
            assert_eq!((account_id, email_id), ("account-1", "email-1"));
            Ok(Some(EmailContent {
                metadata: EmailMetadata {
                    id: email_id.into(),
                    subject: None,
                    sender: None,
                    received_at: None,
                    preview: None,
                    size: None,
                    has_attachment: false,
                },
                text: "原文内容".into(),
                character_count: 0,
                is_long: false,
            }))
        }
    }

    #[tokio::test]
    async fn service_keeps_original_content_and_marks_long_messages() {
        let service = JmapService::with_long_email_limit(MockBackend, "account-1", 2).unwrap();
        assert_eq!(service.list_folders().await.unwrap().len(), 1);
        assert_eq!(
            service.list_emails(Some("inbox"), 10).await.unwrap().len(),
            1
        );
        let email = service.read_email("email-1").await.unwrap().unwrap();
        assert_eq!(email.text, "原文内容");
        assert_eq!(email.character_count, 4);
        assert!(email.is_long);
    }

    #[test]
    fn service_rejects_invalid_limits_and_account() {
        assert!(JmapService::new(MockBackend, " ").is_err());
        assert!(JmapService::with_long_email_limit(MockBackend, "a", 0).is_err());
    }
}
