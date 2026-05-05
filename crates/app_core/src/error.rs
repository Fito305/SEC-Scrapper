use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("telemetry bootstrap failed: {0}")]
    Telemetry(String),
    #[error("invalid SEC configuration: {0}")]
    InvalidSecConfig(String),
    #[error("SEC retrieval bootstrap failed: {0}")]
    SecRetrieval(String),
}
