//! # WebSocket Channel Registry & Message Router (F01)
//!
//! Provides explicit chanId→channel routing instead of array-shape heuristics.
//!
//! ## Core Invariants
//! 1. **chanId registry** — Explicit map of chanId → ChannelKind. Route by kind, not array shape.
//! 2. **Book isolation** — Only book channels mutate the order book.
//! 3. **Malformed snapshot rejection** — Validate entry structure before mutation.
//! 4. **Reset on reconnect** — Clear registry when WebSocket reconnects.

use crate::order_book::OrderBook;
use crate::types::Side;
use serde::Deserialize;
use std::collections::HashMap;

/// Identifies the type of a WebSocket channel after subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelKind {
    Ticker,
    Book,
    Trades,
}

/// Metadata for a subscribed channel.
#[derive(Debug, Clone)]
pub struct ChannelInfo {
    pub kind: ChannelKind,
    pub symbol: String,
}

/// WebSocket channel registry that maps chanId → ChannelKind.
///
/// # F01 Invariants
/// - Routes messages by channel kind, not array shape.
/// - Only book channels mutate the order book.
/// - Malformed snapshots are rejected.
/// - Registry is cleared on reconnect.
#[derive(Debug)]
pub struct ChannelRegistry {
    channels: HashMap<i64, ChannelInfo>,
    book_sequence: u64,
}

impl ChannelRegistry {
    pub fn new() -> Self {
        Self {
            channels: HashMap::new(),
            book_sequence: 0,
        }
    }

    /// Register a channel with its kind. Called on subscription confirmation.
    pub fn register(&mut self, chan_id: i64, kind: ChannelKind, symbol: String) {
        self.channels.insert(chan_id, ChannelInfo { kind, symbol });
    }

    /// Get the channel kind for a given chanId.
    pub fn kind(&self, chan_id: i64) -> Option<ChannelKind> {
        self.channels.get(&chan_id).map(|c| c.kind)
    }

    /// Get the full channel info for a given chanId.
    pub fn info(&self, chan_id: i64) -> Option<&ChannelInfo> {
        self.channels.get(&chan_id)
    }

    /// Clear all channel registrations (called on reconnect).
    pub fn clear(&mut self) {
        self.channels.clear();
        self.book_sequence = 0;
    }

    /// Get the number of registered channels.
    pub fn len(&self) -> usize {
        self.channels.len()
    }

    pub fn is_empty(&self) -> bool {
        self.channels.is_empty()
    }

    /// Validate a book snapshot entry: [PRICE, COUNT, AMOUNT]
    /// Returns Some((price, count, amount)) if valid, None otherwise.
    pub fn validate_book_entry(entry: &serde_json::Value) -> Option<(f64, u32, f64)> {
        let arr = entry.as_array()?;
        if arr.len() < 3 {
            return None;
        }
        let price = arr[0].as_f64()?;
        let count = arr[1].as_i64()? as u32;
        let amount = arr[2].as_f64()?;
        if !price.is_finite() || price <= 0.0 {
            return None;
        }
        Some((price, count, amount))
    }

    /// Process a book snapshot for a given chanId.
    /// Only book channels are processed; others are ignored.
    /// Returns the number of entries processed.
    pub fn process_book_snapshot(
        &mut self,
        chan_id: i64,
        data: &[serde_json::Value],
        book: &mut OrderBook,
    ) -> Option<usize> {
        // F01: Only book channels mutate the book
        if self.kind(chan_id) != Some(ChannelKind::Book) {
            return None;
        }

        book.clear();
        let mut processed = 0;
        for entry in data {
            // F01: Reject malformed snapshots
            if let Some((price, count, amount)) = Self::validate_book_entry(entry) {
                let side = if amount > 0.0 { Side::Buy } else { Side::Sell };
                book.update_level(side, price, amount.abs(), count);
                processed += 1;
            }
        }
        self.book_sequence += 1;
        Some(processed)
    }

    /// Process a single book update [PRICE, COUNT, AMOUNT] for a given chanId.
    pub fn process_book_update(
        &mut self,
        chan_id: i64,
        data: &[serde_json::Value],
        book: &mut OrderBook,
    ) -> bool {
        // F01: Only book channels mutate the book
        if self.kind(chan_id) != Some(ChannelKind::Book) {
            return false;
        }

        // F01: Reject malformed updates
        if data.len() < 3 {
            return false;
        }

        let price = match data[0].as_f64() {
            Some(p) if p.is_finite() && p > 0.0 => p,
            _ => return false,
        };
        let count = match data[1].as_i64() {
            Some(c) if c >= 0 => c as u32,
            _ => return false,
        };
        let amount = match data[2].as_f64() {
            Some(a) if a.is_finite() => a,
            _ => return false,
        };

        let side = if amount > 0.0 { Side::Buy } else { Side::Sell };
        book.update_level(side, price, amount.abs(), count);
        true
    }
}

impl Default for ChannelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Event received from Bitfinex WebSocket.
#[derive(Debug, Clone, Deserialize)]
pub struct WsEvent {
    pub event: String,
    pub channel: Option<String>,
    pub symbol: Option<String>,
    pub chan_id: Option<i64>,
}

/// Message from Bitfinex WebSocket.
#[derive(Debug, Clone, Deserialize)]
pub struct WsMessage {
    pub chan_id: i64,
    pub payload: serde_json::Value,
}

