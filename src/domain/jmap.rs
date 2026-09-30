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

/// One page of JMAP `Email/changes` (RFC 8620 §4.1.2), narrowed to what a
/// notifier can act on.
///
/// The response also carries `oldState` and `destroyed`, and both are dropped
/// here on purpose. `oldState` echoes the `sinceState` the server accepted,
/// which the caller already holds. `destroyed` lists ids that disappeared, and
/// a deleted email has nothing left to look up, so it can drive neither a
/// notification nor the cursor. The cursor is `newState` alone, which means
/// both fields would have been values written and never read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailChanges {
    /// `newState`: the state after this page. Persist it and the next
    /// `/changes` replays nothing already seen.
    pub new_state: String,
    /// `created`: ids that appeared since `sinceState`.
    pub created: Vec<String>,
    /// `updated`: ids whose content changed since `sinceState`.
    pub updated: Vec<String>,
    /// `hasMoreChanges`: more exist. Callers may widen the next window instead
    /// of advancing the cursor, because the page returned was not truncated.
    pub has_more_changes: bool,
}

/// Highlighted RFC 8621 `SearchSnippet/get` fragment for one email. Values
/// carry the server's highlight markup (`<mark>`); the boundary passes them
/// through untouched and the presentation layer must sanitize before display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchSnippet {
    pub email_id: String,
    pub subject: Option<String>,
    pub preview: Option<String>,
}

/// Bounded search outcome: matched ids in server order plus any snippets the
/// server produced. `snippets` is empty when `Email/query` succeeded but
/// `SearchSnippet/get` was unavailable; ids remain the authoritative result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub ids: Vec<String>,
    pub snippets: Vec<SearchSnippet>,
}

