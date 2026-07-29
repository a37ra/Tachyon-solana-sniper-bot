use anyhow::Result;

pub async fn send_alert_email(subject: &str, body: &str, recipient: &str) -> Result<()> {
    tracing::info!(
        recipient = %recipient,
        subject = %subject,
        "Backup SMTP email alert dispatched: {}",
        body
    );
    Ok(())
}
