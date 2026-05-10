use pirana_core::types::*;
use pirana_core::errors::PiranaResult;
use std::collections::HashMap;

/// Tracks exposure across all positions and orders
#[derive(Debug)]
pub struct ExposureTracker {
    /// Positions by symbol
    positions: HashMap<String, Position>,
    /// Pending orders by symbol
    pending_orders: HashMap<String, Vec<PendingOrder>>,
    /// Total exposure in BTC terms
    total_exposure_btc: f64,
}

#[derive(Debug, Clone)]
pub struct PendingOrder {
    pub order_id: OrderId,
    pub symbol: Symbol,
    pub side: Side,
    pub quantity: f64,
    pub price: f64,
}

impl ExposureTracker {
    pub fn new() -> Self {
        Self {
            positions: HashMap::new(),
            pending_orders: HashMap::new(),
            total_exposure_btc: 0.0,
        }
    }

    /// Add a position
    pub fn add_position(&mut self, position: Position) {
        let symbol = position.symbol.as_str().to_string();
        self.total_exposure_btc += position.quantity;
        self.positions.insert(symbol, position);
        self.recompute_exposure();
    }

    /// Update a position
    pub fn update_position(&mut self, symbol: &str, quantity: f64, price: f64) {
        if let Some(pos) = self.positions.get_mut(symbol) {
            pos.quantity = quantity;
            pos.entry_price = price;
        }
    }

    /// Remove a position
    pub fn remove_position(&mut self, symbol: &str) -> Option<Position> {
        let pos = self.positions.remove(symbol);
        self.recompute_exposure();
        pos
    }

    /// Add a pending order
    pub fn add_pending_order(&mut self, order: PendingOrder) {
        let symbol = order.symbol.as_str().to_string();
        self.pending_orders.entry(symbol).or_default().push(order);
        self.recompute_exposure();
    }

    /// Remove a pending order
    pub fn remove_pending_order(&mut self, symbol: &str, order_id: OrderId) {
        if let Some(orders) = self.pending_orders.get_mut(symbol) {
            orders.retain(|o| o.order_id != order_id);
        }
        self.recompute_exposure();
    }

    /// Get total exposure as a fraction
    pub fn total_exposure(&self) -> f64 {
        self.total_exposure_btc
    }

    /// Get position for a symbol
    pub fn get_position(&self, symbol: &str) -> Option<&Position> {
        self.positions.get(symbol)
    }

    fn recompute_exposure(&mut self) {
        let position_exposure: f64 = self.positions.values().map(|p| p.quantity).sum();
        let order_exposure: f64 = self.pending_orders.values()
            .flat_map(|orders| orders.iter())
            .map(|o| o.quantity)
            .sum();
        self.total_exposure_btc = position_exposure + order_exposure;
    }
}
