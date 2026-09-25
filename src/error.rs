use thiserror::Error;

#[derive(Debug, Error)]
pub enum BotError {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("state error")]
    State(#[from] crate::state::StateError),
}
