use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BotEvent {
    PositionOpened {
        position_id: String,
        mint: String,
        entry_price_sol: f64,
        size_sol: f64,
    },
    PositionClosed {
        position_id: String,
        mint: String,
        pnl_net_sol: f64,
        reason: String,
    },
    TpTriggered {
        position_id: String,
        tp_level: String,
        sell_fraction: f64,
    },
    StopLossTriggered {
        position_id: String,
        trigger_price_sol: f64,
    },
    EscalationStageChanged {
        position_id: String,
        from_stage: String,
        to_stage: String,
    },
    CircuitBreakerWarning {
        tier: String,
        loss_pct: f64,
    },
    CircuitBreakerTriggered {
        tier: String,
        loss_pct: f64,
    },
    SigningRateAnomaly {
        tier: String,
        rate: u32,
    },
    SmartMoneyHit {
        wallet: String,
        mint: String,
        tx_signature: String,
    },
    KillSwitchActivated {
        activated: bool,
        reason: String,
    },
    StuckPosition {
        position_id: String,
        mint: String,
        stage: String,
    },
}