#[async_trait]
pub trait JmapBackend: Send + Sync {
    async fn create_push_subscription(&self, callback_url: &str) -> Result<String, JmapError>;
    async fn destroy_push_subscription(&self, subscription_id: &str) -> Result<(), JmapError>;
    async fn verify_push_subscription(
        &self,
        subscription_id: &str,
        verification_code: &str,
    ) -> Result<(), JmapError>;
    /// RFC 8620 §2.1 `Session.state`: the token the server minted when the
    /// session was built.
    ///
    /// This is **not** an `Email`-collection state token, which is what
    /// `Email/changes` wants as `sinceState` (RFC 8620 §4.1.1). A collection
    /// token only comes back out of a `Changes` response itself (`newState`),
    /// so before the first successful call the session token is the only
    /// candidate we have, and it is the one we hand over. A strictly conforming
    /// server may therefore answer a `sinceState` error to the very first
    /// `/changes` after a re-baseline; `MetadataWorker::replay_changes` absorbs
    /// that by re-baselining to a fresh token and returning, so the next pass
    /// starts the walk over instead of retrying a token the server will keep
    /// rejecting. Once the walk finishes, the persisted cursor is a real
    /// collection token (`newState`), so the mismatch can only ever bite the
    /// first call after a re-baseline. The 24h dedup key bounds the replay it
    /// causes to at most one duplicate.
    async fn session_state(&self) -> Result<String, JmapError>;
    /// RFC 8620 §4.1 `Email/changes`: the events recorded since `since_state`,
    /// capped at `max_changes`. A server may return fewer and set
    /// `hasMoreChanges` instead of replaying everything at once; the
    /// window-widening loop that answers it lives in
    /// `MetadataWorker::replay_changes`.
    async fn email_changes(
        &self,
        account_id: &str,
        since_state: &str,
        max_changes: usize,
    ) -> Result<EmailChanges, JmapError>;
    async fn list_folders(&self, account_id: &str) -> Result<Vec<Folder>, JmapError>;
    async fn list_emails(
        &self,
        account_id: &str,
        folder_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EmailMetadata>, JmapError>;
    /// Fetch one bounded page without requiring the caller to materialize the
    /// entire mailbox. Implementations with a native JMAP position query should
    /// override this; the fallback preserves test backends' existing contract.
    async fn list_emails_page(
        &self,
        account_id: &str,
        folder_id: Option<&str>,
        position: usize,
        limit: usize,
    ) -> Result<Vec<EmailMetadata>, JmapError> {
        let all = self
            .list_emails(account_id, folder_id, position.saturating_add(limit))
            .await?;
        Ok(all.into_iter().skip(position).take(limit).collect())
    }
    async fn read_email(
        &self,
        account_id: &str,
        email_id: &str,
    ) -> Result<Option<EmailContent>, JmapError>;
    /// Bounded RFC 8621 `Email/query` text search across the mailbox. The
    /// result set must be capped at `limit`; implementations must not fall
    /// back to a full-mailbox scan. Snippet retrieval is best-effort —
    /// `SearchSnippet/get` failure degrades to ids-only, `Email/query` failure
    /// propagates as an error.
    async fn search_emails(
        &self,
        account_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<SearchResult, JmapError>;
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
    pub async fn create_push_subscription(&self, callback_url: &str) -> Result<String, JmapError> {
        if !callback_url.starts_with("https://") {
            return Err(JmapError::InvalidRequest("push callback URL"));
        }
        self.backend.create_push_subscription(callback_url).await
    }

    pub async fn destroy_push_subscription(&self, subscription_id: &str) -> Result<(), JmapError> {
        if subscription_id.trim().is_empty() {
            return Err(JmapError::InvalidRequest("push subscription id"));
        }
        self.backend
            .destroy_push_subscription(subscription_id)
            .await
    }
    pub async fn verify_push_subscription(
        &self,
        subscription_id: &str,
        verification_code: &str,
    ) -> Result<(), JmapError> {
        if subscription_id.trim().is_empty() || verification_code.trim().is_empty() {
            return Err(JmapError::InvalidRequest("push verification"));
        }
        self.backend
            .verify_push_subscription(subscription_id, verification_code)
            .await
    }
    pub async fn session_state(&self) -> Result<String, JmapError> {
        self.backend.session_state().await
    }

    pub async fn email_changes(
        &self,
        since_state: &str,
        max_changes: usize,
    ) -> Result<EmailChanges, JmapError> {
        if since_state.trim().is_empty() || max_changes == 0 {
            return Err(JmapError::InvalidRequest("changes"));
        }
        self.backend
            .email_changes(&self.account_id, since_state, max_changes)
            .await
    }
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

    pub async fn list_emails_page(
        &self,
        folder_id: Option<&str>,
        position: usize,
        limit: usize,
    ) -> Result<Vec<EmailMetadata>, JmapError> {
        if limit == 0 {
            return Err(JmapError::InvalidRequest("limit"));
        }
        self.backend
            .list_emails_page(&self.account_id, folder_id, position, limit)
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

    pub async fn search_emails(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<SearchResult, JmapError> {
        if query.trim().is_empty() || limit == 0 {
            return Err(JmapError::InvalidRequest("search query"));
        }
        self.backend
            .search_emails(&self.account_id, query, limit)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockBackend;

    #[async_trait]
    impl JmapBackend for MockBackend {
        async fn destroy_push_subscription(&self, subscription_id: &str) -> Result<(), JmapError> {
            assert_eq!(subscription_id, "push-1");
            Ok(())
        }
        async fn create_push_subscription(&self, callback_url: &str) -> Result<String, JmapError> {
            assert_eq!(callback_url, "https://bot.example/push/jmap");
            Ok("push-1".into())
        }
        async fn verify_push_subscription(
            &self,
            subscription_id: &str,
            verification_code: &str,
        ) -> Result<(), JmapError> {
            assert_eq!((subscription_id, verification_code), ("push-1", "code"));
            Ok(())
        }
        async fn session_state(&self) -> Result<String, JmapError> {
            Ok("state-1".into())
        }
        async fn email_changes(
            &self,
            account_id: &str,
            since_state: &str,
            max_changes: usize,
        ) -> Result<EmailChanges, JmapError> {
            assert_eq!(
                (account_id, since_state, max_changes),
                ("account-1", "old", 10)
            );
            Ok(EmailChanges {
                new_state: "new".into(),
                created: vec!["email-1".into()],
                updated: vec![],
                has_more_changes: false,
            })
        }
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
        async fn search_emails(
            &self,
            account_id: &str,
            query: &str,
            limit: usize,
        ) -> Result<SearchResult, JmapError> {
            assert_eq!((account_id, query, limit), ("account-1", "hello", 10));
            Ok(SearchResult {
                ids: vec!["email-1".into()],
                snippets: vec![SearchSnippet {
                    email_id: "email-1".into(),
                    subject: Some("<mark>hello</mark>".into()),
                    preview: Some("say <mark>hello</mark> now".into()),
                }],
            })
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

    #[tokio::test]
    async fn service_search_passes_bounded_query_through() {
        let service = JmapService::new(MockBackend, "account-1").unwrap();
        let result = service.search_emails("hello", 10).await.unwrap();
        assert_eq!(result.ids, vec!["email-1"]);
        assert_eq!(result.snippets.len(), 1);
        assert_eq!(
            result.snippets[0].subject.as_deref(),
            Some("<mark>hello</mark>")
        );
    }

    #[tokio::test]
    async fn service_rejects_invalid_search_input() {
        let service = JmapService::new(MockBackend, "account-1").unwrap();
        let err = service.search_emails("   ", 10).await.unwrap_err();
        assert!(matches!(err, JmapError::InvalidRequest("search query")));
        let err = service.search_emails("hello", 0).await.unwrap_err();
        assert!(matches!(err, JmapError::InvalidRequest("search query")));
    }

    #[test]
    fn service_rejects_invalid_limits_and_account() {
        assert!(JmapService::new(MockBackend, " ").is_err());
        assert!(JmapService::with_long_email_limit(MockBackend, "a", 0).is_err());
    }
}
