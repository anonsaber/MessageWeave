use async_trait::async_trait;

use crate::domain::{Notification, UserCommand};

#[expect(dead_code, reason = "稳定ID+阶段0占位：渠道抽象供后续 adapter 使用")]
#[async_trait]
pub trait Channel: Send + Sync {
    async fn receive(&self, command: UserCommand);
}

#[expect(dead_code, reason = "稳定ID+阶段0占位：通知抽象供后续 adapter 使用")]
#[async_trait]
pub trait Notifier: Send + Sync {
    async fn notify(&self, notification: Notification);
}

#[expect(dead_code, reason = "稳定ID+阶段0占位：渠道消息适配抽象")]
pub trait MessageAdapter: Send + Sync {
    fn render_notification(&self, notification: &Notification) -> String;
}

/// Telegram is intentionally only an adapter boundary in stage 0 (no bot commands yet).
pub mod telegram {
    use crate::domain::Notification;
    use crate::state::RuntimeConfigProvider;
    use secrecy::{ExposeSecret, SecretString};
    use serde::Serialize;

    pub struct TelegramClient {
        http: reqwest::Client,
        token: SecretString,
        endpoint: String,
        runtime: RuntimeConfigProvider,
    }

    #[derive(Serialize)]
    struct SendMessage<'a> {
        chat_id: i64,
        text: &'a str,
    }

    impl TelegramClient {
        pub fn with_runtime(token: SecretString, runtime: RuntimeConfigProvider) -> Self {
            Self::with_endpoint_inner(token, runtime, "https://api.telegram.org".into())
        }

        fn with_endpoint_inner(
            token: SecretString,
            runtime: RuntimeConfigProvider,
            endpoint: String,
        ) -> Self {
            Self {
                http: reqwest::Client::new(),
                token,
                endpoint,
                runtime,
            }
        }

        /// Test-only endpoint injection; production always uses Telegram's official HTTPS host.
        #[cfg(test)]
        pub fn with_endpoint(
            token: SecretString,
            runtime: RuntimeConfigProvider,
            endpoint: String,
        ) -> Self {
            Self::with_endpoint_inner(token, runtime, endpoint)
        }

        /// Sends metadata only; body text is intentionally not accepted here.
        pub async fn send_notification(
            &self,
            chat_id: i64,
            notification: &Notification,
        ) -> Result<(), String> {
            let text = format!(
                "From: {}\nSubject: {}\nReceived: {}",
                notification.sender, notification.subject, notification.received_at
            );
            self.send_text(chat_id, &text).await
        }

        pub async fn send_text(&self, chat_id: i64, text: &str) -> Result<(), String> {
            let (timeout_ms, max_retries) = self
                .runtime
                .read()
                .map(|config| (config.telegram_timeout_ms, config.max_retries.min(5)))
                .unwrap_or((10_000, 3));
            let mut last_error = None;
            for _ in 0..=max_retries {
                let request = self
                    .http
                    .post(format!(
                        "{}/bot{}/sendMessage",
                        self.endpoint,
                        self.token.expose_secret()
                    ))
                    .json(&SendMessage { chat_id, text })
                    .send();
                let result = match tokio::time::timeout(
                    std::time::Duration::from_millis(timeout_ms.max(100)),
                    request,
                )
                .await
                {
                    Ok(result) => result
                        .and_then(|response| response.error_for_status())
                        .map_err(|_| "telegram request failed".to_owned()),
                    Err(_) => Err("telegram request timed out".to_owned()),
                };
                match result {
                    Ok(_) => return Ok(()),
                    Err(error) => last_error = Some(error),
                }
            }
            Err(last_error.unwrap_or_else(|| "telegram request failed".to_owned()))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::state::{runtime_provider, OutboundConfig};
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        async fn mock_server(
            status: &'static str,
            delay_ms: u64,
            requests: Arc<AtomicUsize>,
        ) -> String {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            tokio::spawn(async move {
                loop {
                    let Ok((mut socket, _)) = listener.accept().await else {
                        break;
                    };
                    requests.fetch_add(1, Ordering::Relaxed);
                    let mut buffer = [0_u8; 1024];
                    let _ = socket.read(&mut buffer).await;
                    if delay_ms > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    }
                    let response = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n");
                    let _ = socket.write_all(response.as_bytes()).await;
                }
            });
            format!("http://{address}")
        }

        fn client(endpoint: String, retries: u8, timeout_ms: u64) -> TelegramClient {
            let runtime = runtime_provider(OutboundConfig {
                telegram_timeout_ms: timeout_ms,
                max_retries: retries,
                ..OutboundConfig::default()
            });
            TelegramClient::with_endpoint(SecretString::new("test-token".into()), runtime, endpoint)
        }

        #[tokio::test]
        async fn mock_endpoint_success_sends_message() {
            let requests = Arc::new(AtomicUsize::new(0));
            let endpoint = mock_server("200 OK", 0, requests.clone()).await;
            client(endpoint, 0, 1_000)
                .send_text(42, "hello")
                .await
                .unwrap();
            assert_eq!(requests.load(Ordering::Relaxed), 1);
        }

        #[tokio::test]
        async fn error_response_retries_with_bound() {
            let requests = Arc::new(AtomicUsize::new(0));
            let endpoint = mock_server("500 Internal Server Error", 0, requests.clone()).await;
            assert!(client(endpoint, 2, 1_000)
                .send_text(42, "hello")
                .await
                .is_err());
            assert_eq!(requests.load(Ordering::Relaxed), 3);
        }

        #[tokio::test]
        async fn timeout_retries_with_bound() {
            let requests = Arc::new(AtomicUsize::new(0));
            let endpoint = mock_server("200 OK", 250, requests.clone()).await;
            assert!(client(endpoint, 1, 100)
                .send_text(42, "hello")
                .await
                .is_err());
            // The first request is guaranteed to time out; reqwest may cancel the
            // second connection before the tiny test server accepts it.
            assert!(requests.load(Ordering::Relaxed) >= 1);
        }
    }
}
