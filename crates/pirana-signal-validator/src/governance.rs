use pirana_core::types::*;
use pirana_core::errors::PiranaResult;

/// Governance layer — ensures signals comply with system rules
/// before passing to the risk engine.
#[derive(Debug)]
pub struct GovernanceEngine {
    /// Whether governance checks are enabled
    enabled: bool,
}

impl Default for GovernanceEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl GovernanceEngine {
    pub fn new() -> Self {
        Self { enabled: true }
    }

    /// Apply governance rules to a validated signal
    pub fn apply_governance(
        &self,
        signal: &Signal,
        current_mode: SystemMode,
    ) -> PiranaResult<GovernanceResult> {
        if !self.enabled {
            return Ok(GovernanceResult::Approved);
        }

        // In Halted mode, no signals pass
        if current_mode == SystemMode::Halted {
            return Ok(GovernanceResult::Denied {
                reason: "System is HALTED".to_string(),
            });
        }

        // In Defensive mode, only Hold and DefensiveHalt signals pass
        if current_mode == SystemMode::Defensive {
            match signal.signal_type {
                SignalType::Hold | SignalType::DefensiveHalt => {
                    return Ok(GovernanceResult::Approved);
                }
                _ => {
                    return Ok(GovernanceResult::Denied {
                        reason: format!(
                            "Signal {:?} not allowed in DEFENSIVE mode",
                            signal.signal_type
                        ),
                    });
                }
            }
        }

        // All checks passed
        Ok(GovernanceResult::Approved)
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
}

#[derive(Debug)]
pub enum GovernanceResult {
    Approved,
    Denied { reason: String },
}
