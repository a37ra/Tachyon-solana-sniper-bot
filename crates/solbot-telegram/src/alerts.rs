use super::markdown_v2::escape_markdown_v2;
use solbot_notify::events::BotEvent;

pub fn format_event(event: &BotEvent) -> String {
    match event {
        BotEvent::PositionOpened { position_id, mint, entry_price_sol, size_sol } => {
            format!(
                "🟢 *Position Opened*\n• ID: `{}`\n• Mint: `{}`\n• Entry: `{:.4} SOL`\n• Size: `{:.2} SOL`",
                escape_markdown_v2(position_id),
                escape_markdown_v2(mint),
                entry_price_sol,
                size_sol
            )
        }
        BotEvent::PositionClosed { position_id, mint, pnl_net_sol, reason } => {
            let icon = if *pnl_net_sol >= 0.0 { "🟢" } else { "🔴" };
            format!(
                "{} *Position Closed*\n• ID: `{}`\n• Mint: `{}`\n• PnL: `{:.4} SOL`\n• Reason: `{}`",
                icon,
                escape_markdown_v2(position_id),
                escape_markdown_v2(mint),
                pnl_net_sol,
                escape_markdown_v2(reason)
            )
        }
        BotEvent::TpTriggered { position_id, tp_level, sell_fraction } => {
            format!(
                "🎯 *Take Profit Triggered*\n• ID: `{}`\n• Level: `{}`\n• Fraction: `{:.2}%`",
                escape_markdown_v2(position_id),
                escape_markdown_v2(tp_level),
                sell_fraction * 100.0
            )
        }
        BotEvent::StopLossTriggered { position_id, trigger_price_sol } => {
            format!(
                "🛑 *Stop Loss Triggered*\n• ID: `{}`\n• Trigger Price: `{:.4} SOL`",
                escape_markdown_v2(position_id),
                trigger_price_sol
            )
        }
        BotEvent::EscalationStageChanged { position_id, from_stage, to_stage } => {
            format!(
                "⚠️ *Escalation Stage Changed*\n• ID: `{}`\n• `{}` ➔ `{}`",
                escape_markdown_v2(position_id),
                escape_markdown_v2(from_stage),
                escape_markdown_v2(to_stage)
            )
        }
        BotEvent::CircuitBreakerWarning { tier, loss_pct } => {
            format!(
                "⚠️ *Circuit Breaker Warning*\n• Tier: `{}`\n• Loss: `{:.2}%`",
                escape_markdown_v2(tier),
                loss_pct * 100.0
            )
        }
        BotEvent::CircuitBreakerTriggered { tier, loss_pct } => {
            format!(
                "🚨 *CIRCUIT BREAKER TRIGGERED*\n• Tier: `{}`\n• Loss: `{:.2}%`",
                escape_markdown_v2(tier),
                loss_pct * 100.0
            )
        }
        BotEvent::SigningRateAnomaly { tier, rate } => {
            format!(
                "🚨 *Signing Rate Anomaly*\n• Tier: `{}`\n• Rate: `{}/5min`",
                escape_markdown_v2(tier),
                rate
            )
        }
        BotEvent::SmartMoneyHit { wallet, mint, tx_signature } => {
            format!(
                "🧠 *Smart Money Hit*\n• Wallet: `{}`\n• Mint: `{}`\n• Tx: `{}`",
                escape_markdown_v2(wallet),
                escape_markdown_v2(mint),
                escape_markdown_v2(tx_signature)
            )
        }
        BotEvent::KillSwitchActivated { activated, reason } => {
            format!(
                "☠️ *Kill Switch Activated*\n• Active: `{}`\n• Reason: `{}`",
                activated,
                escape_markdown_v2(reason)
            )
        }
        BotEvent::StuckPosition { position_id, mint, stage } => {
            format!(
                "🆘 *STUCK POSITION MANUAL INTERVENTION REQUIRED*\n• ID: `{}`\n• Mint: `{}`\n• Stage: `{}`",
                escape_markdown_v2(position_id),
                escape_markdown_v2(mint),
                escape_markdown_v2(stage)
            )
        }
    }
}
