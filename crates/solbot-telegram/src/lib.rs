pub mod admin_commands;
pub mod alerts;
pub mod client;
pub mod email_backup;
pub mod markdown_v2;

pub use client::TelegramClient;
pub use email_backup::{handle_critical_fallback, send_alert_email};
