use crate::types::{PriceLevel, Side, Symbol};
use std::collections::BTreeMap;

/// Lock-free order book implementation optimized for HFT operations.
/// Uses BTreeMap for O(log n) price level lookups.
#[derive(Debug, Clone)]
pub struct OrderBook {
    symbol: Symbol,
    /// Bids sorted descending (highest first via Reverse)
    bids: BTreeMap<u64, PriceLevel>,
    /// Asks sorted ascending (lowest first)
    asks: BTreeMap<u64, PriceLevel>,
    /// Tick size for price normalization
    tick_size: f64,
    sequence: u64,
}

impl OrderBook {
    pub fn new(symbol: Symbol, tick_size: f64) -> Self {
        Self {
            symbol,
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            tick_size,
            sequence: 0,
        }
    }

    /// Convert price to integer key for BTreeMap ordering
    fn price_to_key(&self, price: f64) -> u64 {
        (price / self.tick_size).round() as u64
    }

    /// Convert integer key back to price
    #[allow(dead_code)]
    fn key_to_price(&self, key: u64) -> f64 {
        key as f64 * self.tick_size
    }

    /// Update a price level in the book
    pub fn update_level(&mut self, side: Side, price: f64, quantity: f64, order_count: u32) {
        let key = self.price_to_key(price);
        let level = PriceLevel {
            price,
            quantity,
            order_count,
        };

        match side {
            Side::Buy => {
                if quantity <= 0.0 || order_count == 0 {
                    self.bids.remove(&key);
                } else {
                    self.bids.insert(key, level);
                }
            }
            Side::Sell => {
                if quantity <= 0.0 || order_count == 0 {
                    self.asks.remove(&key);
                } else {
                    self.asks.insert(key, level);
                }
            }
        }
    }

    /// Get the best bid price
    pub fn best_bid(&self) -> Option<PriceLevel> {
        self.bids.values().next_back().copied()
    }

    /// Get the best ask price
    pub fn best_ask(&self) -> Option<PriceLevel> {
        self.asks.values().next().copied()
    }

