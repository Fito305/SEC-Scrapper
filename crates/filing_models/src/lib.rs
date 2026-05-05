//! Shared filing and extraction models used across the workspace.
//!
//! These types are deliberately readable and explicit. They form the stable language that later
//! crates will use when SEC retrieval, extraction, normalization, valuation, and workbook export
//! are implemented.

use serde::{Deserialize, Serialize};
use std::fmt;
use time::Date;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Ticker(String);

impl Ticker {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into().trim().to_ascii_uppercase())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Ticker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Cik(String);

impl Cik {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let digits_only: String =
            value.chars().filter(|character| character.is_ascii_digit()).collect();
        Self(format!("{digits_only:0>10}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Cik {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CompanyId {
    Ticker(Ticker),
    Cik(Cik),
}

impl fmt::Display for CompanyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ticker(ticker) => write!(f, "ticker:{ticker}"),
            Self::Cik(cik) => write!(f, "cik:{cik}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompanyIdentity {
    pub primary_id: CompanyId,
    pub ticker: Option<Ticker>,
    pub cik: Option<Cik>,
    pub issuer_name: String,
    pub exchange: Option<String>,
    pub reported_currency: Option<String>,
    pub fiscal_year_end: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilingForm {
    Form10K,
    Form10Q,
    Form20F,
    Form6K,
    Form8K,
    Other(String),
}

impl FilingForm {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Form10K => "10-K",
            Self::Form10Q => "10-Q",
            Self::Form20F => "20-F",
            Self::Form6K => "6-K",
            Self::Form8K => "8-K",
            Self::Other(value) => value.as_str(),
        }
    }

    pub fn is_supported_v1(&self) -> bool {
        matches!(self, Self::Form10K | Self::Form10Q)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FiscalQuarter {
    Q1,
    Q2,
    Q3,
    Q4,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FiscalPeriod {
    pub fiscal_year: i32,
    pub fiscal_quarter: Option<FiscalQuarter>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeriodContext {
    Instant { as_of: Date },
    Duration { start: Date, end: Date },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportingPeriod {
    pub context: PeriodContext,
    pub fiscal_period: Option<FiscalPeriod>,
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilingUrls {
    pub filing_detail: Option<String>,
    pub primary_document: Option<String>,
    pub xbrl_instance: Option<String>,
    pub html_index: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceType {
    Xbrl,
    Html,
    Text,
    WorkbookImport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilingSourceMethod {
    ApiSubmission,
    ApiXbrlFacts,
    FilingHtml,
    FilingText,
    WorkbookImport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilingMetadata {
    pub accession_number: String,
    pub form_type: FilingForm,
    pub filing_date: Date,
    pub report_period_end: Option<Date>,
    pub fiscal_period: Option<FiscalPeriod>,
    pub filing_urls: FilingUrls,
    pub source_types: Vec<SourceType>,
    pub is_amendment: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RetrievalPriority {
    Required,
    Preferred,
    Optional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DocumentFormat {
    Html,
    Text,
    Xml,
    Json,
    InlineXbrl,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilingAssetKind {
    FilingIndex,
    PrimaryDocument,
    FilingText,
    XbrlInstance,
    XbrlSchema,
    XbrlPresentation,
    XbrlCalculation,
    XbrlLabel,
    Exhibit,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilingAsset {
    pub accession_number: String,
    pub kind: FilingAssetKind,
    pub source_type: SourceType,
    pub source_method: FilingSourceMethod,
    pub format: DocumentFormat,
    pub priority: RetrievalPriority,
    pub description: String,
    pub url: String,
    pub file_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilingAssetManifest {
    pub accession_number: String,
    pub filing_date: Date,
    pub filing_assets: Vec<FilingAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadedFilingAsset {
    pub asset: FilingAsset,
    pub status_code: u16,
    pub content_type: Option<String>,
    pub content_length: usize,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MeasurementUnit {
    Currency(String),
    Shares,
    Percentage,
    Ratio,
    Count,
    Text,
    Other(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ValueScale {
    Raw,
    Thousands,
    Millions,
    Billions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignConvention {
    AsReported,
    NormalizedPositive,
    NormalizedNegative,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLocator {
    pub section_name: Option<String>,
    pub table_name: Option<String>,
    pub row_label: Option<String>,
    pub cell_reference: Option<String>,
    pub segment_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub accession_number: String,
    pub filing_url: Option<String>,
    pub form_type: FilingForm,
    pub source_type: SourceType,
    pub source_method: FilingSourceMethod,
    pub source_location: SourceLocator,
    pub xbrl_tag: Option<String>,
    pub filing_label: Option<String>,
    pub reporting_period: ReportingPeriod,
    pub unit: MeasurementUnit,
    pub scale: ValueScale,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NumericValue {
    pub amount: f64,
    pub unit: MeasurementUnit,
    pub scale: ValueScale,
    pub sign_convention: SignConvention,
    pub label: Option<String>,
    pub reporting_period: ReportingPeriod,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextBlock {
    pub title: String,
    pub content: String,
    pub form_type: FilingForm,
    pub filing_date: Date,
    pub source_type: SourceType,
    pub source_location: SourceLocator,
    pub associated_domain: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MetricValue {
    Numeric(NumericValue),
    Text(TextBlock),
}
