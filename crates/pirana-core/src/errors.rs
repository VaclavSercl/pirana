use thiserror::Error;

/// Core error types for the PIRANA system
#[derive(Error, Debug)]
pub enum PiranaError {
    #[error("Market data error: {0}")]
    MarketData(String),

    #[error("Execution error: {0}")]
    Execution(String),

    #[error("Risk limit violated: {0}")]
    RiskLimit(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Signal validation error: {0}")]
    SignalValidation(String),

    #[error("Exchange API error: {code} - {message}")]
    ExchangeApi { code: i32, message: String },

    #[error("WebSocket error: {0}")]
    WebSocket(String),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("System in invalid state: {0}")]
    InvalidState(String),

    #[error("Not enough data: {0}")]
    InsufficientData(String),

    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("Authentication error: {0}")]
    Authentication(String),

    #[error("Unknown error: {0}")]
    Unknown(String),
}

/// Result type alias for PIRANA operations
pub type PiranaResult<T> = Result<T, PiranaError>;
