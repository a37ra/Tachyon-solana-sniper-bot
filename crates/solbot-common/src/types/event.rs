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

/// Position lifecycle events for state transitions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PositionLifecycleEvent {
    /// Position successfully opened
    PositionOpened {
        position_id: String,
        mint: String,
        entry_price_sol: f64,
        size_sol: f64,
    },
    /// Take profit level filled
    TakeProfitFilled {
        position_id: String,
        tp_level: String, // "tp1", "tp2", "tp3"
        price_sol: f64,
        sold_pct: f64,
        remaining_pct: f64,
        realized_pnl_sol: f64,
    },
    /// Stop loss triggered
    StopLossTriggered {
        position_id: String,
        trigger_price_sol: f64,
        entry_price_sol: f64,
    },
    /// Trailing stop triggered
    TrailingStopTriggered {
        position_id: String,
        trigger_price_sol: f64,
        high_water_sol: f64,
    },
    /// Position closed
    PositionClosed {
        position_id: String,
        final_status: String,
        total_pnl_sol: f64,
        duration_seconds: u64,
    },
    /// Manual intervention required
    ManualInterventionRequired {
        position_id: String,
        reason: String,
        current_status: String,
    },
    /// Position stuck (escalation started)
    PositionStuck {
        position_id: String,
        reason: String,
        escalation_stage: u32,
    },
}
