//! Validated operator ceilings supplied by strategy.toml, separate from measured history.
//! Ověřené stropy operátora ze strategy.toml, oddělené od naměřené historie.
use crate::limits;
use std::io;

#[derive(Debug, Clone)]
pub struct OperatorLimits {
    pub max_aggregate_exposure: f64,
    pub max_single_trade_risk: f64,
    pub max_daily_drawdown: f64,
    pub max_weekly_drawdown: f64,
    pub consecutive_loss_threshold: u32,
    pub vpin_toxicity_threshold: f64,
    pub baseline_pct: f64,
}

impl OperatorLimits {
    pub fn validate(&self) -> io::Result<()> {
        for (name, value, maximum) in [
            ("aggregate exposure", self.max_aggregate_exposure, limits::MAX_AGGREGATE_EXPOSURE),
            ("single trade risk", self.max_single_trade_risk, limits::MAX_SINGLE_TRADE_RISK),
            ("daily drawdown", self.max_daily_drawdown, limits::MAX_DAILY_DRAWDOWN),
            ("weekly drawdown", self.max_weekly_drawdown, limits::MAX_WEEKLY_DRAWDOWN),
            ("baseline percentage", self.baseline_pct, 25.0),
        ] {
            if !value.is_finite() || value <= 0.0 || value > maximum {
                return Err(io::Error::new(io::ErrorKind::InvalidData,
                    format!("invalid operator {name}")));
            }
        }
        if !(1..=limits::CONSECUTIVE_LOSS_THRESHOLD).contains(&self.consecutive_loss_threshold)
            || !self.vpin_toxicity_threshold.is_finite()
            || !(0.30..=0.95).contains(&self.vpin_toxicity_threshold) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid loss or VPIN ceiling"));
        }
        Ok(())
    }
}

// Historical lower-limit fixture; production 1% is separately exercised from strategy.toml.
// Historická fixture s nižším limitem; produkční 1 % se samostatně ověřuje ze strategy.toml.
#[cfg(test)]
pub(crate) fn observed_operator_limits() -> OperatorLimits {
    OperatorLimits {
        max_aggregate_exposure: 0.60, max_single_trade_risk: 0.05,
        max_daily_drawdown: 0.005, max_weekly_drawdown: 0.01165,
        consecutive_loss_threshold: 5, vpin_toxicity_threshold: 0.30,
        baseline_pct: 10.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_or_broader_than_hard_policy_is_rejected() {
        for bad in [f64::NAN, f64::INFINITY, -0.01, 0.0, 0.031] {
            let mut policy = observed_operator_limits();
            policy.max_daily_drawdown = bad;
            assert!(policy.validate().is_err());
        }
        let mut policy = observed_operator_limits();
        policy.consecutive_loss_threshold = 6;
        assert!(policy.validate().is_err());
        policy = observed_operator_limits(); policy.vpin_toxicity_threshold = 0.20;
        assert!(policy.validate().is_err());
        observed_operator_limits().validate().unwrap();
    }
}
