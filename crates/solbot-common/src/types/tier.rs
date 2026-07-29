use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CapitalTier {
    Micro,
    Small,
    Medium,
    Large,
}

impl std::fmt::Display for CapitalTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CapitalTier::Micro => write!(f, "micro"),
            CapitalTier::Small => write!(f, "small"),
            CapitalTier::Medium => write!(f, "medium"),
            CapitalTier::Large => write!(f, "large"),
        }
    }
}
