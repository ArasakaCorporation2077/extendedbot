use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use parking_lot::RwLock;
use std::collections::VecDeque;
use std::time::Instant;

pub struct CircuitBreaker {
    state: RwLock<CircuitBreakerState>,
    config: CircuitBreakerConfig,
}

pub struct CircuitBreakerConfig {
    pub max_daily_loss_usd: Decimal,
    pub max_errors_per_minute: u32,
    pub max_orders_per_minute: u32,
    pub cooldown_s: u64,
}

struct CircuitBreakerState {
    daily_pnl: Decimal,
    is_tripped: bool,
    trip_reason: Option<String>,
    tripped_at: Option<Instant>,
    recent_errors: VecDeque<Instant>,
    recent_orders: VecDeque<Instant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BreakerStatus {
    Normal,
    Tripped(String),
    Cooldown,
}

impl CircuitBreaker {
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            state: RwLock::new(CircuitBreakerState {
                daily_pnl: Decimal::ZERO,
                is_tripped: false,
                trip_reason: None,
                tripped_at: None,
                recent_errors: VecDeque::new(),
                recent_orders: VecDeque::new(),
            }),
            config,
        }
    }

    pub fn is_trading_allowed(&self) -> bool {
        let state = self.state.read();
        if !state.is_tripped { return true; }
        if let Some(tripped_at) = state.tripped_at {
            if tripped_at.elapsed().as_secs() > self.config.cooldown_s {
                drop(state);
                self.reset();
                return true;
            }
        }
        false
    }

    pub fn status(&self) -> BreakerStatus {
        let state = self.state.read();
        if !state.is_tripped {
            BreakerStatus::Normal
        } else if let Some(tripped_at) = state.tripped_at {
            if tripped_at.elapsed().as_secs() > self.config.cooldown_s {
                BreakerStatus::Cooldown
            } else {
                BreakerStatus::Tripped(state.trip_reason.clone().unwrap_or_default())
            }
        } else {
            BreakerStatus::Tripped("Unknown".to_string())
        }
    }

    pub fn record_pnl(&self, pnl: Decimal) {
        let mut state = self.state.write();
        state.daily_pnl += pnl;
        if state.daily_pnl < -self.config.max_daily_loss_usd {
            state.is_tripped = true;
            state.trip_reason = Some(format!(
                "Daily loss limit breached: ${} (limit: ${})",
                state.daily_pnl, self.config.max_daily_loss_usd
            ));
            state.tripped_at = Some(Instant::now());
        }
    }

    pub fn record_error(&self) {
        let mut state = self.state.write();
        let now = Instant::now();
        state.recent_errors.push_back(now);
        while let Some(&ts) = state.recent_errors.front() {
            if now.duration_since(ts).as_secs() > 60 {
                state.recent_errors.pop_front();
            } else { break; }
        }
        if state.recent_errors.len() as u32 > self.config.max_errors_per_minute {
            state.is_tripped = true;
            state.trip_reason = Some(format!(
                "Error rate exceeded: {}/min (limit: {})",
                state.recent_errors.len(), self.config.max_errors_per_minute
            ));
            state.tripped_at = Some(Instant::now());
        }
    }

    pub fn record_order(&self) {
        let mut state = self.state.write();
        let now = Instant::now();
        state.recent_orders.push_back(now);
        while let Some(&ts) = state.recent_orders.front() {
            if now.duration_since(ts).as_secs() > 60 {
                state.recent_orders.pop_front();
            } else { break; }
        }
        if state.recent_orders.len() as u32 > self.config.max_orders_per_minute {
            state.is_tripped = true;
            state.trip_reason = Some("Order rate limit exceeded".to_string());
            state.tripped_at = Some(Instant::now());
        }
    }

    pub fn trip(&self, reason: &str) {
        let mut state = self.state.write();
        state.is_tripped = true;
        state.trip_reason = Some(reason.to_string());
        state.tripped_at = Some(Instant::now());
    }

    pub fn reset(&self) {
        let mut state = self.state.write();
        state.is_tripped = false;
        state.trip_reason = None;
        state.tripped_at = None;
    }

    pub fn reset_daily(&self) {
        self.state.write().daily_pnl = Decimal::ZERO;
    }

    pub fn daily_pnl(&self) -> Decimal {
        self.state.read().daily_pnl
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> CircuitBreakerConfig {
        CircuitBreakerConfig {
            max_daily_loss_usd: dec!(5000),
            max_errors_per_minute: 20,
            max_orders_per_minute: 200,
            cooldown_s: 300,
        }
    }

    #[test]
    fn test_normal_operation() {
        let cb = CircuitBreaker::new(test_config());
        assert!(cb.is_trading_allowed());
        cb.record_pnl(dec!(-100));
        assert!(cb.is_trading_allowed());
    }

    #[test]
    fn test_loss_limit_trip() {
        let cb = CircuitBreaker::new(test_config());
        cb.record_pnl(dec!(-5001));
        assert!(!cb.is_trading_allowed());
    }

    #[test]
    fn test_manual_trip_and_reset() {
        let cb = CircuitBreaker::new(test_config());
        cb.trip("Manual halt");
        assert!(!cb.is_trading_allowed());
        cb.reset();
        assert!(cb.is_trading_allowed());
    }
}
