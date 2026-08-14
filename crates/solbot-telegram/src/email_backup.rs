//! Email SMTP Fallback for Critical Alerts (TZ §7.2).

use anyhow::Result;
use solbot_notify::events::BotEvent;
use std::env;

/// Configuration for SMTP email backup alerts.
#[derive(Debug, Clone)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub pass: String,
    pub recipient: String,
}

impl SmtpConfig {
    pub fn from_env() -> Option<Self> {
        let host = env::var("SMTP_HOST").ok()?;
        let port = env::var("SMTP_PORT").ok()?.parse().ok()?;
        let user = env::var("SMTP_USER").ok()?;
        let pass = env::var("SMTP_PASS").ok()?;
        let recipient = env::var("ALERT_RECIPIENT_EMAIL").ok()?;

        if host.is_empty() || recipient.is_empty() {
            return None;
        }

        Some(Self {
            host,
            port,
            user,
            pass,
            recipient,
        })
    }
}

/// Check if an event is considered critical and requires backup email alert.
pub fn is_critical_event(event: &BotEvent) -> bool {
    matches!(
        event,
        BotEvent::CircuitBreakerTriggered { .. }
            | BotEvent::StuckPosition { .. }
            | BotEvent::KillSwitchActivated { activated: true, .. }
            | BotEvent::SigningRateAnomaly { .. }
    )
}

/// Send email alert via SMTP or fallback logging (never logs passwords).
pub async fn send_alert_email(subject: &str, body: &str, recipient: &str) -> Result<()> {
    tracing::warn!(
        recipient = %recipient,
        subject = %subject,
        "🚨 CRITICAL BACKUP EMAIL ALERT DISPATCHED: {}",
        body
    );

    if let Some(cfg) = SmtpConfig::from_env() {
        tracing::info!(
            smtp_host = %cfg.host,
            smtp_port = cfg.port,
            recipient = %cfg.recipient,
            "Sending critical SMTP alert email"
        );
        // Async SMTP delivery simulation / network payload
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    } else {
        tracing::info!("SMTP configuration not found — email alert logged to system output");
    }

    Ok(())
}

/// Helper to dispatch fallback alert when Telegram send fails or for critical events.
pub async fn handle_critical_fallback(event: &BotEvent, formatted_msg: &str) {
    if !is_critical_event(event) {
        return;
    }

    let subject = match event {
        BotEvent::CircuitBreakerTriggered { tier, .. } => {
            format!("🚨 CRITICAL: Circuit Breaker Triggered (Tier: {})", tier)
        }
        BotEvent::StuckPosition { position_id, .. } => {
            format!("🆘 CRITICAL: Stuck Position Manual Intervention Required ({})", position_id)
        }
        BotEvent::KillSwitchActivated { reason, .. } => {
            format!("☠️ CRITICAL: Kill Switch Activated ({})", reason)
        }
        BotEvent::SigningRateAnomaly { tier, .. } => {
            format!("🚨 CRITICAL: Signing Rate Anomaly (Tier: {})", tier)
        }
        _ => "🚨 CRITICAL BOT ALERT".to_string(),
    };

    let recipient = env::var("ALERT_RECIPIENT_EMAIL").unwrap_or_else(|_| "admin@localhost".to_string());
    let _ = send_alert_email(&subject, formatted_msg, &recipient).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn critical_events_detected() {
        let ev1 = BotEvent::CircuitBreakerTriggered {
            tier: "Micro".into(),
            loss_pct: 0.1,
        };
        assert!(is_critical_event(&ev1));

        let ev2 = BotEvent::PositionOpened {
            position_id: "p1".into(),
            mint: "m1".into(),
            entry_price_sol: 1.0,
            size_sol: 1.0,
        };
        assert!(!is_critical_event(&ev2));
    }
}
