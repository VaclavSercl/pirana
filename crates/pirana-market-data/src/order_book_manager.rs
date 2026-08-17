use pirana_core::order_book::OrderBook;
use pirana_core::types::*;
use pirana_core::errors::{PiranaError, PiranaResult};
use std::collections::HashMap;

/// Manages order books for multiple symbols
pub struct OrderBookManager {
    books: HashMap<String, OrderBook>,
    depth: usize,
}

impl OrderBookManager {
    pub fn new(depth: usize) -> Self {
        Self {
            books: HashMap::new(),
            depth,
        }
    }

    /// Get or create an order book for a symbol
    pub fn get_or_create(&mut self, symbol: &str) -> &mut OrderBook {
        self.books.entry(symbol.to_string()).or_insert_with(|| {
            OrderBook::new(Symbol::new(symbol), 0.01)
        })
    }

    /// Process a raw order book update from Bitfinex WebSocket
    /// Format: [PRICE, COUNT, AMOUNT]
    /// COUNT > 0: add/update, COUNT == 0: remove
    /// AMOUNT > 0: bid, AMOUNT < 0: ask
    pub fn process_update(&mut self, data: &[serde_json::Value]) -> PiranaResult<()> {
        // Check if it's a snapshot (array of arrays) or single update
        if data.is_empty() {
            return Ok(());
        }

        if data[0].is_array() {
            // Snapshot — process all levels
            self.process_snapshot(data)
        } else {
            // Single update
            self.process_single_update(data)
        }
    }

    /// Process an order book snapshot
    fn process_snapshot(&mut self, data: &[serde_json::Value]) -> PiranaResult<()> {
        for item in data {
            let arr = item.as_array().ok_or_else(|| {
                PiranaError::MarketData("Invalid snapshot entry".to_string())
            })?;
            self.process_single_update(arr)?;
        }
        Ok(())
    }

    /// Process a single order book update
    fn process_single_update(&mut self, data: &[serde_json::Value]) -> PiranaResult<()> {
        if data.len() < 3 {
            return Err(PiranaError::MarketData(
                "Order book update needs at least 3 elements".to_string(),
            ));
        }

        let price = data[0].as_f64().ok_or_else(|| {
            PiranaError::MarketData("Invalid price in book update".to_string())
        })?;

        let count = data[1].as_i64().ok_or_else(|| {
            PiranaError::MarketData("Invalid count in book update".to_string())
        })?;

        let amount = data[2].as_f64().ok_or_else(|| {
            PiranaError::MarketData("Invalid amount in book update".to_string())
        })?;

        if count == 0 {
            // Remove level
            let side = if amount > 0.0 {
                pirana_core::types::Side::Buy
            } else {
                pirana_core::types::Side::Sell
            };
            // We need to know the symbol — use a default or track by channel
            // For now, use the default symbol
            let book = self.get_or_create("tBTCUSD");
            book.update_level(side, price, 0.0, 0);
        } else {
            // Add/update level
            let side = if amount > 0.0 {
                pirana_core::types::Side::Buy
            } else {
                pirana_core::types::Side::Sell
            };
            let book = self.get_or_create("tBTCUSD");
            book.update_level(side, price, amount.abs(), count as u32);
        }

        Ok(())
    }

    /// Get the current order book snapshot for a symbol
    pub fn get_snapshot(&self, symbol: &str) -> Option<OrderBookSnapshot> {
        self.books.get(symbol).map(|book| {
            let (bids, asks) = book.top_levels(self.depth);
            OrderBookSnapshot {
                symbol: Symbol::new(symbol),
                bids,
                asks,
                timestamp: chrono::Utc::now(),
                sequence: book.sequence(),
            }
        })
    }

    /// Get a reference to an order book
    pub fn get_book(&self, symbol: &str) -> Option<&OrderBook> {
        self.books.get(symbol)
    }

    /// Get a mutable reference to an order book
    pub fn get_book_mut(&mut self, symbol: &str) -> Option<&mut OrderBook> {
        self.books.get_mut(symbol)
    }
}
