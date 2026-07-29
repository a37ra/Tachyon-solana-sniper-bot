use anyhow::Result;

pub async fn execute(action: &str, position_id: Option<&str>) -> Result<()> {
    match action {
        "list" => tracing::info!("Listing all positions"),
        "show" => tracing::info!("Showing details for position: {:?}", position_id),
        "force-close" => tracing::info!("Force closing position: {:?}", position_id),
        _ => tracing::info!("Unknown position action"),
    }
    Ok(())
}
