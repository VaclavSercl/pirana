use crate::websocket::BitfinexWebSocket;
use crate::rest::BitfinexRestApi;
use crate::order_book_manager::OrderBookManager;
use crate::auth::BitfinexAuth;
use pirana_core::types::*;
use pirana_core::constants::*;
use pirana_core::errors::{PiranaError, PiranaResult};
use tokio::sync::broadcast;
use tracing::{info, warn, error, debug};

/// Central market data engine that coordinates WebSocket feeds,
/// REST API calls, and order book management for Bitfinex.
pub struct MarketDataEngine {
    /// WebSocket connection for real-time data
    ws: BitfinexWebSocket,
    /// REST API for snapshots and historical data
    rest: BitfinexRestApi,
    /// Order book manager
    book_manager: OrderBookManager,
    /// Authentication (read-only keys for market data)
    auth: Option<BitfinexAuth>,
    /// Channel for broadcasting ticks
    tick_tx: broadcast::Sender<Tick>,
    /// Channel for broadcasting order book updates
    book_tx: broadcast::Sender<OrderBookSnapshot>,
    /// Subscribed symbols
    symbols: Vec<Symbol>,
}

impl MarketDataEngine {
    pub fn new(symbols: Vec<Symbol>) -> Self {
        let (tick_tx, _) = broadcast::channel(10_000);
        let (book_tx, _) = broadcast::channel(10_000);

        Self {
            ws: BitfinexWebSocket::new(BITFINEX_WS_URL),
            rest: BitfinexRestApi::new(BITFINEX_REST_URL),
            book_manager: OrderBookManager::new(ORDER_BOOK_DEPTH),
            auth: None,
            tick_tx,
            book_tx,
            symbols,
        }
    }

    /// Set authentication for private channels (account updates)
    pub fn set_auth(&mut self, auth: BitfinexAuth) {
        self.auth = Some(auth);
    }

    /// Start the market data engine
    pub async fn start(&mut self) -> PiranaResult<()> {
        info!("Starting Market Data Engine for {:?}", self.symbols);

        // Connect WebSocket
        self.ws.connect().await.map_err(|e| {
            PiranaError::WebSocket(format!("Failed to connect: {}", e))
        })?;

        // Subscribe to channels
        for symbol in &self.symbols {
            // Subscribe to order book (precision P0, frequency F0)
            self.ws.subscribe_order_book(symbol.as_str()).await.map_err(|e| {
                PiranaError::WebSocket(format!("Subscribe order book failed: {}", e))
            })?;

            // Subscribe to trades
            self.ws.subscribe_trades(symbol.as_str()).await.map_err(|e| {
                PiranaError::WebSocket(format!("Subscribe trades failed: {}", e))
            })?;

            info!("Subscribed to order book and trades for {}", symbol.as_str());
        }

        // If authenticated, subscribe to private channels
        if let Some(ref auth) = self.auth {
            self.ws.authenticate(auth).await.map_err(|e| {
                PiranaError::Authentication(format!("Auth failed: {}", e))
            })?;
        }

        // Start processing loop
        self.run_event_loop().await
    }

    /// Main event loop processing WebSocket messages
    async fn run_event_loop(&mut self) -> PiranaResult<()> {
        info!("Market Data Engine event loop started");

        loop {
            match self.ws.next_message().await {
                Ok(msg) => {
                    if let Err(e) = self.process_message(msg).await {
                        error!("Error processing message: {}", e);
                    }
                }
                Err(e) => {
                    error!("WebSocket error: {}", e);
                    warn!("Attempting reconnection...");
                    tokio::time::sleep(tokio::time::Duration::from_millis(WS_RECONNECT_DELAY_MS)).await;
                    if let Err(e) = self.ws.reconnect().await {
                        error!("Reconnection failed: {}", e);
                        return Err(PiranaError::WebSocket(e.to_string()));
                    }
                }
            }
        }
    }

    /// Process a single WebSocket message
    async fn process_message(&mut self, msg: serde_json::Value) -> PiranaResult<()> {
        // Bitfinex sends either arrays (channel data) or objects (events)
        if let Some(array) = msg.as_array() {
            self.process_channel_data(array).await?;
        } else if let Some(object) = msg.as_object() {
            self.process_event(object).await?;
        }
        Ok(())
    }

    /// Process channel data (order book updates, trades)
    async fn process_channel_data(&mut self, data: &[serde_json::Value]) -> PiranaResult<()> {
        if data.len() < 2 {
            return Ok(());
        }

        let channel_id = data[0].as_i64().unwrap_or(0);
        let channel_type = self.ws.get_channel_type(channel_id);

        match channel_type.as_str() {
            "book" => {
                self.book_manager.process_update(&data[1..])?;
                if let Some(snapshot) = self.book_manager.get_snapshot(DEFAULT_SYMBOL) {
                    let _ = self.book_tx.send(snapshot);
                }
            }
            "trades" => {
                if let Some(tick) = self.parse_trade(&data[1..])? {
                    let _ = self.tick_tx.send(tick);
                }
            }
            _ => {}
        }

        Ok(())
    }

    /// Process event messages (auth, subscriptions, errors)
    async fn process_event(&mut self, event: &serde_json::Map<String, serde_json::Value>) -> PiranaResult<()> {
        let event_type = event.get("event").and_then(|v| v.as_str()).unwrap_or("");

        match event_type {
            "subscribed" => {
                info!("Successfully subscribed: {:?}", event.get("channel"));
            }
            "error" => {
                let msg = event.get("msg").and_then(|v| v.as_str()).unwrap_or("unknown");
                error!("Bitfinex error: {}", msg);
            }
            "auth" => {
                let status = event.get("status").and_then(|v| v.as_str()).unwrap_or("");
                if status == "OK" {
                    info!("Authentication successful");
                } else {
                    error!("Authentication failed: {:?}", event);
                }
            }
            "hb" => {
                // Heartbeat — no action needed
            }
            _ => {
                debug!("Unhandled event type: {}", event_type);
            }
        }

        Ok(())
    }

    /// Parse a trade from channel data
    fn parse_trade(&self, data: &[serde_json::Value]) -> PiranaResult<Option<Tick>> {
        // Bitfinex trade format: [ID, TIMESTAMP, AMOUNT, PRICE]
        // or for snapshots: [[ID, TS, AMOUNT, PRICE], ...]
        if data.is_empty() {
            return Ok(None);
        }

        if data[0].is_array() {
            // Snapshot — skip, we process individual trades
            return Ok(None);
        }

        let trade_id = data[0].as_i64().unwrap_or(0) as u64;
        let timestamp_ms = data[1].as_i64().unwrap_or(0);
        let amount = data[2].as_f64().unwrap_or(0.0);
        let price = data[3].as_f64().unwrap_or(0.0);

        let side = if amount > 0.0 { Side::Buy } else { Side::Sell };

        Ok(Some(Tick {
            symbol: Symbol::new(DEFAULT_SYMBOL),
            price,
            quantity: amount.abs(),
            side,
            timestamp: chrono::DateTime::from_timestamp_millis(timestamp_ms)
                .unwrap_or_else(|| chrono::Utc::now()),
            trade_id,
        }))
    }

    /// Get a receiver for tick updates
    pub fn subscribe_ticks(&self) -> broadcast::Receiver<Tick> {
        self.tick_tx.subscribe()
    }

    /// Get a receiver for order book snapshots
    pub fn subscribe_order_book(&self) -> broadcast::Receiver<OrderBookSnapshot> {
        self.book_tx.subscribe()
    }
}

pub mod websocket;
pub mod rest;
pub mod order_book_manager;
pub mod auth;
