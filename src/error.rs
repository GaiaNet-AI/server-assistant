use thiserror::Error;

#[derive(Error, Clone, Debug, PartialEq, Eq)]
pub enum AssistantError {
    /// Error returned while parsing socket address failed
    #[error("Failed to parse socket address: {0}")]
    SocketAddr(String),
    /// Error returned while parsing CLI options failed
    #[error("{0}")]
    ArgumentError(String),
    /// Generic error returned while performing an operation
    #[error("{0}")]
    Operation(String),
}
