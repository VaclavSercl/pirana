use pirana_core::types::*;
use pirana_core::constants::*;
use pirana_core::errors::{PiranaError, PiranaResult};
use tracing::{info, warn, error, debug};

/// Order router — manages order lifecycle
pub struct OrderRouter {
    /// Active orders by ID
    active_orders: Vec<ActiveOrder>,
    /// Order history
    order_history: Vec<OrderRecord>,
    /// Maximum open orders
    max_open_orders: usize,
}

#[derive(Debug, Clone)]
pub struct ActiveOrder {
    pub order_id: OrderId,
    pub exchange_order_id: Option<String>,
    pub symbol: Symbol,
    pub side: Side,
    pub order_type: OrderType,
    pub quantity: f64,
    pub price: Option<f64>,
    pub stop_price: Option<f64>,
    pub status: OrderStatus,
    pub filled_quantity: f64,
    pub average_fill_price: f64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone)]
pub struct OrderRecord {
    pub order_id: OrderId,
    pub symbol: Symbol,
    pub side: Side,
    pub quantity: f64,
    pub price: f64,
    pub pnl: f64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub closed_at: chrono::DateTime<chrono::Utc>,
}

impl OrderRouter {
    pub fn new() -> Self {
        Self {
            active_orders: Vec::new(),
            order_history: Vec::new(),
            max_open_orders: MAX_OPEN_ORDERS_PER_SYMBOL,
        }
    }

    /// Create a new order from a validated signal
    pub fn create_order(
        &mut self,
        signal: &Signal,
        current_price: f64,
        quantity: f64,
    ) -> PiranaResult<OrderId> {
        // Check open order limit
        let symbol_orders = self.active_orders.iter()
            .filter(|o| o.symbol.as_str() == signal.target_asset.as_str())
            .count();

        if symbol_orders >= self.max_open_orders {
            return Err(PiranaError::Execution(format!(
                "Max open orders ({}) reached for {}",
                self.max_open_orders,
                signal.target_asset.as_str()
            )));
        }

        let order_id = OrderId::new();
        let side = match signal.signal_type {
            SignalType::AccumulationEntry => Side::Buy,
            SignalType::DistributionExit => Side::Sell,
            SignalType::SpreadCapture => Side::Buy, // Will place both sides
            SignalType::MarketMaking => Side::Buy,   // Will place both sides
            SignalType::Hold | SignalType::DefensiveHalt => {
                return Err(PiranaError::Execution(
                    "Hold/DefensiveHalt signals do not create orders".to_string(),
                ));
            }
        };

        // Calculate entry price from signal zone
        let entry_price = (signal.recommended_params.entry_zone.0 + signal.recommended_params.entry_zone.1) / 2.0;

        // Calculate position size using the passed quantity directly
        let position_size = quantity;

        let order = ActiveOrder {
            order_id,
            exchange_order_id: None,
            symbol: signal.target_asset.clone(),
            side,
            order_type: OrderType::Limit,
            quantity: position_size,
            price: Some(entry_price),
            stop_price: Some(signal.invalidation_level),
            status: OrderStatus::Pending,
            filled_quantity: 0.0,
            average_fill_price: 0.0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        self.active_orders.push(order);

        info!(
            "Order created: id={}, symbol={:?}, side={:?}, qty={:.6}, price={:.2}",
            order_id.0, signal.target_asset, side, position_size, entry_price
        );

        Ok(order_id)
    }

    /// Cancel an order
    pub fn cancel_order(&mut self, order_id: OrderId) -> PiranaResult<()> {
        if let Some(order) = self.active_orders.iter_mut().find(|o| o.order_id == order_id) {
            order.status = OrderStatus::Cancelled;
            order.updated_at = chrono::Utc::now();
            info!("Order {} cancelled", order_id.0);
            Ok(())
        } else {
            Err(PiranaError::Execution(format!(
                "Order {} not found",
                order_id.0
            )))
        }
    }

    /// Update order status from exchange notification
    pub fn update_order(
        &mut self,
        order_id: OrderId,
        status: OrderStatus,
        filled_qty: f64,
        avg_price: f64,
        exchange_id: Option<String>,
    ) -> PiranaResult<()> {
        if let Some(order) = self.active_orders.iter_mut().find(|o| o.order_id == order_id) {
            order.status = status;
            order.filled_quantity = filled_qty;
            order.average_fill_price = avg_price;
            order.updated_at = chrono::Utc::now();

            if let Some(id) = exchange_id {
                order.exchange_order_id = Some(id);
            }

            // If fully filled or cancelled, move to history
            match status {
                OrderStatus::Filled | OrderStatus::Cancelled | OrderStatus::Rejected | OrderStatus::Expired => {
                    let record = OrderRecord {
                        order_id: order.order_id,
                        symbol: order.symbol.clone(),
                        side: order.side,
                        quantity: order.quantity,
                        price: order.price.unwrap_or(0.0),
                        pnl: 0.0, // Calculated by position manager
                        created_at: order.created_at,
                        closed_at: chrono::Utc::now(),
                    };
                    self.order_history.push(record);
                    self.active_orders.retain(|o| o.order_id != order_id);
                }
                _ => {}
            }

            Ok(())
        } else {
            Err(PiranaError::Execution(format!(
                "Order {} not found for update",
                order_id.0
            )))
        }
    }

    /// Get active orders for a symbol
    pub fn get_active_orders(&self, symbol: &str) -> Vec<&ActiveOrder> {
        self.active_orders.iter()
            .filter(|o| o.symbol.as_str() == symbol)
            .collect()
    }

    /// Cancel all orders for a symbol
    pub fn cancel_all_for_symbol(&mut self, symbol: &str) -> Vec<OrderId> {
        let mut cancelled = Vec::new();
        for order in self.active_orders.iter_mut() {
            if order.symbol.as_str() == symbol {
                order.status = OrderStatus::Cancelled;
                cancelled.push(order.order_id);
            }
        }
        cancelled
    }
}
