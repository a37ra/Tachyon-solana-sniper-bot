use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlEvent {
    MigrationEvent {
        mint: String,
        new_pool_address: String,
    },
    KillSwitch {
        activated: bool,
        reason: String,
    },
    TpTriggerRecompute {
        position_id: String,
    },
    SlTriggerRecompute {
        position_id: String,
    },
    ManualCliCommand {
        command: String,
        args: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MarketDataEvent {
    PriceTick {
        mint: String,
        pool_address: String,
        price_sol: f64,
        timestamp: u64,
    },
    LiquidityUpdate {
        mint: String,
        pool_address: String,
        liquidity_usd: f64,
    },
}
