//! Future forex integration boundary.
//!
//! This crate is intentionally a skeleton. The SEC retrieval/parsing crates must not call external
//! currency APIs directly. When currency conversion is needed, wire a provider implementation into
//! application orchestration and keep original reported values distinct from converted values.

use thiserror::Error;
use time::Date;

#[derive(Debug, Clone, PartialEq)]
pub struct ExchangeRate {
    pub from_currency: String,
    pub to_currency: String,
    pub rate: f64,
    pub date: Date,
    pub provider_name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConvertedAmount {
    pub original_amount: f64,
    pub original_currency: String,
    pub converted_amount: f64,
    pub converted_currency: String,
    pub exchange_rate: ExchangeRate,
}

#[derive(Debug, Error)]
pub enum ForexError {
    #[error("exchange rate was not available for {from_currency}->{to_currency} on {date}")]
    RateUnavailable { from_currency: String, to_currency: String, date: Date },
}

pub trait ExchangeRateProvider {
    fn historical_rate(
        &self,
        from_currency: &str,
        to_currency: &str,
        date: Date,
    ) -> Result<ExchangeRate, ForexError>;
}

pub fn convert_amount(
    provider: &dyn ExchangeRateProvider,
    amount: f64,
    from_currency: &str,
    to_currency: &str,
    date: Date,
) -> Result<ConvertedAmount, ForexError> {
    let exchange_rate = provider.historical_rate(from_currency, to_currency, date)?;

    Ok(ConvertedAmount {
        original_amount: amount,
        original_currency: from_currency.to_string(),
        converted_amount: amount * exchange_rate.rate,
        converted_currency: to_currency.to_string(),
        exchange_rate,
    })
}
