use anyhow::Result;
use solbot_common::types::event::PositionLifecycleEvent;
use solbot_common::types::position::{Position, PositionStatus};
use crate::exit::{ExitAction, TpLevel};
use crate::state::position_lifecycle::PositionLifecycle;
use std::sync::mpsc::Sender;

/// Position lifecycle manager with event emission.
///
/// Wraps `PositionLifecycle` and emits `PositionLifecycleEvent` messages
/// to the event bus for each state transition.
pub struct PositionLifecycleManager {
    lifecycle: PositionLifecycle,
    event_tx: Sender<PositionLifecycleEvent>,
}

impl PositionLifecycleManager {
    pub fn new(lifecycle: PositionLifecycle, event_tx: Sender<PositionLifecycleEvent>) -> Self {
        Self { lifecycle, event_tx }
    }

    /// Handle position opening (Opening → Open).
    pub async fn on_position_opened(&self, position: &Position) -> Result<()> {
        self.lifecycle.transition(position, PositionStatus::Open, None).await?;

        self.emit_event(PositionLifecycleEvent::PositionOpened {
            position_id: position.position_id.clone(),
            mint: position.mint.clone(),
            entry_price_sol: position.entry_price_sol,
            size_sol: position.size_sol,
        });

        tracing::info!(
            position_id = %position.position_id,
            "Position opened"
        );

        Ok(())
    }

    /// Handle take profit fill.
    pub async fn on_take_profit(
        &self,
        position: &Position,
        tp_level: TpLevel,
        current_price: f64,
        realized_pnl: f64,
    ) -> Result<()> {
        let new_status = crate::exit::take_profit_ladder::next_status_after_tp(tp_level);
        self.lifecycle.transition(position, new_status, None).await?;

        let tp_level_str = match tp_level {
            TpLevel::Tp1 => "tp1",
            TpLevel::Tp2 => "tp2",
            TpLevel::Tp3 => "tp3",
        };

        let sold_pct = tp_level.sell_pct();
        let remaining_pct = crate::exit::take_profit_ladder::remaining_after_tp(tp_level);

        self.emit_event(PositionLifecycleEvent::TakeProfitFilled {
            position_id: position.position_id.clone(),
            tp_level: tp_level_str.to_string(),
            price_sol: current_price,
            sold_pct,
            remaining_pct,
            realized_pnl_sol: realized_pnl,
        });

        tracing::info!(
            position_id = %position.position_id,
            tp_level = %tp_level_str,
            price = %current_price,
            sold_pct = %sold_pct,
            remaining_pct = %remaining_pct,
            pnl = %realized_pnl,
            "Take profit filled"
        );

        Ok(())
    }

    /// Handle stop loss trigger.
    pub async fn on_stop_loss(&self, position: &Position, current_price: f64) -> Result<()> {
        let _transition = self.lifecycle.transition(position, PositionStatus::Closing, None).await?;

        self.emit_event(PositionLifecycleEvent::StopLossTriggered {
            position_id: position.position_id.clone(),
            trigger_price_sol: current_price,
            entry_price_sol: position.entry_price_sol,
        });

        tracing::warn!(
            position_id = %position.position_id,
            trigger_price = %current_price,
            entry_price = %position.entry_price_sol,
            "Stop loss triggered"
        );

        Ok(())
    }

    /// Handle trailing stop trigger.
    pub async fn on_trailing_stop(
        &self,
        position: &Position,
        trigger_price: f64,
        high_water: f64,
    ) -> Result<()> {
        let _transition = self.lifecycle.transition(position, PositionStatus::Closing, None).await?;

        self.emit_event(PositionLifecycleEvent::TrailingStopTriggered {
            position_id: position.position_id.clone(),
            trigger_price_sol: trigger_price,
            high_water_sol: high_water,
        });

        tracing::info!(
            position_id = %position.position_id,
            trigger_price = %trigger_price,
            high_water = %high_water,
            "Trailing stop triggered"
        );

        Ok(())
    }

    /// Handle position closure.
    pub async fn on_position_closed(
        &self,
        position: &Position,
        tx_signature: String,
    ) -> Result<()> {
        let _transition = self.lifecycle.transition(position, PositionStatus::Closed, Some(tx_signature.clone())).await?;

        let duration = position.closed_at_utc.unwrap_or_else(|| chrono::Utc::now().timestamp() as u64)
            .saturating_sub(position.opened_at_utc);

        self.emit_event(PositionLifecycleEvent::PositionClosed {
            position_id: position.position_id.clone(),
            final_status: crate::state::positions_repo::status_to_string(&PositionStatus::Closed).to_string(),
            total_pnl_sol: position.realized_pnl_net_sol,
            duration_seconds: duration,
        });

        tracing::info!(
            position_id = %position.position_id,
            total_pnl = %position.realized_pnl_net_sol,
            duration_sec = %duration,
            tx_signature = %tx_signature,
            "Position closed"
        );

        Ok(())
    }

