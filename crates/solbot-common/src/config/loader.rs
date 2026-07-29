use super::base_config::BaseConfig;
use super::tier_params::TierParams;
use anyhow::{Context, Result};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub base: BaseConfig,
    pub micro_tier: TierParams,
    pub small_tier: TierParams,
    pub medium_tier: TierParams,
    pub large_tier: TierParams,
}

pub fn load_all_configs(config_dir: &Path) -> Result<AppConfig> {
    let base = load_yaml::<BaseConfig>(&config_dir.join("base.yaml"))?;
    let micro_tier = load_yaml::<TierParams>(&config_dir.join("tiers/micro.yaml"))?;
    let small_tier = load_yaml::<TierParams>(&config_dir.join("tiers/small.yaml"))?;
    let medium_tier = load_yaml::<TierParams>(&config_dir.join("tiers/medium.yaml"))?;
    let large_tier = load_yaml::<TierParams>(&config_dir.join("tiers/large.yaml"))?;

    Ok(AppConfig {
        base,
        micro_tier,
        small_tier,
        medium_tier,
        large_tier,
    })
}

fn load_yaml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read file: {:?}", path))?;
    serde_yaml::from_str(&content).with_context(|| format!("Failed to parse YAML: {:?}", path))
}
