use crate::error::AppError;
use tracing_subscriber::{EnvFilter, fmt};

pub fn init_tracing() -> Result<(), AppError> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init()
        .map_err(|error| AppError::Telemetry(error.to_string()))
}
