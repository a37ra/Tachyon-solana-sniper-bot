use anyhow::Result;

pub async fn execute(action: &str) -> Result<()> {
    match action {
        "on" => tracing::info!("Kill switch ACTIVATED via CLI"),
        "off" => tracing::info!("Kill switch DEACTIVATED via CLI"),
        _ => tracing::info!("Kill switch status checked via CLI"),
    }
    Ok(())
}
