//! SEC access layer.
//!
//! This crate centralizes request policy, endpoint construction, and request metadata. The goal is
//! to keep SEC behavior readable and consistent before extraction code starts depending on it.

use app_core::{AppConfig, AppError, config::SecConfig};
use filing_models::{
    Cik, CompanyId, DocumentFormat, DownloadedFilingAsset, FilingAsset, FilingAssetKind,
    FilingAssetManifest, FilingMetadata, FilingSourceMethod, RetrievalPriority, SourceType,
};
use reqwest::header::{ACCEPT, HeaderMap, HeaderValue, USER_AGENT};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use thiserror::Error;
use tokio::time::sleep;

const SEC_JSON_ACCEPT: &str = "application/json, text/plain;q=0.9, */*;q=0.8";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EndpointClass {
    CompanyTickers,
    Submissions,
    FilingIndex,
    FilingDocument,
    XbrlFacts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_backoff_millis: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self { max_attempts: 3, initial_backoff_millis: 500 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimitPolicy {
    pub max_requests_per_second: u32,
    pub minimum_interval_millis: u64,
}

impl RateLimitPolicy {
    pub fn new(max_requests_per_second: u32) -> Result<Self, SecClientError> {
        if max_requests_per_second == 0 {
            return Err(SecClientError::InvalidPolicy(
                "max_requests_per_second must be greater than zero".to_string(),
            ));
        }

        Ok(Self {
            max_requests_per_second,
            minimum_interval_millis: 1_000 / u64::from(max_requests_per_second),
        })
    }

    pub fn minimum_interval(&self) -> Duration {
        Duration::from_millis(self.minimum_interval_millis.max(1))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecRequestPolicy {
    pub user_agent: String,
    pub timeout_seconds: u64,
    pub retry_policy: RetryPolicy,
    pub rate_limit_policy: RateLimitPolicy,
}

impl SecRequestPolicy {
    pub fn from_sec_config(config: &SecConfig) -> Result<Self, SecClientError> {
        if config.user_agent.trim().is_empty() {
            return Err(SecClientError::InvalidPolicy(
                "SEC user agent cannot be empty".to_string(),
            ));
        }

        Ok(Self {
            user_agent: config.user_agent.clone(),
            timeout_seconds: config.request_timeout_seconds,
            retry_policy: RetryPolicy {
                max_attempts: config.max_retry_attempts,
                ..RetryPolicy::default()
            },
            rate_limit_policy: RateLimitPolicy::new(config.max_requests_per_second)?,
        })
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_seconds)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecEndpointCatalog {
    pub submissions_base_url: String,
    pub data_base_url: String,
    pub archives_base_url: String,
    pub company_tickers_url: String,
}

impl SecEndpointCatalog {
    pub fn from_sec_config(config: &SecConfig) -> Self {
        Self {
            submissions_base_url: config.submissions_base_url.clone(),
            data_base_url: config.data_base_url.clone(),
            archives_base_url: config.archives_base_url.clone(),
            company_tickers_url: config.company_tickers_url.clone(),
        }
    }

    pub fn company_tickers_url(&self) -> String {
        self.company_tickers_url.clone()
    }

    pub fn submissions_url(&self, cik: &Cik) -> String {
        format!("{}/CIK{}.json", self.submissions_base_url, cik.as_str())
    }

    pub fn submissions_file_url(&self, file_name: &str) -> String {
        format!("{}/{}", self.submissions_base_url, file_name.trim_start_matches('/'))
    }

    pub fn company_facts_url(&self, cik: &Cik) -> String {
        format!("{}/api/xbrl/companyfacts/CIK{}.json", self.data_base_url, cik.as_str())
    }

    pub fn filing_index_url(&self, accession_number: &str) -> String {
        format!("{}/{}", self.archives_base_url, accession_number)
    }

    pub fn filing_directory_url(&self, cik: &Cik, accession_number: &str) -> String {
        let cik_without_padding = cik.as_str().trim_start_matches('0');
        let accession_without_dashes = accession_number.replace('-', "");

        format!(
            "{}/edgar/data/{}/{}/",
            self.archives_base_url, cik_without_padding, accession_without_dashes
        )
    }

    pub fn filing_primary_document_url(
        &self,
        cik: &Cik,
        accession_number: &str,
        primary_document: &str,
    ) -> String {
        format!("{}{}", self.filing_directory_url(cik, accession_number), primary_document)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecRequest {
    pub endpoint_class: EndpointClass,
    pub source_method: FilingSourceMethod,
    pub url: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TickerLookupRecord {
    #[serde(deserialize_with = "deserialize_cik_str")]
    pub cik_str: String,
    pub ticker: String,
    pub title: String,
}

impl TickerLookupRecord {
    pub fn cik(&self) -> Cik {
        Cik::new(self.cik_str.clone())
    }
}

pub type CompanyTickersResponse = BTreeMap<String, TickerLookupRecord>;

pub fn ticker_lookup_records_from_response(
    response: CompanyTickersResponse,
) -> Vec<TickerLookupRecord> {
    response.into_values().collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentFilingLists {
    #[serde(rename = "accessionNumber", default)]
    pub accession_number: Vec<String>,
    #[serde(rename = "filingDate", default)]
    pub filing_date: Vec<String>,
    #[serde(rename = "reportDate", default)]
    pub report_date: Vec<String>,
    #[serde(default)]
    pub form: Vec<String>,
    #[serde(rename = "primaryDocument", default)]
    pub primary_document: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmissionsResponse {
    #[serde(deserialize_with = "deserialize_cik_str")]
    pub cik: String,
    pub name: String,
    #[serde(default)]
    pub tickers: Vec<String>,
    #[serde(default)]
    pub exchanges: Vec<String>,
    #[serde(rename = "fiscalYearEnd", default)]
    pub fiscal_year_end: Option<String>,
    #[serde(default)]
    pub forms: Vec<String>,
    pub filings: SubmissionsFilings,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmissionsFilings {
    pub recent: RecentFilingLists,
    #[serde(default)]
    pub files: Vec<SubmissionsFileReference>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmissionsFileReference {
    pub name: String,
    #[serde(rename = "filingCount", default)]
    pub filing_count: Option<u32>,
    #[serde(rename = "filingFrom", default)]
    pub filing_from: Option<String>,
    #[serde(rename = "filingTo", default)]
    pub filing_to: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilingDirectoryEntry {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Error)]
pub enum SecClientError {
    #[error("invalid SEC policy: {0}")]
    InvalidPolicy(String),
    #[error("invalid request header: {0}")]
    InvalidHeader(String),
    #[error("reqwest client build failed: {0}")]
    ClientBuild(String),
    #[error("request execution failed: {0}")]
    RequestFailed(String),
    #[error("response decoding failed for {url}: {reason}")]
    Decode { url: String, reason: String },
    #[error("SEC returned an unsuccessful status for {url}: {status}")]
    HttpStatus { url: String, status: u16 },
    #[error("app config could not produce an SEC client: {0}")]
    Config(String),
}

impl From<AppError> for SecClientError {
    fn from(value: AppError) -> Self {
        Self::Config(value.to_string())
    }
}

#[derive(Debug, Clone)]
pub struct SecClient {
    policy: SecRequestPolicy,
    endpoints: SecEndpointCatalog,
    http_client: reqwest::Client,
    last_request_at: Arc<Mutex<Option<Instant>>>,
}

impl SecClient {
    pub fn from_app_config(config: &AppConfig) -> Result<Self, SecClientError> {
        let policy = SecRequestPolicy::from_sec_config(&config.sec)?;
        let endpoints = SecEndpointCatalog::from_sec_config(&config.sec);
        let headers = build_default_headers(&policy)?;

        let http_client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(policy.timeout())
            .build()
            .map_err(|error| SecClientError::ClientBuild(error.to_string()))?;

        Ok(Self { policy, endpoints, http_client, last_request_at: Arc::new(Mutex::new(None)) })
    }

    pub fn policy(&self) -> &SecRequestPolicy {
        &self.policy
    }

    pub fn endpoints(&self) -> &SecEndpointCatalog {
        &self.endpoints
    }

    pub fn http_client(&self) -> &reqwest::Client {
        &self.http_client
    }

    pub fn describe_lookup_target(&self, company_id: &CompanyId) -> String {
        company_id.to_string()
    }

    pub fn company_tickers_request(&self) -> SecRequest {
        SecRequest {
            endpoint_class: EndpointClass::CompanyTickers,
            source_method: FilingSourceMethod::ApiSubmission,
            url: self.endpoints.company_tickers_url(),
            description: "ticker-to-CIK lookup".to_string(),
        }
    }

    pub fn submissions_request(&self, cik: &Cik) -> SecRequest {
        SecRequest {
            endpoint_class: EndpointClass::Submissions,
            source_method: FilingSourceMethod::ApiSubmission,
            url: self.endpoints.submissions_url(cik),
            description: format!("company submissions for CIK {}", cik.as_str()),
        }
    }

    pub fn submissions_file_request(&self, file_name: &str) -> SecRequest {
        SecRequest {
            endpoint_class: EndpointClass::Submissions,
            source_method: FilingSourceMethod::ApiSubmission,
            url: self.endpoints.submissions_file_url(file_name),
            description: format!("historical submissions file {file_name}"),
        }
    }

    pub fn company_facts_request(&self, cik: &Cik) -> SecRequest {
        SecRequest {
            endpoint_class: EndpointClass::XbrlFacts,
            source_method: FilingSourceMethod::ApiXbrlFacts,
            url: self.endpoints.company_facts_url(cik),
            description: format!("company facts for CIK {}", cik.as_str()),
        }
    }

    pub fn build_filing_asset_manifest(&self, filing: &FilingMetadata) -> FilingAssetManifest {
        let mut filing_assets = Vec::new();

        if let Some(index_url) = &filing.filing_urls.html_index {
            filing_assets.push(FilingAsset {
                accession_number: filing.accession_number.clone(),
                kind: FilingAssetKind::FilingIndex,
                source_type: SourceType::Html,
                source_method: FilingSourceMethod::FilingHtml,
                format: DocumentFormat::Html,
                priority: RetrievalPriority::Required,
                description: "filing directory index".to_string(),
                url: index_url.clone(),
                file_name: None,
            });
        }

        if let Some(primary_document) = &filing.filing_urls.primary_document {
            filing_assets.push(FilingAsset {
                accession_number: filing.accession_number.clone(),
                kind: FilingAssetKind::PrimaryDocument,
                source_type: SourceType::Html,
                source_method: FilingSourceMethod::FilingHtml,
                format: classify_document_format(primary_document),
                priority: RetrievalPriority::Required,
                description: "primary filing document".to_string(),
                url: primary_document.clone(),
                file_name: file_name_from_url(primary_document),
            });
        }

        if let Some(xbrl_instance) = &filing.filing_urls.xbrl_instance {
            filing_assets.push(FilingAsset {
                accession_number: filing.accession_number.clone(),
                kind: FilingAssetKind::XbrlInstance,
                source_type: SourceType::Xbrl,
                source_method: FilingSourceMethod::FilingHtml,
                format: classify_document_format(xbrl_instance),
                priority: RetrievalPriority::Preferred,
                description: "xbrl instance document".to_string(),
                url: xbrl_instance.clone(),
                file_name: file_name_from_url(xbrl_instance),
            });
        }

        FilingAssetManifest {
            accession_number: filing.accession_number.clone(),
            filing_date: filing.filing_date,
            filing_assets,
        }
    }

    pub async fn get_json<T>(&self, request: &SecRequest) -> Result<T, SecClientError>
    where
        T: DeserializeOwned,
    {
        let response = self.send_with_retry(&request.url).await?;
        let final_url = response.url().to_string();

        let body = response.bytes().await.map_err(|error| SecClientError::Decode {
            url: final_url.clone(),
            reason: error.to_string(),
        })?;

        serde_json::from_slice::<T>(&body)
            .map_err(|error| SecClientError::Decode { url: final_url, reason: error.to_string() })
    }

    pub async fn get_text(&self, request: &SecRequest) -> Result<String, SecClientError> {
        let response = self.send_with_retry(&request.url).await?;
        let final_url = response.url().to_string();

        response
            .text()
            .await
            .map_err(|error| SecClientError::Decode { url: final_url, reason: error.to_string() })
    }

    pub fn extend_manifest_with_directory_entries(
        &self,
        manifest: &mut FilingAssetManifest,
        entries: &[FilingDirectoryEntry],
    ) {
        for entry in entries {
            let planned_asset = classify_directory_entry(&manifest.accession_number, entry);

            if manifest.filing_assets.iter().any(|existing| existing.url == planned_asset.url) {
                continue;
            }

            manifest.filing_assets.push(planned_asset);
        }
    }

    pub async fn download_asset(
        &self,
        asset: &FilingAsset,
    ) -> Result<DownloadedFilingAsset, SecClientError> {
        let response = self.send_with_retry(&asset.url).await?;

        let status = response.status();

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_string());

        let body = response
            .bytes()
            .await
            .map_err(|error| SecClientError::RequestFailed(error.to_string()))?
            .to_vec();

        Ok(DownloadedFilingAsset {
            asset: asset.clone(),
            status_code: status.as_u16(),
            content_type,
            content_length: body.len(),
            body,
        })
    }

    async fn send_with_retry(&self, url: &str) -> Result<reqwest::Response, SecClientError> {
        let max_attempts = self.policy.retry_policy.max_attempts.max(1);
        let mut last_error = None;

        for attempt in 1..=max_attempts {
            self.wait_for_rate_limit_slot().await;

            match self.http_client.get(url).send().await {
                Ok(response) => {
                    let status = response.status();
                    let final_url = response.url().to_string();

                    if status.is_success() {
                        return Ok(response);
                    }

                    if should_retry_status(status.as_u16()) && attempt < max_attempts {
                        sleep(self.retry_backoff(attempt)).await;
                        continue;
                    }

                    return Err(SecClientError::HttpStatus {
                        url: final_url,
                        status: status.as_u16(),
                    });
                }
                Err(error) => {
                    last_error = Some(error.to_string());

                    if attempt < max_attempts {
                        sleep(self.retry_backoff(attempt)).await;
                        continue;
                    }
                }
            }
        }

        Err(SecClientError::RequestFailed(
            last_error.unwrap_or_else(|| "request failed without an error message".to_string()),
        ))
    }

    async fn wait_for_rate_limit_slot(&self) {
        let delay = {
            let now = Instant::now();
            let minimum_interval = self.policy.rate_limit_policy.minimum_interval();
            let mut last_request_at =
                self.last_request_at.lock().expect("rate-limit mutex should not be poisoned");

            match *last_request_at {
                Some(previous_slot) => {
                    let next_slot = previous_slot + minimum_interval;

                    if now < next_slot {
                        *last_request_at = Some(next_slot);
                        next_slot.duration_since(now)
                    } else {
                        *last_request_at = Some(now);
                        Duration::ZERO
                    }
                }
                None => {
                    *last_request_at = Some(now);
                    Duration::ZERO
                }
            }
        };

        if !delay.is_zero() {
            sleep(delay).await;
        }
    }

    fn retry_backoff(&self, attempt: u32) -> Duration {
        let multiplier = 2_u64.saturating_pow(attempt.saturating_sub(1));
        Duration::from_millis(
            self.policy.retry_policy.initial_backoff_millis.saturating_mul(multiplier),
        )
    }
}

fn should_retry_status(status: u16) -> bool {
    status == 429 || (500..=599).contains(&status)
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum CikStringOrNumber {
    String(String),
    Number(u64),
}

fn deserialize_cik_str<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = CikStringOrNumber::deserialize(deserializer)?;

    Ok(match value {
        CikStringOrNumber::String(value) => value,
        CikStringOrNumber::Number(value) => value.to_string(),
    })
}

fn build_default_headers(policy: &SecRequestPolicy) -> Result<HeaderMap, SecClientError> {
    let mut headers = HeaderMap::new();

    let user_agent = HeaderValue::from_str(&policy.user_agent)
        .map_err(|error| SecClientError::InvalidHeader(error.to_string()))?;
    let accept = HeaderValue::from_str(SEC_JSON_ACCEPT)
        .map_err(|error| SecClientError::InvalidHeader(error.to_string()))?;

    headers.insert(USER_AGENT, user_agent);
    headers.insert(ACCEPT, accept);

    Ok(headers)
}

fn classify_directory_entry(accession_number: &str, entry: &FilingDirectoryEntry) -> FilingAsset {
    let lower_name = entry.name.to_ascii_lowercase();
    let (kind, source_type, source_method, priority) = if lower_name.ends_with(".xml") {
        if lower_name.contains("_pre") {
            (
                FilingAssetKind::XbrlPresentation,
                SourceType::Xbrl,
                FilingSourceMethod::FilingHtml,
                RetrievalPriority::Preferred,
            )
        } else if lower_name.contains("_cal") {
            (
                FilingAssetKind::XbrlCalculation,
                SourceType::Xbrl,
                FilingSourceMethod::FilingHtml,
                RetrievalPriority::Preferred,
            )
        } else if lower_name.contains("_lab") {
            (
                FilingAssetKind::XbrlLabel,
                SourceType::Xbrl,
                FilingSourceMethod::FilingHtml,
                RetrievalPriority::Preferred,
            )
        } else if lower_name.contains(".xsd") {
            (
                FilingAssetKind::XbrlSchema,
                SourceType::Xbrl,
                FilingSourceMethod::FilingHtml,
                RetrievalPriority::Optional,
            )
        } else {
            (
                FilingAssetKind::XbrlInstance,
                SourceType::Xbrl,
                FilingSourceMethod::FilingHtml,
                RetrievalPriority::Preferred,
            )
        }
    } else if lower_name.ends_with(".xsd") {
        (
            FilingAssetKind::XbrlSchema,
            SourceType::Xbrl,
            FilingSourceMethod::FilingHtml,
            RetrievalPriority::Optional,
        )
    } else if lower_name.ends_with(".txt") {
        (
            FilingAssetKind::FilingText,
            SourceType::Text,
            FilingSourceMethod::FilingText,
            RetrievalPriority::Preferred,
        )
    } else if lower_name.ends_with(".htm") || lower_name.ends_with(".html") {
        (
            FilingAssetKind::Exhibit,
            SourceType::Html,
            FilingSourceMethod::FilingHtml,
            RetrievalPriority::Optional,
        )
    } else {
        (
            FilingAssetKind::Unknown,
            SourceType::Html,
            FilingSourceMethod::FilingHtml,
            RetrievalPriority::Optional,
        )
    };

    FilingAsset {
        accession_number: accession_number.to_string(),
        kind,
        source_type,
        source_method,
        format: classify_document_format(&entry.url),
        priority,
        description: format!("directory-discovered asset {}", entry.name),
        url: entry.url.clone(),
        file_name: Some(entry.name.clone()),
    }
}

fn classify_document_format(url: &str) -> DocumentFormat {
    let lower = url.to_ascii_lowercase();

    if lower.ends_with(".htm") || lower.ends_with(".html") {
        DocumentFormat::Html
    } else if lower.ends_with(".txt") {
        DocumentFormat::Text
    } else if lower.ends_with(".xml") {
        DocumentFormat::Xml
    } else if lower.ends_with(".json") {
        DocumentFormat::Json
    } else if lower.ends_with(".xsd") {
        DocumentFormat::Xml
    } else {
        DocumentFormat::Unknown
    }
}

fn file_name_from_url(url: &str) -> Option<String> {
    url.rsplit('/').next().map(|segment| segment.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use filing_models::{FilingForm, FilingUrls};
    use time::macros::date;

    #[test]
    fn rate_limit_policy_rejects_zero_requests_per_second() {
        let error = RateLimitPolicy::new(0).unwrap_err();
        assert!(matches!(error, SecClientError::InvalidPolicy(_)));
    }

    #[test]
    fn submissions_url_uses_zero_padded_cik() {
        let endpoints = SecEndpointCatalog {
            submissions_base_url: "https://data.sec.gov/submissions".to_string(),
            data_base_url: "https://data.sec.gov".to_string(),
            archives_base_url: "https://www.sec.gov/Archives".to_string(),
            company_tickers_url: "https://www.sec.gov/files/company_tickers.json".to_string(),
        };

        let cik = Cik::new("798354");
        assert_eq!(
            endpoints.submissions_url(&cik),
            "https://data.sec.gov/submissions/CIK0000798354.json"
        );
    }

    #[test]
    fn submissions_file_url_uses_submissions_base_path() {
        let endpoints = SecEndpointCatalog {
            submissions_base_url: "https://data.sec.gov/submissions".to_string(),
            data_base_url: "https://data.sec.gov".to_string(),
            archives_base_url: "https://www.sec.gov/Archives".to_string(),
            company_tickers_url: "https://www.sec.gov/files/company_tickers.json".to_string(),
        };

        assert_eq!(
            endpoints.submissions_file_url("CIK0000798354-submissions-001.json"),
            "https://data.sec.gov/submissions/CIK0000798354-submissions-001.json"
        );
    }

    #[test]
    fn filing_document_url_uses_sec_archive_path_shape() {
        let endpoints = SecEndpointCatalog {
            submissions_base_url: "https://data.sec.gov/submissions".to_string(),
            data_base_url: "https://data.sec.gov".to_string(),
            archives_base_url: "https://www.sec.gov/Archives".to_string(),
            company_tickers_url: "https://www.sec.gov/files/company_tickers.json".to_string(),
        };

        let cik = Cik::new("798354");
        let url =
            endpoints.filing_primary_document_url(&cik, "0000798354-25-000010", "form10k.htm");

        assert_eq!(
            url,
            "https://www.sec.gov/Archives/edgar/data/798354/000079835425000010/form10k.htm"
        );
    }

    #[test]
    fn client_uses_workspace_sec_defaults() {
        let config = AppConfig::default();
        let client = SecClient::from_app_config(&config).expect("default config should be valid");

        assert_eq!(client.policy().rate_limit_policy.max_requests_per_second, 10);
        assert_eq!(client.policy().retry_policy.max_attempts, 3);
    }

    #[test]
    fn parses_sec_company_tickers_keyed_response() {
        let payload = r#"{
            "0": {"cik_str": 798354, "ticker": "TEST", "title": "Example Corp"},
            "1": {"cik_str": "320193", "ticker": "AAPL", "title": "Apple Inc."}
        }"#;

        let response: CompanyTickersResponse =
            serde_json::from_str(payload).expect("ticker response should parse");
        let records = ticker_lookup_records_from_response(response);

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].cik().as_str(), "0000798354");
        assert_eq!(records[1].cik().as_str(), "0000320193");
    }

    #[test]
    fn parses_sec_submissions_camel_case_recent_arrays() {
        let payload = r#"{
            "cik": "0000798354",
            "name": "Example Corp",
            "tickers": ["TEST"],
            "exchanges": ["NYSE"],
            "fiscalYearEnd": "1231",
            "filings": {
                "recent": {
                    "accessionNumber": ["0000798354-25-000010"],
                    "filingDate": ["2025-02-01"],
                    "reportDate": ["2024-12-31"],
                    "form": ["10-K"],
                    "primaryDocument": ["form10k.htm"]
                },
                "files": [
                    {
                        "name": "CIK0000798354-submissions-001.json",
                        "filingCount": 100,
                        "filingFrom": "2010-01-01",
                        "filingTo": "2020-01-01"
                    }
                ]
            }
        }"#;

        let response: SubmissionsResponse =
            serde_json::from_str(payload).expect("submissions response should parse");

        assert_eq!(response.cik, "0000798354");
        assert_eq!(response.forms.len(), 0);
        assert_eq!(response.exchanges, vec!["NYSE".to_string()]);
        assert_eq!(response.fiscal_year_end.as_deref(), Some("1231"));
        assert_eq!(response.filings.recent.accession_number[0], "0000798354-25-000010");
        assert_eq!(response.filings.files.len(), 1);
    }

    #[test]
    fn filing_asset_manifest_includes_required_seed_assets() {
        let config = AppConfig::default();
        let client = SecClient::from_app_config(&config).expect("default config should be valid");
        let manifest = client.build_filing_asset_manifest(&FilingMetadata {
            accession_number: "0000798354-25-000010".to_string(),
            form_type: FilingForm::Form10K,
            filing_date: date!(2025 - 02 - 01),
            report_period_end: Some(date!(2024 - 12 - 31)),
            fiscal_period: None,
            filing_urls: FilingUrls {
                filing_detail: Some("https://www.sec.gov/Archives/edgar/data/798354/000079835425000010/".to_string()),
                primary_document: Some("https://www.sec.gov/Archives/edgar/data/798354/000079835425000010/form10k.htm".to_string()),
                xbrl_instance: Some("https://www.sec.gov/Archives/edgar/data/798354/000079835425000010/example_htm.xml".to_string()),
                html_index: Some("https://www.sec.gov/Archives/edgar/data/798354/000079835425000010/".to_string()),
            },
            source_types: vec![SourceType::Html, SourceType::Xbrl],
            is_amendment: false,
        });

        assert_eq!(manifest.filing_assets.len(), 3);
        assert!(
            manifest
                .filing_assets
                .iter()
                .any(|asset| asset.kind == FilingAssetKind::PrimaryDocument)
        );
        assert!(
            manifest.filing_assets.iter().any(|asset| asset.kind == FilingAssetKind::XbrlInstance)
        );
    }

    #[test]
    fn directory_entries_are_classified_into_retrieval_assets() {
        let config = AppConfig::default();
        let client = SecClient::from_app_config(&config).expect("default config should be valid");
        let mut manifest = FilingAssetManifest {
            accession_number: "0000798354-25-000010".to_string(),
            filing_date: date!(2025 - 02 - 01),
            filing_assets: Vec::new(),
        };

        client.extend_manifest_with_directory_entries(
            &mut manifest,
            &[
                FilingDirectoryEntry {
                    name: "example_htm.xml".to_string(),
                    url: "https://www.sec.gov/Archives/edgar/data/798354/000079835425000010/example_htm.xml".to_string(),
                },
                FilingDirectoryEntry {
                    name: "example_pre.xml".to_string(),
                    url: "https://www.sec.gov/Archives/edgar/data/798354/000079835425000010/example_pre.xml".to_string(),
                },
                FilingDirectoryEntry {
                    name: "example.txt".to_string(),
                    url: "https://www.sec.gov/Archives/edgar/data/798354/000079835425000010/example.txt".to_string(),
                },
            ],
        );

        assert_eq!(manifest.filing_assets.len(), 3);
        assert_eq!(manifest.filing_assets[0].kind, FilingAssetKind::XbrlInstance);
        assert_eq!(manifest.filing_assets[1].kind, FilingAssetKind::XbrlPresentation);
        assert_eq!(manifest.filing_assets[2].kind, FilingAssetKind::FilingText);
    }
}
