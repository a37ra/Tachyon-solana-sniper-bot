use super::{TradeResult, TradeStats};

pub struct AdaptiveSizer {
    trade_history: Vec<TradeResult>,
    calibration_window: usize,
    min_sample_size: usize,
}

impl AdaptiveSizer {
    pub fn new(calibration_window: usize, min_sample_size: usize) -> Self {
        Self {
            trade_history: Vec::new(),
            calibration_window,
            min_sample_size,
        }
    }

    pub fn add_trade_result(&mut self, result: TradeResult) {
        self.trade_history.push(result);

        if self.trade_history.len() > self.calibration_window {
            self.trade_history.remove(0);
        }
    }

    pub fn recalibrate(&mut self) -> Option<f64> {
        if self.trade_history.len() < self.min_sample_size {
            return None;
        }

        let wins: f64 = self
            .trade_history
            .iter()
            .filter(|t| t.pnl_pct > 0.0)
            .count() as f64;

        let total = self.trade_history.len() as f64;
        let win_rate = wins / total;

        let winning_trades: Vec<f64> = self
            .trade_history
            .iter()
            .filter(|t| t.pnl_pct > 0.0)
            .map(|t| t.pnl_pct)
            .collect();

        let losing_trades: Vec<f64> = self
            .trade_history
            .iter()
            .filter(|t| t.pnl_pct <= 0.0)
            .map(|t| t.pnl_pct.abs())
            .collect();

        let _avg_win = if winning_trades.is_empty() {
            0.0
        } else {
            winning_trades.iter().sum::<f64>() / winning_trades.len() as f64
        };

        let _avg_loss = if losing_trades.is_empty() {
            0.0
        } else {
            losing_trades.iter().sum::<f64>() / losing_trades.len() as f64
        };

        Some(win_rate)
    }

    pub fn get_stats(&self) -> Option<TradeStats> {
        if self.trade_history.len() < self.min_sample_size {
            return None;
        }

        let wins: f64 = self
            .trade_history
            .iter()
            .filter(|t| t.pnl_pct > 0.0)
            .count() as f64;

        let total = self.trade_history.len() as f64;
        let win_rate = wins / total;

        let winning_trades: Vec<f64> = self
            .trade_history
            .iter()
            .filter(|t| t.pnl_pct > 0.0)
            .map(|t| t.pnl_pct)
            .collect();

        let losing_trades: Vec<f64> = self
            .trade_history
            .iter()
            .filter(|t| t.pnl_pct <= 0.0)
            .map(|t| t.pnl_pct.abs())
            .collect();

        let avg_win = if winning_trades.is_empty() {
            0.0
        } else {
            winning_trades.iter().sum::<f64>() / winning_trades.len() as f64
        };

        let avg_loss = if losing_trades.is_empty() {
            0.0
        } else {
            losing_trades.iter().sum::<f64>() / losing_trades.len() as f64
        };

        Some(TradeStats {
            win_rate,
            avg_win,
            avg_loss,
            total_trades: self.trade_history.len() as u32,
        })
    }
}

impl Default for AdaptiveSizer {
    fn default() -> Self {
        Self::new(100, 30)
    }
}
