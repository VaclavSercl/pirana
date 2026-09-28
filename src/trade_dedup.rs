//! # Trade Event Deduplication (F04)
//!
//! Deduplicates te/tu messages by exchange trade ID before recording or feature updates.
//!
//! ## Core Invariants
//! 1. **ID-based dedup** — te and tu for the same trade ID count only once.
//! 2. **Set-based tracking** — Track seen trade IDs with configurable retention.
//! 3. **Automatic eviction** — FIFO eviction when retention limit is reached.
//!
//! ## Why Dedup Is Needed
//! Bitfinex sends:
//! - `te` (trade entry) — initial trade notification
//! - `tu` (trade update) — confirmation with same ID
//!
//! Without dedup, features (OFI, Flow, Hawkes, VPIN) update twice per trade,
//! inflating volumes and distorting signals.

use std::collections::VecDeque;

/// Deduplicates trade events by exchange trade ID.
///
/// # F04 Invariants
/// - `check_and_remember(id)` returns true if the ID is new, false if already seen.
/// - When the retention limit is reached, the oldest ID is evicted (FIFO).
#[derive(Debug)]
pub struct TradeDedup {
    seen: VecDeque<u64>,
    retention_limit: usize,
}

impl TradeDedup {
    pub fn new(retention_limit: usize) -> Self {
        Self {
            seen: VecDeque::with_capacity(retention_limit.min(10000)),
            retention_limit,
        }
    }

    /// Check if a trade ID is new and remember it.
    /// Returns true if the ID was not previously seen (first occurrence).
    /// Returns false if this ID was already processed (duplicate).
    pub fn check_and_remember(&mut self, trade_id: u64) -> bool {
        if self.seen.contains(&trade_id) {
            false
        } else {
            if self.seen.len() >= self.retention_limit {
                self.seen.pop_front();
            }
            self.seen.push_back(trade_id);
            true
        }
    }

    /// Check if an ID was already seen (without remembering).
    pub fn contains(&self, trade_id: u64) -> bool {
        self.seen.contains(&trade_id)
    }

    /// Current number of tracked IDs.
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }

    /// Clear all tracked IDs.
    pub fn clear(&mut self) {
        self.seen.clear();
    }
}

impl Default for TradeDedup {
    fn default() -> Self {
        Self::new(10000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trade_dedup_first_occurrence() {
        let mut dedup = TradeDedup::new(1000);
        assert!(dedup.check_and_remember(1));
        assert!(dedup.check_and_remember(2));
        assert!(dedup.check_and_remember(3));
        assert_eq!(dedup.len(), 3);
    }

    #[test]
    fn test_trade_dedup_duplicate_rejected() {
        let mut dedup = TradeDedup::new(1000);
        assert!(dedup.check_and_remember(42));
        assert!(!dedup.check_and_remember(42)); // duplicate
        assert!(!dedup.check_and_remember(42)); // still duplicate
        assert_eq!(dedup.len(), 1);
    }

    #[test]
    fn test_trade_dedup_tu_after_te() {
        // Simulate te with ID 100, then tu with same ID 100
        let mut dedup = TradeDedup::new(1000);
        assert!(dedup.check_and_remember(100)); // te — new
        assert!(!dedup.check_and_remember(100)); // tu — duplicate
    }

    #[test]
    fn test_trade_dedup_different_ids() {
        let mut dedup = TradeDedup::new(1000);
        assert!(dedup.check_and_remember(1));
        assert!(dedup.check_and_remember(2));
        assert!(dedup.check_and_remember(3));
        assert_eq!(dedup.len(), 3);
    }

    #[test]
    fn test_trade_dedup_retention_limit() {
        let mut dedup = TradeDedup::new(3);
        assert!(dedup.check_and_remember(1));
        assert!(dedup.check_and_remember(2));
        assert!(dedup.check_and_remember(3));
        assert_eq!(dedup.len(), 3);

        // Evicts oldest (1)
        assert!(dedup.check_and_remember(4));
        assert_eq!(dedup.len(), 3);
        assert!(!dedup.contains(1));
        assert!(dedup.contains(4));
    }

    #[test]
    fn test_trade_dedup_clear() {
        let mut dedup = TradeDedup::new(1000);
        dedup.check_and_remember(1);
        dedup.check_and_remember(2);
        assert_eq!(dedup.len(), 2);
        dedup.clear();
        assert!(dedup.is_empty());
    }
}
