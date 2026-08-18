use tokio_tungstenite::connect_async;
use futures::{SinkExt, StreamExt};
use tracing::{info, warn, error, debug};
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc::Sender;

/// Coinbase Public Market Tick
#[derive(Debug, Clone)]
pub struct CoinbaseTradeTick {
    pub price: f64,
    pub quantity: f64,
    pub side: String,
    pub timestamp_ms: u64,
}

/// Coinbase Public WebSocket Client for BTC-USD live feed
pub struct CoinbaseWebSocket {
    url: String,
}

impl CoinbaseWebSocket {
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
        }
    }

    /// Spawns a background worker loop that continuously connects and streams Coinbase ticks.
    pub async fn run_loop(&self, sender: Sender<CoinbaseTradeTick>) {
        info!("Coinbase WebSocket worker initiated: {}", self.url);

        loop {
            match connect_async(&self.url).await {
                Ok((mut ws_stream, _)) => {
                    info!("✓ Connected to Coinbase WebSocket feed");

                    // Subscribe to ticker and matches for BTC-USD
                    let subscribe_msg = serde_json::json!({
                        "type": "subscribe",
                        "product_ids": ["BTC-USD"],
                        "channels": ["ticker", "matches"]
                    });

                    if let Err(e) = ws_stream.send(tokio_tungstenite::tungstenite::Message::Text(subscribe_msg.to_string())).await {
                        error!("Failed to send subscribe message to Coinbase: {}", e);
                        tokio::time::sleep(Duration::from_secs(3)).await;
                        continue;
                    }

                    info!("Subscribed to Coinbase BTC-USD ticker & matches");
                    let (_, mut read) = ws_stream.split();

                    while let Some(msg_res) = read.next().await {
                        match msg_res {
                            Ok(msg) => {
                                if msg.is_text() {
                                    if let Ok(text) = msg.to_text() {
                                        if let Ok(v) = serde_json::from_str::<Value>(text) {
                                            let msg_type = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
                                            if msg_type == "ticker" || msg_type == "match" || msg_type == "last_match" {
                                                if let Some(p_str) = v.get("price").and_then(|x| x.as_str()) {
                                                    if let Ok(price) = p_str.parse::<f64>() {
                                                        let qty = v.get("size")
                                                            .and_then(|x| x.as_str())
                                                            .and_then(|s| s.parse::<f64>().ok())
                                                            .unwrap_or(0.0);
                                                        let side = v.get("side")
                                                            .and_then(|x| x.as_str())
                                                            .unwrap_or("buy")
                                                            .to_string();
                                                        let timestamp_ms = chrono::Utc::now().timestamp_millis() as u64;

                                                        let tick = CoinbaseTradeTick {
                                                            price,
                                                            quantity: qty,
                                                            side,
                                                            timestamp_ms,
                                                        };

                                                        if sender.send(tick).await.is_err() {
                                                            warn!("Coinbase sender channel closed, terminating loop.");
                                                            return;
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                } else if msg.is_ping() {
                                    debug!("Coinbase WebSocket ping received");
                                } else if msg.is_close() {
                                    warn!("Coinbase WebSocket close frame received");
                                    break;
                                }
                            }
                            Err(e) => {
                                warn!("Coinbase WebSocket read error: {}", e);
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Failed to connect to Coinbase WebSocket: {}", e);
                }
            }

            warn!("Reconnecting to Coinbase WebSocket in 3 seconds...");
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
    }
}
