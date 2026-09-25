//! HTTPS OpenAI-compatible summary client. It never persists prompts or results.
use crate::state::{runtime_provider, OutboundConfig, RuntimeConfigProvider};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub enum AiError {
    InvalidEndpoint,
    Request,
    Response,
}

pub struct LlmClient {
    http: reqwest::Client,
    endpoint: String,
    key: SecretString,
    model: String,
    max_chars: usize,
    runtime: RuntimeConfigProvider,
}

impl LlmClient {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "stable client constructor retained for adapter tests"
        )
    )]
    pub fn new(
        endpoint: String,
        key: SecretString,
        model: String,
        max_retries: u8,
        max_chars: usize,
    ) -> Result<Self, AiError> {
        Self::new_with_timeout(endpoint, key, model, max_retries, max_chars, 30_000)
    }

    pub fn new_with_timeout(
        endpoint: String,
        key: SecretString,
        model: String,
        max_retries: u8,
        max_chars: usize,
        timeout_ms: u64,
    ) -> Result<Self, AiError> {
        let url = url::Url::parse(&endpoint).map_err(|_| AiError::InvalidEndpoint)?;
        if url.scheme() != "https" {
            return Err(AiError::InvalidEndpoint);
        }
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_millis(timeout_ms.max(100)))
                .build()
                .map_err(|_| AiError::Request)?,
            endpoint: endpoint.trim_end_matches('/').into(),
            key,
            model,
            max_chars: max_chars.max(1),
            runtime: runtime_provider(OutboundConfig {
                llm_timeout_ms: timeout_ms.max(100),
                max_retries: max_retries.min(5),
                ..OutboundConfig::default()
            }),
        })
    }

    pub fn with_runtime(
        endpoint: String,
        key: SecretString,
        model: String,
        max_chars: usize,
        runtime: RuntimeConfigProvider,
    ) -> Result<Self, AiError> {
        let url = url::Url::parse(&endpoint).map_err(|_| AiError::InvalidEndpoint)?;
        if url.scheme() != "https" {
            return Err(AiError::InvalidEndpoint);
        }
        Ok(Self {
            http: reqwest::Client::new(),
            endpoint: endpoint.trim_end_matches('/').into(),
            key,
            model,
            max_chars: max_chars.max(1),
            runtime,
        })
    }

    pub async fn summarize(&self, body: &str) -> Result<String, AiError> {
        let request = ChatRequest {
            model: &self.model,
            messages: vec![ChatMessage {
                role: "user",
                content: body,
            }],
        };
        let (timeout_ms, max_retries) = self
            .runtime
            .read()
            .map(|config| (config.llm_timeout_ms, config.max_retries.min(5)))
            .unwrap_or((30_000, 3));
        for _ in 0..=max_retries {
            let response = match tokio::time::timeout(
                std::time::Duration::from_millis(timeout_ms.max(100)),
                self.http
                    .post(format!("{}/chat/completions", self.endpoint))
                    .bearer_auth(self.key.expose_secret())
                    .json(&request)
                    .send(),
            )
            .await
            {
                Ok(Ok(response)) => response,
                _ => continue,
            };
            let status = response.status();
            if status.is_success() {
                let value = response
                    .json::<ChatResponse>()
                    .await
                    .map_err(|_| AiError::Response)?;
                let text = value
                    .choices
                    .into_iter()
                    .next()
                    .map(|choice| choice.message.content)
                    .ok_or(AiError::Response)?;
                return Ok(text.chars().take(self.max_chars).collect());
            }
            if !(status.as_u16() == 429 || status.is_server_error()) {
                return Err(AiError::Request);
            }
        }
        Err(AiError::Response)
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage<'a>>,
}
#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'a str,
    content: &'a str,
}
#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}
#[derive(Deserialize)]
struct Choice {
    message: ChatMessageOwned,
}
#[derive(Deserialize)]
struct ChatMessageOwned {
    content: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn llm_requires_https() {
        assert!(matches!(
            LlmClient::new(
                "http://localhost".into(),
                SecretString::new("x".into()),
                "m".into(),
                1,
                300,
            ),
            Err(AiError::InvalidEndpoint)
        ));
    }
}
