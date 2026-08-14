//! Telegram HTTP Client with rate limiting and MarkdownV2 sending.

use anyhow::{anyhow, Result};
use reqwest::Client;
use serde::Serialize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Telegram sendMessage request payload.
#[derive(Debug, Serialize)]
struct SendMessagePayload<'a> {
    chat_id: &'a str,
    text: &'a str,
    parse_mode: &'a str,
    disable_web_page_preview: bool,
}

/// Token bucket rate limiter for Telegram API (max 20 msgs/sec, max 1 msg/sec per chat).
pub struct TelegramRateLimiter {
    last_sent: Mutex<Instant>,
    min_interval: Duration,
}

impl TelegramRateLimiter {
    pub fn new(min_interval: Duration) -> Self {
        Self {
            last_sent: Mutex::new(Instant::now().checked_sub(min_interval).unwrap_or_else(Instant::now)),
            min_interval,
        }
    }

    pub async fn wait_for_slot(&self) {
        let mut last = self.last_sent.lock().await;
        let elapsed = last.elapsed();
        if elapsed < self.min_interval {
            tokio::time::sleep(self.min_interval - elapsed).await;
        }
        *last = Instant::now();
    }
}

impl Default for TelegramRateLimiter {
    fn default() -> Self {
        // Enforce max 1 msg/sec per chat by default (Telegram Bot API limit: 30 msgs/sec global, 1 msg/sec per chat)
        Self::new(Duration::from_millis(1000))
    }
}

/// Telegram HTTP API client.
#[derive(Clone)]
pub struct TelegramClient {
    bot_token: String,
    chat_id: String,
    http: Client,
    rate_limiter: Arc<TelegramRateLimiter>,
}

impl TelegramClient {
    pub fn new(bot_token: impl Into<String>, chat_id: impl Into<String>) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();

        Self {
            bot_token: bot_token.into(),
            chat_id: chat_id.into(),
            http,
            rate_limiter: Arc::new(TelegramRateLimiter::default()),
        }
    }

    pub fn from_env() -> Option<Self> {
        let token = std::env::var("TELEGRAM_BOT_TOKEN").ok()?;
        let chat = std::env::var("TELEGRAM_CHAT_ID").ok()?;
        if token.is_empty() || chat.is_empty() {
            return None;
        }
        Some(Self::new(token, chat))
    }

    pub async fn send_markdown_v2(&self, text: &str) -> Result<()> {
        if self.bot_token.is_empty() || self.chat_id.is_empty() {
            return Err(anyhow!("Telegram credentials not configured"));
        }

        // Wait for rate limiter slot
        self.rate_limiter.wait_for_slot().await;

        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.bot_token);
        let payload = SendMessagePayload {
            chat_id: &self.chat_id,
            text,
            parse_mode: "MarkdownV2",
            disable_web_page_preview: true,
        };

        let response = self.http.post(&url).json(&payload).send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            tracing::warn!(
                status = %status,
                body = %body,
                "Telegram API error response"
            );
            return Err(anyhow!("Telegram API error HTTP {}: {}", status, body));
        }

        tracing::debug!("Telegram MarkdownV2 message sent successfully");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rate_limiter_enforces_interval() {
        let limiter = TelegramRateLimiter::new(Duration::from_millis(50));
        let start = Instant::now();
        limiter.wait_for_slot().await;
        limiter.wait_for_slot().await;
        assert!(start.elapsed() >= Duration::from_millis(45));
    }
}
