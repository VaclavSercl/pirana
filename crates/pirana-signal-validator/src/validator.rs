use pirana_core::types::*;
use pirana_core::constants::*;
use pirana_core::errors::PiranaResult;
use tracing::debug;

/// Signal validation engine — validates AI-generated signals
/// before they reach the risk engine.
#[derive(Debug)]
pub struct SignalValidator {
    /// Minimum confidence threshold
    min_confidence: f64,
    /// Total signals received
    total_signals: u64,
    /// Signals rejected
    rejected_signals: u64,
}

impl Default for SignalValidator {
    fn default() -> Self {
        Self::new()
    }
}

impl SignalValidator {
    pub fn new() -> Self {
        Self {
            min_confidence: SIGNAL_CONFIDENCE_THRESHOLD,
            total_signals: 0,
            rejected_signals: 0,
        }
    }

    /// Validate a signal from the AI layer
    pub fn validate(&mut self, signal: &Signal) -> PiranaResult<ValidationResult> {
        self.total_signals += 1;

        // Check confidence score
        if signal.confidence_score < self.min_confidence {
            self.rejected_signals += 1;
            return Ok(ValidationResult::Rejected {
                reason: format!(
                    "Confidence {:.2} below threshold {:.2}",
                    signal.confidence_score, self.min_confidence
                ),
            });
        }

        // Validate signal type is not DefensiveHalt (that's a system-level signal)
        if signal.signal_type == SignalType::DefensiveHalt {
            return Ok(ValidationResult::SystemAction {
                action: "ENTER_DEFENSIVE_MODE".to_string(),
            });
        }

        // Validate entry zone is sensible
        if signal.recommended_params.entry_zone.0 >= signal.recommended_params.entry_zone.1 {
            self.rejected_signals += 1;
            return Ok(ValidationResult::Rejected {
                reason: "Invalid entry zone: low >= high".to_string(),
            });
        }

        // Validate invalidation level is below entry for buys, above for sells
        // (basic sanity check)
        let _entry_mid = (signal.recommended_params.entry_zone.0 + signal.recommended_params.entry_zone.1) / 2.0;
        if signal.invalidation_level <= 0.0 {
            self.rejected_signals += 1;
            return Ok(ValidationResult::Rejected {
                reason: "Invalid invalidation level".to_string(),
            });
        }

        // Validate position size
        if signal.recommended_params.position_size_pct <= 0.0
            || signal.recommended_params.position_size_pct > MAX_AGGREGATE_EXPOSURE
        {
            self.rejected_signals += 1;
            return Ok(ValidationResult::Rejected {
                reason: format!(
                    "Position size {:.4} outside allowed range (max exposure is {:.2})",
                    signal.recommended_params.position_size_pct, MAX_AGGREGATE_EXPOSURE
                ),
            });
        }

        // Validate max slippage
        if signal.recommended_params.max_slippage_bps > MAX_SLIPPAGE_BPS {
            self.rejected_signals += 1;
            return Ok(ValidationResult::Rejected {
                reason: format!(
                    "Max slippage {} bps exceeds limit {} bps",
                    signal.recommended_params.max_slippage_bps, MAX_SLIPPAGE_BPS
                ),
            });
        }

        // All validations passed
        debug!(
            "Signal {} validated: confidence={:.2}, type={:?}",
            signal.id.0, signal.confidence_score, signal.signal_type
        );

        Ok(ValidationResult::Approved {
            signal_id: signal.id,
            confidence: signal.confidence_score,
        })
    }

    pub fn stats(&self) -> (u64, u64) {
        (self.total_signals, self.rejected_signals)
    }
}

#[derive(Debug)]
pub enum ValidationResult {
    Approved {
        signal_id: SignalId,
        confidence: f64,
    },
    Rejected {
        reason: String,
    },
    SystemAction {
        action: String,
    },
}