    /// Get the bid-ask spread
    pub fn spread(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => Some(ask.price - bid.price),
            _ => None,
        }
    }

    /// Get the mid price
    pub fn mid_price(&self) -> Option<f64> {
        match (self.best_bid(), self.best_ask()) {
            (Some(bid), Some(ask)) => Some((bid.price + ask.price) / 2.0),
            _ => None,
        }
    }

    /// Get the volume-weighted average price that a TAKER order of the given
    /// side would pay for `quantity`:
    /// - taker BUY konzumuje ASK stranu knihy (od nejlevnějšího ask výš),
    /// - taker SELL konzumuje BID stranu knihy (od nejvyššího bid dolů).
    ///
    /// [CASLAV v5.1 / OPONENTURA FIX] Původní implementace měla strany
    /// prohozené (BUY bral bids) — slippage guard tím byl zcela nefunkční:
    /// vždy vyhlásil price improvement a nikdy neskipnul.
    pub fn vwap(&self, taker_side: Side, quantity: f64) -> Option<f64> {
        let levels: Vec<PriceLevel> = match taker_side {
            // Taker BUY platí asky: seřazené vzestupně (nejlevní první).
            Side::Buy => self.asks.values().copied().collect(),
            // Taker SELL dostává od bidů: sestupně (nejvyšší první).
            Side::Sell => self.bids.values().rev().copied().collect(),
        };

        let mut remaining = quantity;
        let mut total_cost = 0.0;
        let mut total_qty = 0.0;

        for level in &levels {
            let fill_qty = remaining.min(level.quantity);
            total_cost += fill_qty * level.price;
            total_qty += fill_qty;
            remaining -= fill_qty;
            if remaining <= 0.0 {
                break;
            }
        }

        if total_qty > 0.0 {
            Some(total_cost / total_qty)
        } else {
            None
        }
    }

    /// Get total bid volume
    pub fn total_bid_volume(&self) -> f64 {
        self.bids.values().map(|l| l.quantity).sum()
    }

    /// Get total ask volume
    pub fn total_ask_volume(&self) -> f64 {
        self.asks.values().map(|l| l.quantity).sum()
    }

    /// Get top N levels for each side
    pub fn top_levels(&self, n: usize) -> (Vec<PriceLevel>, Vec<PriceLevel>) {
        let bids: Vec<PriceLevel> = self
            .bids
            .values()
            .rev()
            .take(n)
            .copied()
            .collect();
        let asks: Vec<PriceLevel> = self
            .asks
            .values()
            .take(n)
            .copied()
            .collect();
        (bids, asks)
    }

    /// Calculate order flow imbalance from current book state
    pub fn book_imbalance(&self, levels: usize) -> f64 {
        let (bids, asks) = self.top_levels(levels);
        let bid_vol: f64 = bids.iter().map(|l| l.quantity).sum();
        let ask_vol: f64 = asks.iter().map(|l| l.quantity).sum();
        let total = bid_vol + ask_vol;
        if total > 0.0 {
            (bid_vol - ask_vol) / total
        } else {
            0.0
        }
    }

    pub fn symbol(&self) -> &Symbol {
        &self.symbol
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn increment_sequence(&mut self) {
        self.sequence += 1;
    }

    /// Clear all levels (used on snapshot reset)
    pub fn clear(&mut self) {
        self.bids.clear();
        self.asks.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_order_book_basic() {
        let mut book = OrderBook::new(Symbol::new("tBTCUSD"), 0.01);

        book.update_level(Side::Buy, 60000.0, 1.5, 10);
        book.update_level(Side::Buy, 59999.0, 2.0, 5);
        book.update_level(Side::Sell, 60001.0, 1.0, 8);
        book.update_level(Side::Sell, 60002.0, 0.5, 3);

        assert_eq!(book.best_bid().unwrap().price, 60000.0);
        assert_eq!(book.best_ask().unwrap().price, 60001.0);
        assert_eq!(book.spread().unwrap(), 1.0);
        assert_eq!(book.mid_price().unwrap(), 60000.5);
    }

    #[test]
    fn test_book_imbalance() {
        let mut book = OrderBook::new(Symbol::new("tBTCUSD"), 0.01);

        book.update_level(Side::Buy, 60000.0, 3.0, 10);
        book.update_level(Side::Sell, 60001.0, 1.0, 8);

        let imbalance = book.book_imbalance(5);
        assert!(imbalance > 0.0); // More bid volume = positive imbalance
    }

    /// [CASLAV v5.1 / OPONENTURA REGRESNÍ TEST] vwap() měl dříve strany
    /// prohozené: taker BUY počítal z bidů. Tento test zajišťuje, že
    /// taker BUY vždy platí ask cenu a taker SELL dostává bid cenu.
    #[test]
    fn test_vwap_taker_semantics() {
        let mut book = OrderBook::new(Symbol::new("tBTCUSD"), 0.01);

        book.update_level(Side::Buy, 60000.0, 5.0, 10);
        book.update_level(Side::Sell, 60010.0, 5.0, 10);

        // Taker BUY 1 BTC konzumuje asky → VWAP musí být 60010 (ask), ne 60000 (bid).
        let buy_vwap = book.vwap(Side::Buy, 1.0).unwrap();
        assert!((buy_vwap - 60_010.0).abs() < 1e-9, "taker BUY VWAP = {buy_vwap}, očekávám ask");

        // Taker SELL 1 BTC konzumuje bidy → VWAP musí být 60000 (bid), ne 60010 (ask).
        let sell_vwap = book.vwap(Side::Sell, 1.0).unwrap();
        assert!((sell_vwap - 60_000.0).abs() < 1e-9, "taker SELL VWAP = {sell_vwap}, očekávám bid");
    }

    /// VWAP musí správně procházet hloubku: taker BUY 3 BTC při asku
    /// 1 BTC @ 60010 a 2 BTC @ 60020 → (1*60010 + 2*60020)/3.
    #[test]
    fn test_vwap_walks_book_depth() {
        let mut book = OrderBook::new(Symbol::new("tBTCUSD"), 0.01);

        book.update_level(Side::Sell, 60010.0, 1.0, 5);
        book.update_level(Side::Sell, 60020.0, 2.0, 5);

        let vwap = book.vwap(Side::Buy, 3.0).unwrap();
        let expected = (1.0 * 60_010.0 + 2.0 * 60_020.0) / 3.0;
        assert!((vwap - expected).abs() < 1e-9, "vwap = {vwap}, očekávám {expected}");
    }

    #[test]
    fn test_order_book_remove_level() {
        let mut book = OrderBook::new(Symbol::new("tBTCUSD"), 0.01);
        book.update_level(Side::Buy, 60000.0, 1.0, 5);
        assert!(book.best_bid().is_some());
        // Remove level via count == 0
        book.update_level(Side::Buy, 60000.0, 1.0, 0);
        assert!(book.best_bid().is_none());
    }
}