    /// Handle manual intervention required.
    pub async fn on_manual_intervention_required(
        &self,
        position: &Position,
        reason: String,
    ) -> Result<()> {
        self.lifecycle.transition_with_reason(
            position,
            PositionStatus::StuckManualInterventionRequired,
            reason.clone(),
            None,
        ).await?;

        self.emit_event(PositionLifecycleEvent::ManualInterventionRequired {
            position_id: position.position_id.clone(),
            reason: reason.clone(),
            current_status: crate::state::positions_repo::status_to_string(&position.status).to_string(),
        });

        tracing::warn!(
            position_id = %position.position_id,
            reason = %reason,
            "Manual intervention required"
        );

        Ok(())
    }

    /// Handle position stuck with escalation.
    pub async fn on_position_stuck(
        &self,
        position: &Position,
        reason: String,
        escalation_stage: u32,
    ) -> Result<()> {
        self.lifecycle.transition_with_reason(
            position,
            PositionStatus::StuckManualInterventionRequired,
            reason.clone(),
            None,
        ).await?;

        self.emit_event(PositionLifecycleEvent::PositionStuck {
            position_id: position.position_id.clone(),
            reason: reason.clone(),
            escalation_stage,
        });

        tracing::error!(
            position_id = %position.position_id,
            reason = %reason,
            escalation_stage = %escalation_stage,
            "Position stuck, escalation started"
        );

        Ok(())
    }

    /// Handle manual resolution of stuck position.
    pub async fn on_manual_resolved(&self, position: &Position, tx_signature: String) -> Result<()> {
        let _transition = self.lifecycle.transition(position, PositionStatus::Closed, Some(tx_signature.clone())).await?;

        let duration = position.closed_at_utc.unwrap_or_else(|| chrono::Utc::now().timestamp() as u64)
            .saturating_sub(position.opened_at_utc);

        self.emit_event(PositionLifecycleEvent::PositionClosed {
            position_id: position.position_id.clone(),
            final_status: crate::state::positions_repo::status_to_string(&PositionStatus::Closed).to_string(),
            total_pnl_sol: position.realized_pnl_net_sol,
            duration_seconds: duration,
        });

        tracing::info!(
            position_id = %position.position_id,
            total_pnl = %position.realized_pnl_net_sol,
            duration_sec = %duration,
            tx_signature = %tx_signature,
            "Position manually resolved"
        );

        Ok(())
    }

    /// Process an exit action from the exit strategy.
    ///
    /// This is the main integration point between the exit strategy
    /// and the position lifecycle.
    pub async fn process_exit_action(
        &self,
        position: &Position,
        exit_action: &ExitAction,
        current_price: f64,
    ) -> Result<()> {
        match exit_action {
            ExitAction::TakeProfit { level, .. } => {
                // Estimate PnL (simplified - should be calculated from actual sell)
                let pnl_pct = (current_price / position.entry_price_sol - 1.0) * level.sell_pct();
                let realized_pnl = position.size_sol * pnl_pct;
                
                self.on_take_profit(position, *level, current_price, realized_pnl).await
            }
            ExitAction::TrailingStop { trigger_price, high_water_mark } => {
                self.on_trailing_stop(position, *trigger_price, *high_water_mark).await
            }
            ExitAction::HardStopLoss => {
                self.on_stop_loss(position, current_price).await
            }
            ExitAction::Emergency { reason } => {
                self.on_manual_intervention_required(position, reason.clone()).await
            }
        }
    }

    /// Emit an event to the event bus.
    fn emit_event(&self, event: PositionLifecycleEvent) {
        if let Err(e) = self.event_tx.send(event) {
            tracing::error!(error = %e, "Failed to emit position lifecycle event");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use solbot_common::types::tier::CapitalTier;
    use std::sync::mpsc::channel;

    fn make_test_position(status: PositionStatus) -> Position {
        Position {
            position_id: "test-pos-1".to_string(),
            mint: "TestMint111111111111111111111111111111111".to_string(),
            pool_address: "TestPool111111111111111111111111111111111".to_string(),
            dex_program_id: "675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8".to_string(),
            tier: CapitalTier::Micro,
            status,
            entry_price_sol: 1.0,
            size_sol: 0.5,
            size_pct_of_capital: 0.05,
            remaining_token_pct: 1.0,
            hard_sl_price_sol: 0.5,
            trailing_active: false,
            trailing_high_water_sol: None,
            realized_pnl_net_sol: 0.0,
            opened_at_utc: 1000000,
            closed_at_utc: None,
        }
    }

    #[test]
    fn test_event_emission() {
        let (tx, rx) = channel();
        
        // Verify the channel works for event emission
        let event = PositionLifecycleEvent::PositionOpened {
            position_id: "test".to_string(),
            mint: "mint".to_string(),
            entry_price_sol: 1.0,
            size_sol: 0.5,
        };
        
        tx.send(event).unwrap();
        let received = rx.recv().unwrap();
        
        assert!(matches!(received, PositionLifecycleEvent::PositionOpened { .. }));
    }
}
