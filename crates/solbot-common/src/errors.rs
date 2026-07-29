use thiserror::Error;

#[derive(Error, Debug)]
pub enum SolbotError {
    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[error("Signer IPC error: {0}")]
    SignerIpcError(String),

    #[error("Database error: {0}")]
    DatabaseError(String),

    #[error("Network error: {0}")]
    NetworkError(String),
    
    #[error("Strategy execution error: {0}")]
    StrategyError(String),
}
