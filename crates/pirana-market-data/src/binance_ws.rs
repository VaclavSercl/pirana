use tokio_tungstenite::connect_async;
use futures::{StreamExt};
use tracing::{info, warn, error, debug};
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc::Sender;

/// Binance Public Trade Tick
#[derive(Debug, Clone)]
pub struct BinanceTradeTick {
    pub price: f64,
    pub quantity: f64,
    pub is_buyer_maker: bool,
    pub timestamp_ms: u64,
}

/// Binance Public WebSocket Client for BTC/USDT live trades
pub struct BinanceWebSocket {
    url: String,
}

impl BinanceWebSocket {
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
        }
    }

    /// Spawns a background worker loop that continuously connects and streams Binance trades.
    pub async fn run_loop(&self, sender: Sender<BinanceTradeTick>) {
        info!("Binance WebSocket worker initiated: {}", self.url);
        
        loop {
            match connect_async(&self.url).await {
                Ok((ws_stream, _)) => {
                    info!("✓ Connected to Binance BTC/USDT WebSocket feed");
                    let (_, mut read) = ws_stream.split();

                    while let Some(msg_res) = read.next().await {
                        match msg_res {
                            Ok(msg) => {
                                if msg.is_text() {
                                    if let Ok(text) = msg.to_text() {
                                        if let Ok(v) = serde_json::from_str::<Value>(text) {
                                            if let (Some(p_str), Some(q_str)) = (v.get("p").and_then(|x| x.as_str()), v.get("q").and_then(|x| x.as_str())) {
                                                if let (Ok(price), Ok(qty)) = (p_str.parse::<f64>(), q_str.parse::<f64>()) {
                                                    let is_buyer_maker = v.get("m").and_then(|x| x.as_bool()).unwrap_or(false);
                                                    let timestamp_ms = v.get("T").and_then(|x| x.as_u64()).unwrap_or_else(|| {
                                                        chrono::Utc::now().timestamp_millis() as u64
                                                    });

                                                    let tick = BinanceTradeTick {
                                                        price,
                                                        quantity: qty,
                                                        is_buyer_maker,
                                                        timestamp_ms,
                                                    };

                                                    if sender.send(tick).await.is_err() {
                                                        warn!("Binance sender channel closed, terminating loop.");
                                                        return;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                } else if msg.is_ping() {
                                    debug!("Binance WebSocket ping received");
                                } else if msg.is_close() {
                                    warn!("Binance WebSocket close frame received");
                                    break;
                                }
                            }
                            Err(e) => {
                                warn!("Binance WebSocket read error: {}", e);
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to connect to Binance WebSocket: {}", e);
                }
            }

            warn!("Reconnecting to Binance WebSocket in 3 seconds...");
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }
}
