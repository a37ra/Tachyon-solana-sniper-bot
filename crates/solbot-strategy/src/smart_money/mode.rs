#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmartMoneyMode {
    TagOnly,
    SoftBoost,
    HardGate,
}

impl SmartMoneyMode {
    pub fn parse(mode_str: &str) -> Self {
        match mode_str.to_lowercase().as_str() {
            "soft_boost" => SmartMoneyMode::SoftBoost,
            "hard_gate" => SmartMoneyMode::HardGate,
            _ => SmartMoneyMode::TagOnly,
        }
    }
}
