use dashmap::DashMap;
use solbot_common::types::tier::CapitalTier;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct CircuitBreakerStatus {
    pub daily_loss_pct: f64,
    pub warning_triggered: bool,
    pub limit_triggered: bool,
}

#[derive(Clone)]
pub struct CircuitBreaker {
    tier_losses: Arc<DashMap<CapitalTier, f64>>,
    warning_thresholds: Arc<DashMap<CapitalTier, f64>>,
    limit_thresholds: Arc<DashMap<CapitalTier, f64>>,
}

impl CircuitBreaker {
    pub fn new() -> Self {
        let cb = Self {
            tier_losses: Arc::new(DashMap::new()),
            warning_thresholds: Arc::new(DashMap::new()),
            limit_thresholds: Arc::new(DashMap::new()),
        };

        // Initialize default thresholds per tier from Appendix A
        cb.warning_thresholds.insert(CapitalTier::Micro, 0.15);
        cb.limit_thresholds.insert(CapitalTier::Micro, 0.25);

        cb.warning_thresholds.insert(CapitalTier::Small, 0.16);
        cb.limit_thresholds.insert(CapitalTier::Small, 0.24);

        cb.warning_thresholds.insert(CapitalTier::Medium, 0.15);
        cb.limit_thresholds.insert(CapitalTier::Medium, 0.216);

        cb.warning_thresholds.insert(CapitalTier::Large, 0.12);
        cb.limit_thresholds.insert(CapitalTier::Large, 0.166);

        cb
    }

    pub fn record_loss(&self, tier: CapitalTier, loss_pct: f64) -> CircuitBreakerStatus {
        let mut entry = self.tier_losses.entry(tier).or_insert(0.0);
        *entry += loss_pct;
        let current_loss = *entry;

        let warning_threshold = self.warning_thresholds.get(&tier).map(|v| *v).unwrap_or(0.15);
        let limit_threshold = self.limit_thresholds.get(&tier).map(|v| *v).unwrap_or(0.25);

        let warning_triggered = current_loss >= warning_threshold;
        let limit_triggered = current_loss >= limit_threshold;

        if limit_triggered {
            tracing::error!(
                tier = ?tier,
                current_loss_pct = current_loss,
                limit_threshold = limit_threshold,
                "Circuit breaker daily loss limit TRIGGERED!"
            );
        } else if warning_triggered {
            tracing::warn!(
                tier = ?tier,
                current_loss_pct = current_loss,
                warning_threshold = warning_threshold,
                "Circuit breaker warning threshold hit"
            );
        }

        CircuitBreakerStatus {
            daily_loss_pct: current_loss,
            warning_triggered,
            limit_triggered,
        }
    }

    pub fn reset_daily(&self) {
        self.tier_losses.clear();
        tracing::info!("Circuit breaker daily losses reset");
    }

    pub fn get_status(&self, tier: CapitalTier) -> CircuitBreakerStatus {
        let loss = self.tier_losses.get(&tier).map(|v| *v).unwrap_or(0.0);
        let warning_threshold = self.warning_thresholds.get(&tier).map(|v| *v).unwrap_or(0.15);
        let limit_threshold = self.limit_thresholds.get(&tier).map(|v| *v).unwrap_or(0.25);

        CircuitBreakerStatus {
            daily_loss_pct: loss,
            warning_triggered: loss >= warning_threshold,
            limit_triggered: loss >= limit_threshold,
        }
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new()
    }
}
