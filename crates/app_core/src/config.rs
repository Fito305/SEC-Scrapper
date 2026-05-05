use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub sec: SecConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecConfig {
    pub max_requests_per_second: u32,
    pub user_agent: String,
    pub request_timeout_seconds: u64,
    pub max_retry_attempts: u32,
    pub submissions_base_url: String,
    pub data_base_url: String,
    pub archives_base_url: String,
    pub company_tickers_url: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self { sec: SecConfig::default() }
    }
}

impl AppConfig {
    pub fn from_env() -> Self {
        Self { sec: SecConfig::from_env() }
    }
}

impl Default for SecConfig {
    fn default() -> Self {
        Self {
            max_requests_per_second: 10,
            user_agent:
                "sec-edgar-scraper/0.1.0 QualitativeFocus Felipe Acosta felipe.acosta002@gmail.com"
                    .into(),
            request_timeout_seconds: 30,
            max_retry_attempts: 3,
            submissions_base_url: "https://data.sec.gov/submissions".into(),
            data_base_url: "https://data.sec.gov".into(),
            archives_base_url: "https://www.sec.gov/Archives".into(),
            company_tickers_url: "https://www.sec.gov/files/company_tickers.json".into(),
        }
    }
}

impl SecConfig {
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(user_agent) = std::env::var("SEC_EDGAR_USER_AGENT") {
            if !user_agent.trim().is_empty() {
                config.user_agent = user_agent;
            }
        }

        if let Ok(value) = std::env::var("SEC_EDGAR_MAX_REQUESTS_PER_SECOND") {
            if let Ok(max_requests_per_second) = value.parse::<u32>() {
                config.max_requests_per_second = max_requests_per_second;
            }
        }

        if let Ok(value) = std::env::var("SEC_EDGAR_REQUEST_TIMEOUT_SECONDS") {
            if let Ok(request_timeout_seconds) = value.parse::<u64>() {
                config.request_timeout_seconds = request_timeout_seconds;
            }
        }

        if let Ok(value) = std::env::var("SEC_EDGAR_MAX_RETRY_ATTEMPTS") {
            if let Ok(max_retry_attempts) = value.parse::<u32>() {
                config.max_retry_attempts = max_retry_attempts;
            }
        }

        config
    }
}
