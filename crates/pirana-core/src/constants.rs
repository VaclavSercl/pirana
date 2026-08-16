/// Hard risk limits — these are NON-NEGOTIABLE constants.
/// The AI layer CANNOT override these values.

/// HFT TRADING STRATEGY
/// Buy and sell BTC in milliseconds — profit from spread capture
/// Bitcoin is the base asset — we trade around it actively
/// No panic selling — if price drops, we buy more or hold
/// Short-term trades for maximum profit, long-term BTC appreciation

/// Maximum aggregate exposure as a fraction of total capital (90%)
pub const MAX_AGGREGATE_EXPOSURE: f64 = 0.90;

/// Maximum single trade risk as a fraction of total capital (5.0%)
pub const MAX_SINGLE_TRADE_RISK: f64 = 0.05;

/// Maximum daily drawdown before defensive mode (3%)
pub const MAX_DAILY_DRAWDOWN: f64 = 0.03;

/// Maximum weekly drawdown before full halt (7%)
pub const MAX_WEEKLY_DRAWDOWN: f64 = 0.07;

/// Number of consecutive losses before defensive protocol activates
pub const CONSECUTIVE_LOSS_THRESHOLD: u32 = 5;

/// Default trading symbol
pub const DEFAULT_SYMBOL: &str = "tBTCUSD";

/// Bitfinex WebSocket API URL
pub const BITFINEX_WS_URL: &str = "wss://api-pub.bitfinex.com/ws/2";

/// Bitfinex REST API URL
pub const BITFINEX_REST_URL: &str = "https://api.bitfinex.com";

/// Bitfinex API v2 version
pub const BITFINEX_API_VERSION: &str = "v2";

/// Order book depth levels to maintain
pub const ORDER_BOOK_DEPTH: usize = 25;

/// Maximum number of open orders per symbol
pub const MAX_OPEN_ORDERS_PER_SYMBOL: usize = 10;

/// Maximum position size in BTC
pub const MAX_POSITION_SIZE_BTC: f64 = 1.0;

/// Minimum order size on Bitfinex for BTC
pub const MIN_ORDER_SIZE_BTC: f64 = 0.00001;

/// WebSocket reconnection delay in milliseconds
pub const WS_RECONNECT_DELAY_MS: u64 = 1000;

/// Maximum WebSocket reconnection attempts before alerting
pub const WS_MAX_RECONNECT_ATTEMPTS: u32 = 10;

/// Heartbeat interval in seconds
pub const HEARTBEAT_INTERVAL_SECS: u64 = 30;

/// Feature computation window size (number of ticks)
pub const FEATURE_WINDOW_SIZE: usize = 100;

/// OFI threshold for significant imbalance detection
pub const OFI_THRESHOLD: f64 = 0.6;

/// Volatility spike threshold (standard deviations)
pub const VOLATILITY_SPIKE_THRESHOLD: f64 = 3.0;

/// Liquidity compression threshold (percentage drop)
pub const LIQUIDITY_COMPRESSION_THRESHOLD: f64 = 0.3;

/// Spread capture minimum profit in basis points
pub const SPREAD_CAPTURE_MIN_PROFIT_BPS: u32 = 2;

/// Maximum slippage tolerance in basis points
pub const MAX_SLIPPAGE_BPS: u32 = 10;

/// Signal confidence threshold for execution
pub const SIGNAL_CONFIDENCE_THRESHOLD: f64 = 0.70;

/// Prometheus metrics port
pub const METRICS_PORT: u16 = 9090;

/// Health check port
pub const HEALTH_CHECK_PORT: u16 = 8080;
