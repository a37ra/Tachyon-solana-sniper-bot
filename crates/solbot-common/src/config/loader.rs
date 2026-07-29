use std::path::Path;
use anyhow::{Context, Result};
use super::tier_params::TierParams;

pub fn load_tier_params<P: AsRef<Path>>(path: P) -> Result<TierParams> {
    let file_content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read tier config file: {:?}", path.as_ref()))?;
    
    // For now we assume YAML parsing since files are .yaml
    let params: TierParams = serde_yaml::from_str(&file_content)
        .with_context(|| format!("Failed to parse tier config file: {:?}", path.as_ref()))?;
        
    Ok(params)
}
