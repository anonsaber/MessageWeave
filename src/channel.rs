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
    use serde::{Deserialize, Serialize};

    /// ⑥-a 重试退避常量。
    const TELEGRAM_BACKOFF_BASE_MS: u64 = 250; // 起步
    const TELEGRAM_BACKOFF_CAP_MS: u64 = 4_000; // 单轮封顶
    const TELEGRAM_RETRY_AFTER_CAP_MS: u64 = 60_000; // 429 retry_after 钳到 ≤60s
    const TELEGRAM_TOTAL_BACKOFF_BUDGET_MS: u64 = 60_000; // 总退避预算上限，避免失控

    /// 退避序列纯函数：起步 250ms、每轮 ×2、单轮封顶 4s（⑥-a）。sleep 只调用它。
    /// 入参为已完成的重试轮次，与 `max_retries` 同为 `u8`。
    fn backoff_delay_ms(attempt: u8) -> u64 {
        let shift = attempt.min(4);
        (TELEGRAM_BACKOFF_BASE_MS << shift).min(TELEGRAM_BACKOFF_CAP_MS)
    }

    /// 429 响应体 `parameters.retry_after`（秒）→ 毫秒，钳到 ≤60s。
    /// 缺字段 / 非数字 / 非正 → None（调用方回退到固定指数退避）。
    fn retry_after_ms(body: &str) -> Option<u64> {
        #[derive(Deserialize)]
        struct Payload {
            parameters: Parameters,
        }
        #[derive(Deserialize)]
        struct Parameters {
            retry_after: Option<f64>,
        }
        let payload: Payload = serde_json::from_str(body).ok()?;
        let seconds = payload.parameters.retry_after?;
        if !seconds.is_finite() || seconds <= 0.0 {
            return None;
        }
        let capped = seconds.min(TELEGRAM_RETRY_AFTER_CAP_MS as f64 / 1000.0);
        Some((capped * 1000.0) as u64)
    }

    /// 可注入的 sleep（生产恒为 tokio::time::sleep；测试覆盖为 no-op，避免真实等待）。
    type SleepFn =
        fn(std::time::Duration) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;
    fn default_sleep(
        d: std::time::Duration,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(tokio::time::sleep(d))
    }

    pub struct TelegramClient {
        http: reqwest::Client,
        token: SecretString,
        endpoint: String,
        runtime: RuntimeConfigProvider,
        sleep: SleepFn,
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
                sleep: default_sleep,
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

        /// Test-only sleep injection：覆盖为 no-op，避免退避测试真实等待（⑥-a）。
        #[cfg(test)]
        pub fn with_sleep_override(mut self, sleep: SleepFn) -> Self {
            self.sleep = sleep;
            self
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
            let mut backoff_spent_ms: u64 = 0;
            let mut next_retry_after_ms: Option<u64> = None;
            for attempt in 0..=max_retries {
                if attempt > 0 {
                    // ⑥-a：失败重试之间的退避（指数退避；429 时按服务器指定 retry_after）。
                    let delay = next_retry_after_ms
                        .take()
                        .unwrap_or_else(|| backoff_delay_ms(attempt - 1));
                    let delay = delay
                        .min(TELEGRAM_TOTAL_BACKOFF_BUDGET_MS.saturating_sub(backoff_spent_ms));
                    backoff_spent_ms += delay;
                    if delay > 0 {
                        (self.sleep)(std::time::Duration::from_millis(delay)).await;
                    }
                }
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
                    Ok(result) => match result {
                        Ok(response)
                            if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS =>
                        {
                            // 429：读取响应体 parameters.retry_after（秒），钳 ≤60s。
                            // body 读取同样受 telegram_timeout_ms 约束，避免挂起。
                            let body = match tokio::time::timeout(
                                std::time::Duration::from_millis(timeout_ms.max(100)),
                                response.text(),
                            )
                            .await
                            {
                                Ok(Ok(body)) => body,
                                _ => String::new(),
                            };
                            next_retry_after_ms = retry_after_ms(&body);
                            Err("telegram request rate limited (429)".to_owned())
                        }
                        Ok(response) => response
                            .error_for_status()
                            .map(|_| ())
                            .map_err(|_| "telegram request failed".to_owned()),
                        Err(_) => Err("telegram request failed".to_owned()),
                    },
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
            body: &'static str,
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
                    if body.is_empty() {
                        let response = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\n\r\n");
                        let _ = socket.write_all(response.as_bytes()).await;
                    } else {
                        let response = format!(
                            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = socket.write_all(response.as_bytes()).await;
                    }
                }
            });
            format!("http://{address}")
        }

        /// 测试用 no-op sleep：退避逻辑完整走通但不真实等待（⑥-a）。
        fn instant_sleep(
            _: std::time::Duration,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
            Box::pin(async {})
        }

        fn client(endpoint: String, retries: u8, timeout_ms: u64) -> TelegramClient {
            let runtime = runtime_provider(OutboundConfig {
                telegram_timeout_ms: timeout_ms,
                max_retries: retries,
                ..OutboundConfig::default()
            });
            TelegramClient::with_endpoint(SecretString::new("test-token".into()), runtime, endpoint)
                .with_sleep_override(instant_sleep)
        }

        #[tokio::test]
        async fn mock_endpoint_success_sends_message() {
            let requests = Arc::new(AtomicUsize::new(0));
            let endpoint = mock_server("200 OK", 0, "", requests.clone()).await;
            client(endpoint, 0, 1_000)
                .send_text(42, "hello")
                .await
                .unwrap();
            assert_eq!(requests.load(Ordering::Relaxed), 1);
        }

        #[tokio::test]
        async fn error_response_retries_with_bound() {
            let requests = Arc::new(AtomicUsize::new(0));
            let endpoint = mock_server("500 Internal Server Error", 0, "", requests.clone()).await;
            assert!(client(endpoint, 2, 1_000)
                .send_text(42, "hello")
                .await
                .is_err());
            assert_eq!(requests.load(Ordering::Relaxed), 3);
        }

        #[tokio::test]
        async fn timeout_retries_with_bound() {
            let requests = Arc::new(AtomicUsize::new(0));
            let endpoint = mock_server("200 OK", 250, "", requests.clone()).await;
            assert!(client(endpoint, 1, 100)
                .send_text(42, "hello")
                .await
                .is_err());
            // The first request is guaranteed to time out; reqwest may cancel the
            // second connection before the tiny test server accepts it.
            assert!(requests.load(Ordering::Relaxed) >= 1);
        }

        #[tokio::test]
        async fn rate_limited_429_retries_until_max_attempts() {
            let requests = Arc::new(AtomicUsize::new(0));
            let endpoint = mock_server(
                "429 Too Many Requests",
                0,
                r#"{"ok":false,"parameters":{"retry_after":1}}"#,
                requests.clone(),
            )
            .await;
            let result = client(endpoint, 2, 1_000).send_text(42, "hello").await;
            assert!(result.is_err());
            // max_retries=2 → 最多 max_retries+1 = 3 次请求；must have retried at least once.
            let n = requests.load(Ordering::Relaxed);
            assert!((2..=3).contains(&n), "expected 2..=3 attempts, got {n}");
        }

        #[test]
        fn backoff_delay_sequence_grows_exponentially_then_caps() {
            assert_eq!(backoff_delay_ms(0), 250);
            assert_eq!(backoff_delay_ms(1), 500);
            assert_eq!(backoff_delay_ms(2), 1_000);
            assert_eq!(backoff_delay_ms(3), 2_000);
            assert_eq!(backoff_delay_ms(4), 4_000);
            assert_eq!(backoff_delay_ms(5), 4_000);
            assert_eq!(backoff_delay_ms(100), 4_000);
        }

        #[test]
        fn retry_after_parses_and_clamps() {
            // 正常值：1s → 1000ms
            assert_eq!(
                retry_after_ms(r#"{"parameters":{"retry_after":1}}"#),
                Some(1_000)
            );
            // 缺字段
            assert_eq!(retry_after_ms(r#"{}"#), None);
            assert_eq!(retry_after_ms(r#"{"parameters":{}}"#), None);
            assert_eq!(
                retry_after_ms(r#"{"parameters":{"retry_after":null}}"#),
                None
            );
            // 非数字
            assert_eq!(
                retry_after_ms(r#"{"parameters":{"retry_after":"1"}}"#),
                None
            );
            assert_eq!(retry_after_ms(r#"not json"#), None);
            // 超大值 → 钳到 60s 上限
            assert_eq!(
                retry_after_ms(r#"{"parameters":{"retry_after":999999}}"#),
                Some(60_000)
            );
            // 非正 → None（回退到固定指数退避）
            assert_eq!(retry_after_ms(r#"{"parameters":{"retry_after":0}}"#), None);
            assert_eq!(retry_after_ms(r#"{"parameters":{"retry_after":-3}}"#), None);
        }
    }
}
