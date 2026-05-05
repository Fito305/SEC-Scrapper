//! Future API adapter seam.

use filing_models::{CompanyIdentity, FilingMetadata, Provenance};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompanySummaryDto {
    pub issuer_name: String,
    pub ticker: Option<String>,
    pub cik: Option<String>,
}

impl From<&CompanyIdentity> for CompanySummaryDto {
    fn from(value: &CompanyIdentity) -> Self {
        Self {
            issuer_name: value.issuer_name.clone(),
            ticker: value.ticker.as_ref().map(|ticker| ticker.as_str().to_string()),
            cik: value.cik.as_ref().map(|cik| cik.as_str().to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilingSummaryDto {
    pub accession_number: String,
    pub form_type: String,
}

impl From<&FilingMetadata> for FilingSummaryDto {
    fn from(value: &FilingMetadata) -> Self {
        Self {
            accession_number: value.accession_number.clone(),
            form_type: value.form_type.as_str().to_string(),
        }
    }
}

pub trait FilingQueryService {
    fn list_filings(&self) -> Vec<FilingSummaryDto>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkbookViewRequestDto {
    pub company_cik: String,
    pub workbook_schema_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceRequestDto {
    pub metric_id: String,
    pub period_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceDto {
    pub accession_number: String,
    pub source_type: String,
    pub filing_label: Option<String>,
    pub xbrl_tag: Option<String>,
}

impl From<&Provenance> for ProvenanceDto {
    fn from(value: &Provenance) -> Self {
        Self {
            accession_number: value.accession_number.clone(),
            source_type: format!("{:?}", value.source_type),
            filing_label: value.filing_label.clone(),
            xbrl_tag: value.xbrl_tag.clone(),
        }
    }
}
