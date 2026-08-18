use pirana_core::errors::{PiranaError, PiranaResult};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use futures::{SinkExt, StreamExt};
use tracing::{info, warn};
use std::collections::HashMap;

/// WebSocket connection to Bitfinex
pub struct BitfinexWebSocket {
    url: String,
    stream: Option<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    /// Channel ID to channel type mapping
    channels: HashMap<i64, String>,
    next_channel_id: i64,
}

impl BitfinexWebSocket {
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            stream: None,
            channels: HashMap::new(),
            next_channel_id: 1,
        }
    }

    /// Connect to Bitfinex WebSocket
    pub async fn connect(&mut self) -> PiranaResult<()> {
        info!("Connecting to Bitfinex WebSocket: {}", self.url);

        let (ws_stream, response) = connect_async(&self.url).await.map_err(|e| {
            PiranaError::WebSocket(format!("Connection failed: {}", e))
        })?;

        info!("WebSocket connected: {:?}", response);
        self.stream = Some(ws_stream);
        Ok(())
    }

    /// Reconnect after connection loss
    pub async fn reconnect(&mut self) -> PiranaResult<()> {
        warn!("Reconnecting to Bitfinex WebSocket...");
        self.stream = None;
        self.channels.clear();
        self.connect().await
    }

    /// Subscribe to order book channel
    pub async fn subscribe_order_book(&mut self, symbol: &str) -> PiranaResult<i64> {
        let msg = serde_json::json!({
            "event": "subscribe",
            "channel": "book",
            "symbol": symbol,
            "prec": "P0",  // Price precision: raw
            "freq": "F0",  // Frequency: real-time
            "len": "25"    // Book length
        });

        self.send_message(&msg).await?;
        let channel_id = self.next_channel_id;
        self.next_channel_id += 1;
        self.channels.insert(channel_id, "book".to_string());

        info!("Subscribed to order book for {} (channel {})", symbol, channel_id);
        Ok(channel_id)
    }

    /// Subscribe to trades channel
    pub async fn subscribe_trades(&mut self, symbol: &str) -> PiranaResult<i64> {
        let msg = serde_json::json!({
            "event": "subscribe",
            "channel": "trades",
            "symbol": symbol
        });

        self.send_message(&msg).await?;
        let channel_id = self.next_channel_id;
        self.next_channel_id += 1;
        self.channels.insert(channel_id, "trades".to_string());

        info!("Subscribed to trades for {} (channel {})", symbol, channel_id);
        Ok(channel_id)
    }

    /// Subscribe to ticker channel
    pub async fn subscribe_ticker(&mut self, symbol: &str) -> PiranaResult<i64> {
        let msg = serde_json::json!({
            "event": "subscribe",
            "channel": "ticker",
            "symbol": symbol
        });

        self.send_message(&msg).await?;
        let channel_id = self.next_channel_id;
        self.next_channel_id += 1;
        self.channels.insert(channel_id, "ticker".to_string());

        Ok(channel_id)
    }

    /// Authenticate for private channels
    pub async fn authenticate(&mut self, auth: &super::auth::BitfinexAuth) -> PiranaResult<()> {
        let nonce = chrono::Utc::now().timestamp_millis().to_string();
        let auth_payload = format!("AUTH{}", nonce);
        let signature = auth.sign(&auth_payload);

        let msg = serde_json::json!({
            "event": "auth",
            "apiKey": auth.api_key(),
            "authSig": signature,
            "authPayload": auth_payload,
            "authNonce": nonce,
            "filter": ["trading", "wallet", "balance"]
        });

        self.send_message(&msg).await?;
        info!("Authentication request sent");
        Ok(())
    }

    /// Send a JSON message
    async fn send_message(&mut self, msg: &serde_json::Value) -> PiranaResult<()> {
        let stream = self.stream.as_mut().ok_or_else(|| {
            PiranaError::WebSocket("Not connected".to_string())
        })?;

        let text = serde_json::to_string(msg)?;
        stream.send(tokio_tungstenite::tungstenite::Message::Text(text)).await.map_err(|e| {
            PiranaError::WebSocket(format!("Send failed: {}", e))
        })?;

        Ok(())
    }

    /// Receive the next message
    pub async fn next_message(&mut self) -> PiranaResult<serde_json::Value> {
        let stream = self.stream.as_mut().ok_or_else(|| {
            PiranaError::WebSocket("Not connected".to_string())
        })?;

        match stream.next().await {
            Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                let value: serde_json::Value = serde_json::from_str(&text)?;
                Ok(value)
            }
            Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(data))) => {
                // Respond to ping with pong
                stream.send(tokio_tungstenite::tungstenite::Message::Pong(data)).await.ok();
                // Return empty object to continue loop
                Ok(serde_json::json!({}))
            }
            Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) => {
                Err(PiranaError::WebSocket("Connection closed by server".to_string()))
            }
            Some(Ok(_)) => {
                // Binary or other message types — skip
                Ok(serde_json::json!({}))
            }
            Some(Err(e)) => {
                Err(PiranaError::WebSocket(format!("Receive error: {}", e)))
            }
            None => {
                Err(PiranaError::WebSocket("Stream ended".to_string()))
            }
        }
    }

    /// Get channel type by ID
    pub fn get_channel_type(&self, channel_id: i64) -> String {
        self.channels.get(&channel_id).cloned().unwrap_or_default()
    }
}
