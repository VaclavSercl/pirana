use pirana_core::types::*;
use pirana_core::errors::PiranaResult;
use std::collections::HashMap;

/// Inventory balancer — manages position inventory to minimize directional risk
#[derive(Debug)]
pub struct InventoryBalancer {
    /// Target inventory (0 = neutral)
    target_inventory: f64,
    /// Maximum allowed deviation
    max_deviation: f64,
    /// Current inventory by symbol
    inventory: HashMap<String, f64>,
}

impl InventoryBalancer {
    pub fn new(target_inventory: f64, max_deviation: f64) -> Self {
        Self {
            target_inventory,
            max_deviation,
            inventory: HashMap::new(),
        }
    }

    /// Update inventory for a symbol
    pub fn update(&mut self, symbol: &str, quantity: f64, side: Side) {
        let delta = match side {
            Side::Buy => quantity,
            Side::Sell => -quantity,
        };
        let entry = self.inventory.entry(symbol.to_string()).or_insert(0.0);
        *entry += delta;
    }

    /// Get current inventory for a symbol
    pub fn get_inventory(&self, symbol: &str) -> f64 {
        *self.inventory.get(symbol).unwrap_or(&0.0)
    }

    /// Check if inventory is within acceptable bounds
    pub fn is_balanced(&self, symbol: &str) -> bool {
        let inv = self.get_inventory(symbol);
        (inv - self.target_inventory).abs() <= self.max_deviation
    }

    /// Get the rebalancing action needed
    pub fn rebalance_action(&self, symbol: &str) -> Option<RebalanceAction> {
        let inv = self.get_inventory(symbol);
        let deviation = inv - self.target_inventory;

        if deviation.abs() <= self.max_deviation {
            return None;
        }

        if deviation > 0.0 {
            // Too long — need to sell
            Some(RebalanceAction {
                side: Side::Sell,
                quantity: deviation,
                reason: format!("Inventory {} above target {}", inv, self.target_inventory),
            })
        } else {
            // Too short — need to buy
            Some(RebalanceAction {
                side: Side::Buy,
                quantity: deviation.abs(),
                reason: format!("Inventory {} below target {}", inv, self.target_inventory),
            })
        }
    }
}

#[derive(Debug)]
pub struct RebalanceAction {
    pub side: Side,
    pub quantity: f64,
    pub reason: String,
}
