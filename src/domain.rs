//! Channel-independent domain values. Telegram/teloxide types must not cross this boundary.

pub mod jmap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub sender: String,
    pub subject: String,
    pub received_at: String,
}