/// Parse a subscribe confirmation event and return (chan_id, kind, symbol).
pub fn parse_subscribe_confirmation(data: &serde_json::Value) -> Option<(i64, ChannelKind, String)> {
    let event = data.as_object()?;
    let event_type = event.get("event")?.as_str()?;
    if event_type != "subscribed" {
        return None;
    }
    let chan_id = event.get("chanId")?.as_i64()?;
    let channel = event.get("channel")?.as_str()?;
    let symbol = event.get("symbol")?.as_str()?;
    let kind = match channel {
        "ticker" => ChannelKind::Ticker,
        "book" => ChannelKind::Book,
        "trades" => ChannelKind::Trades,
        _ => return None,
    };
    Some((chan_id, kind, symbol.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_registry_register_and_lookup() {
        let mut reg = ChannelRegistry::new();
        reg.register(1, ChannelKind::Ticker, "tBTCUSD".to_string());
        reg.register(2, ChannelKind::Book, "tBTCUSD".to_string());
        reg.register(3, ChannelKind::Trades, "tBTCUSD".to_string());

        assert_eq!(reg.kind(1), Some(ChannelKind::Ticker));
        assert_eq!(reg.kind(2), Some(ChannelKind::Book));
        assert_eq!(reg.kind(3), Some(ChannelKind::Trades));
        assert_eq!(reg.len(), 3);
    }

    #[test]
    fn test_channel_registry_clear() {
        let mut reg = ChannelRegistry::new();
        reg.register(1, ChannelKind::Ticker, "tBTCUSD".to_string());
        assert_eq!(reg.len(), 1);
        reg.clear();
        assert!(reg.is_empty());
    }

    #[test]
    fn test_validate_book_entry_valid() {
        let entry = serde_json::json!([80000.0, 5, 1.5]);
        let result = ChannelRegistry::validate_book_entry(&entry);
        assert_eq!(result, Some((80000.0, 5, 1.5)));
    }

    #[test]
    fn test_validate_book_entry_negative_amount() {
        let entry = serde_json::json!([80000.0, 5, -1.5]);
        let result = ChannelRegistry::validate_book_entry(&entry);
        assert_eq!(result, Some((80000.0, 5, -1.5)));
    }

    #[test]
    fn test_validate_book_entry_malformed() {
        // Missing amount
        let entry = serde_json::json!([80000.0, 5]);
        assert!(ChannelRegistry::validate_book_entry(&entry).is_none());

        // Non-finite price
        let entry = serde_json::json!(["bad", 5, 1.5]);
        assert!(ChannelRegistry::validate_book_entry(&entry).is_none());

        // Zero price
        let entry = serde_json::json!([0.0, 5, 1.5]);
        assert!(ChannelRegistry::validate_book_entry(&entry).is_none());

        // Not an array
        let entry = serde_json::json!({"price": 80000.0});
        assert!(ChannelRegistry::validate_book_entry(&entry).is_none());
    }

    #[test]
    fn test_process_book_snapshot_only_book_channel() {
        use crate::types::Symbol;

        let mut reg = ChannelRegistry::new();
        let mut book = OrderBook::new(Symbol::new("tBTCUSD"), 0.01);

        reg.register(1, ChannelKind::Book, "tBTCUSD".to_string());
        reg.register(2, ChannelKind::Ticker, "tBTCUSD".to_string());

        let snapshot = vec![
            serde_json::json!([80000.0, 5, 1.5]),   // bid (positive amount)
            serde_json::json!([80010.0, 3, -2.0]),  // ask (negative amount)
        ];

        // Book channel: should process
        let processed = reg.process_book_snapshot(1, &snapshot, &mut book);
        assert_eq!(processed, Some(2));
        assert!(book.best_bid().is_some());
        assert!(book.best_ask().is_some());

        // Ticker channel: should NOT process
        let mut book2 = OrderBook::new(Symbol::new("tBTCUSD"), 0.01);
        let result = reg.process_book_snapshot(2, &snapshot, &mut book2);
        assert_eq!(result, None);
        assert!(book2.best_bid().is_none());
    }

    #[test]
    fn test_process_book_snapshot_rejects_malformed() {
        use crate::types::Symbol;

        let mut reg = ChannelRegistry::new();
        let mut book = OrderBook::new(Symbol::new("tBTCUSD"), 0.01);

        reg.register(1, ChannelKind::Book, "tBTCUSD".to_string());

        let snapshot = vec![
            serde_json::json!([80000.0, 5, 1.5]),
            serde_json::json!([80010.0]), // malformed - missing fields
            serde_json::json!(["bad", 3, 2.0]), // malformed - non-finite price
        ];

        let processed = reg.process_book_snapshot(1, &snapshot, &mut book);
        assert_eq!(processed, Some(1)); // Only the valid entry
    }

    #[test]
    fn test_process_book_update() {
        use crate::types::Symbol;

        let mut reg = ChannelRegistry::new();
        let mut book = OrderBook::new(Symbol::new("tBTCUSD"), 0.01);

        reg.register(1, ChannelKind::Book, "tBTCUSD".to_string());

        let update = vec![
            serde_json::json!(80000.0),
            serde_json::json!(5),
            serde_json::json!(1.5),
        ];

        assert!(reg.process_book_update(1, &update, &mut book));
        assert!(book.best_bid().is_some());
    }

    #[test]
    fn test_process_book_update_malformed() {
        use crate::types::Symbol;

        let mut reg = ChannelRegistry::new();
        let mut book = OrderBook::new(Symbol::new("tBTCUSD"), 0.01);

        reg.register(1, ChannelKind::Book, "tBTCUSD".to_string());

        // Missing fields
        let update = vec![serde_json::json!(80000.0), serde_json::json!(5)];
        assert!(!reg.process_book_update(1, &update, &mut book));

        // Non-finite price
        let update = vec![
            serde_json::json!(f64::NAN),
            serde_json::json!(5),
            serde_json::json!(1.5),
        ];
        assert!(!reg.process_book_update(1, &update, &mut book));
    }
}
