//! Filing discovery and filtering services.

use filing_models::{
    Cik, CompanyId, CompanyIdentity, FilingForm, FilingMetadata, FilingUrls, FiscalPeriod,
    FiscalQuarter, SourceType, Ticker,
};
use sec_client::{
    RecentFilingLists, SecEndpointCatalog, SubmissionsFileReference, SubmissionsResponse,
    TickerLookupRecord,
};
use std::collections::BTreeMap;
use thiserror::Error;
use time::{Date, Month, format_description::well_known::Iso8601};

#[derive(Debug, Error)]
pub enum FilingDiscoveryError {
    #[error("ticker {ticker} was not found in the SEC ticker index")]
    UnknownTicker { ticker: String },
    #[error("SEC submissions payload could not be converted into filing metadata: {reason}")]
    InvalidSubmissions { reason: String },
    #[error("invalid date in SEC response: {value}")]
    InvalidDate { value: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilingDiscoveryService {
    endpoints: SecEndpointCatalog,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilingHistoryPlan {
    pub selected_filings: Vec<FilingMetadata>,
    pub historical_files_to_fetch: Vec<SubmissionsFileReference>,
    pub coverage: FilingHistoryCoverage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilingHistoryCoverage {
    pub requested_years: u8,
    pub earliest_required_year: Option<i32>,
    pub earliest_selected_year: Option<i32>,
    pub latest_selected_year: Option<i32>,
    pub has_requested_year_span: bool,
}

impl FilingDiscoveryService {
    pub fn new(endpoints: SecEndpointCatalog) -> Self {
        Self { endpoints }
    }

    pub fn resolve_cik_from_ticker(
        &self,
        ticker: &Ticker,
        records: &[TickerLookupRecord],
    ) -> Result<Cik, FilingDiscoveryError> {
        records
            .iter()
            .find(|record| record.ticker.eq_ignore_ascii_case(ticker.as_str()))
            .map(TickerLookupRecord::cik)
            .ok_or_else(|| FilingDiscoveryError::UnknownTicker {
                ticker: ticker.as_str().to_string(),
            })
    }

    pub fn filter_original_filings(&self, filings: Vec<FilingMetadata>) -> Vec<FilingMetadata> {
        let mut filtered: Vec<FilingMetadata> = filings
            .into_iter()
            .filter(|filing| !filing.is_amendment && filing.form_type.is_supported_v1())
            .collect();

        filtered.sort_by(|left, right| {
            right
                .report_period_end
                .cmp(&left.report_period_end)
                .then_with(|| right.filing_date.cmp(&left.filing_date))
        });

        filtered
    }

    pub fn filter_original_filings_for_years(
        &self,
        filings: Vec<FilingMetadata>,
        years: u8,
    ) -> Vec<FilingMetadata> {
        let filtered = self.filter_original_filings(filings);
        let Some(latest_year) = filtered
            .iter()
            .filter_map(|filing| filing.report_period_end)
            .map(|date| date.year())
            .max()
        else {
            return filtered;
        };
        let earliest_year = latest_year - i32::from(years.max(1)) + 1;

        filtered
            .into_iter()
            .filter(|filing| {
                filing.report_period_end.map(|date| date.year() >= earliest_year).unwrap_or(true)
            })
            .collect()
    }

    pub fn plan_filing_history_from_submissions(
        &self,
        cik: &Cik,
        recent_response: &SubmissionsResponse,
        historical_filing_lists: &[RecentFilingLists],
        years: u8,
    ) -> Result<FilingHistoryPlan, FilingDiscoveryError> {
        let mut filings = self.filings_from_submissions(cik, recent_response)?;

        for historical_filing_list in historical_filing_lists {
            filings.extend(self.filings_from_recent_filing_lists(cik, historical_filing_list)?);
        }

        let merged = merge_filings_by_accession(filings);
        let selected_filings = self.filter_original_filings_for_years(merged, years);
        let coverage = filing_history_coverage(&selected_filings, years);
        let historical_files_to_fetch = if coverage.has_requested_year_span {
            Vec::new()
        } else {
            recent_response.filings.files.clone()
        };

        Ok(FilingHistoryPlan { selected_filings, historical_files_to_fetch, coverage })
    }

    pub fn filings_from_submissions(
        &self,
        cik: &Cik,
        response: &SubmissionsResponse,
    ) -> Result<Vec<FilingMetadata>, FilingDiscoveryError> {
        self.filings_from_recent_filing_lists(cik, &response.filings.recent)
    }

    pub fn filings_from_recent_filing_lists(
        &self,
        cik: &Cik,
        recent: &RecentFilingLists,
    ) -> Result<Vec<FilingMetadata>, FilingDiscoveryError> {
        let total = recent.accession_number.len();

        if recent.form.len() != total
            || recent.filing_date.len() != total
            || recent.report_date.len() != total
            || recent.primary_document.len() != total
        {
            return Err(FilingDiscoveryError::InvalidSubmissions {
                reason: "recent filing arrays are not aligned".to_string(),
            });
        }

        let mut filings = Vec::with_capacity(total);

        for index in 0..total {
            let form_text = &recent.form[index];
            let form_type = parse_form(form_text);
            let accession_number = recent.accession_number[index].clone();
            let filing_date = parse_sec_date(&recent.filing_date[index])?;
            let report_period_end = parse_optional_sec_date(&recent.report_date[index])?;
            let primary_document = recent.primary_document[index].clone();

            filings.push(FilingMetadata {
                accession_number: accession_number.clone(),
                form_type: form_type.clone(),
                filing_date,
                report_period_end,
                fiscal_period: infer_fiscal_period(&form_type, report_period_end),
                filing_urls: FilingUrls {
                    filing_detail: Some(
                        self.endpoints.filing_directory_url(cik, &accession_number),
                    ),
                    primary_document: Some(self.endpoints.filing_primary_document_url(
                        cik,
                        &accession_number,
                        &primary_document,
                    )),
                    xbrl_instance: None,
                    html_index: Some(self.endpoints.filing_directory_url(cik, &accession_number)),
                },
                source_types: vec![SourceType::Html, SourceType::Xbrl],
                is_amendment: form_text.ends_with("/A"),
            });
        }

        Ok(filings)
    }

    pub fn describe_company(&self, company_id: &CompanyId) -> String {
        format!("filing discovery for {company_id}")
    }

    pub fn company_identity_from_submissions(
        &self,
        primary_id: CompanyId,
        response: &SubmissionsResponse,
    ) -> CompanyIdentity {
        CompanyIdentity {
            primary_id,
            ticker: response.tickers.first().cloned().map(Ticker::new),
            cik: Some(Cik::new(response.cik.clone())),
            issuer_name: response.name.clone(),
            exchange: response.exchanges.first().cloned(),
            reported_currency: None,
            fiscal_year_end: response.fiscal_year_end.clone(),
        }
    }
}

fn merge_filings_by_accession(filings: Vec<FilingMetadata>) -> Vec<FilingMetadata> {
    let mut by_accession = BTreeMap::new();

    for filing in filings {
        by_accession.entry(filing.accession_number.clone()).or_insert(filing);
    }

    by_accession.into_values().collect()
}

fn filing_history_coverage(
    filings: &[FilingMetadata],
    requested_years: u8,
) -> FilingHistoryCoverage {
    let latest_selected_year =
        filings.iter().filter_map(|filing| filing.report_period_end).map(|date| date.year()).max();
    let earliest_selected_year =
        filings.iter().filter_map(|filing| filing.report_period_end).map(|date| date.year()).min();
    let earliest_required_year =
        latest_selected_year.map(|year| year - i32::from(requested_years.max(1)) + 1);
    let has_requested_year_span = match (earliest_selected_year, earliest_required_year) {
        (Some(earliest_selected_year), Some(earliest_required_year)) => {
            earliest_selected_year <= earliest_required_year
        }
        _ => false,
    };

    FilingHistoryCoverage {
        requested_years,
        earliest_required_year,
        earliest_selected_year,
        latest_selected_year,
        has_requested_year_span,
    }
}

fn parse_form(value: &str) -> FilingForm {
    match value {
        "10-K" | "10-K/A" => FilingForm::Form10K,
        "10-Q" | "10-Q/A" => FilingForm::Form10Q,
        "20-F" | "20-F/A" => FilingForm::Form20F,
        "6-K" => FilingForm::Form6K,
        "8-K" => FilingForm::Form8K,
        other => FilingForm::Other(other.to_string()),
    }
}

fn parse_sec_date(value: &str) -> Result<Date, FilingDiscoveryError> {
    Date::parse(value, &Iso8601::DATE)
        .map_err(|_| FilingDiscoveryError::InvalidDate { value: value.to_string() })
}

fn parse_optional_sec_date(value: &str) -> Result<Option<Date>, FilingDiscoveryError> {
    if value.trim().is_empty() {
        return Ok(None);
    }

    parse_sec_date(value).map(Some)
}

fn infer_fiscal_period(
    form_type: &FilingForm,
    report_period_end: Option<Date>,
) -> Option<FiscalPeriod> {
    let report_period_end = report_period_end?;

    let fiscal_quarter = match form_type {
        FilingForm::Form10K => Some(FiscalQuarter::Q4),
        FilingForm::Form10Q => quarter_for_month(report_period_end.month()),
        _ => None,
    };

    Some(FiscalPeriod { fiscal_year: report_period_end.year(), fiscal_quarter })
}

fn quarter_for_month(month: Month) -> Option<FiscalQuarter> {
    match month {
        Month::January | Month::February | Month::March => Some(FiscalQuarter::Q1),
        Month::April | Month::May | Month::June => Some(FiscalQuarter::Q2),
        Month::July | Month::August | Month::September => Some(FiscalQuarter::Q3),
        Month::October | Month::November | Month::December => Some(FiscalQuarter::Q4),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn discovery_service() -> FilingDiscoveryService {
        FilingDiscoveryService::new(SecEndpointCatalog {
            submissions_base_url: "https://data.sec.gov/submissions".to_string(),
            data_base_url: "https://data.sec.gov".to_string(),
            archives_base_url: "https://www.sec.gov/Archives".to_string(),
            company_tickers_url: "https://www.sec.gov/files/company_tickers.json".to_string(),
        })
    }

    #[test]
    fn resolves_cik_from_ticker_case_insensitively() {
        let service = discovery_service();
        let records = vec![TickerLookupRecord {
            cik_str: "798354".to_string(),
            ticker: "AAPL".to_string(),
            title: "Example Corp".to_string(),
        }];

        let cik = service
            .resolve_cik_from_ticker(&Ticker::new("aapl"), &records)
            .expect("ticker should resolve");

        assert_eq!(cik.as_str(), "0000798354");
    }

    #[test]
    fn filings_from_submissions_marks_amendments_and_builds_urls() {
        let service = discovery_service();
        let cik = Cik::new("798354");
        let response = SubmissionsResponse {
            cik: cik.as_str().to_string(),
            name: "Example Corp".to_string(),
            tickers: vec!["AAPL".to_string()],
            exchanges: vec!["NYSE".to_string()],
            fiscal_year_end: Some("1231".to_string()),
            forms: vec!["10-K".to_string()],
            filings: sec_client::SubmissionsFilings {
                recent: sec_client::RecentFilingLists {
                    accession_number: vec![
                        "0000798354-25-000010".to_string(),
                        "0000798354-25-000011".to_string(),
                    ],
                    filing_date: vec!["2025-02-01".to_string(), "2025-03-01".to_string()],
                    report_date: vec!["2024-12-31".to_string(), "2024-12-31".to_string()],
                    form: vec!["10-K".to_string(), "10-K/A".to_string()],
                    primary_document: vec!["form10k.htm".to_string(), "form10ka.htm".to_string()],
                },
                files: Vec::new(),
            },
        };

        let filings = service
            .filings_from_submissions(&cik, &response)
            .expect("submissions payload should parse");

        assert_eq!(filings.len(), 2);
        assert!(!filings[0].is_amendment);
        assert!(filings[1].is_amendment);
        assert_eq!(
            filings[0].filing_urls.primary_document.as_deref(),
            Some("https://www.sec.gov/Archives/edgar/data/798354/000079835425000010/form10k.htm")
        );
    }

    #[test]
    fn filter_original_filings_excludes_amendments_and_keeps_supported_forms() {
        let service = discovery_service();
        let cik = Cik::new("798354");
        let filings = service
            .filings_from_submissions(
                &cik,
                &SubmissionsResponse {
                    cik: cik.as_str().to_string(),
                    name: "Example Corp".to_string(),
                    tickers: vec!["AAPL".to_string()],
                    exchanges: vec!["NYSE".to_string()],
                    fiscal_year_end: Some("1231".to_string()),
                    forms: vec![],
                    filings: sec_client::SubmissionsFilings {
                        recent: sec_client::RecentFilingLists {
                            accession_number: vec![
                                "0000798354-25-000010".to_string(),
                                "0000798354-25-000011".to_string(),
                                "0000798354-25-000012".to_string(),
                            ],
                            filing_date: vec![
                                "2025-02-01".to_string(),
                                "2025-03-01".to_string(),
                                "2025-04-01".to_string(),
                            ],
                            report_date: vec![
                                "2024-12-31".to_string(),
                                "2024-12-31".to_string(),
                                "2025-03-31".to_string(),
                            ],
                            form: vec!["10-K".to_string(), "10-K/A".to_string(), "8-K".to_string()],
                            primary_document: vec![
                                "form10k.htm".to_string(),
                                "form10ka.htm".to_string(),
                                "form8k.htm".to_string(),
                            ],
                        },
                        files: Vec::new(),
                    },
                },
            )
            .expect("fixture submissions should parse");

        let filtered = service.filter_original_filings(filings);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].form_type.as_str(), "10-K");
    }

    #[test]
    fn plans_historical_file_fetch_when_recent_filings_do_not_cover_requested_years() {
        let service = discovery_service();
        let cik = Cik::new("798354");
        let response = SubmissionsResponse {
            cik: cik.as_str().to_string(),
            name: "Example Corp".to_string(),
            tickers: vec!["AAPL".to_string()],
            exchanges: vec!["NYSE".to_string()],
            fiscal_year_end: Some("1231".to_string()),
            forms: vec![],
            filings: sec_client::SubmissionsFilings {
                recent: sec_client::RecentFilingLists {
                    accession_number: vec!["0000798354-25-000010".to_string()],
                    filing_date: vec!["2025-02-01".to_string()],
                    report_date: vec!["2024-12-31".to_string()],
                    form: vec!["10-K".to_string()],
                    primary_document: vec!["form10k.htm".to_string()],
                },
                files: vec![sec_client::SubmissionsFileReference {
                    name: "CIK0000798354-submissions-001.json".to_string(),
                    filing_count: Some(100),
                    filing_from: Some("2014-01-01".to_string()),
                    filing_to: Some("2020-01-01".to_string()),
                }],
            },
        };

        let plan = service
            .plan_filing_history_from_submissions(&cik, &response, &[], 10)
            .expect("history plan should build");

        assert_eq!(plan.selected_filings.len(), 1);
        assert!(!plan.coverage.has_requested_year_span);
        assert_eq!(plan.historical_files_to_fetch.len(), 1);
    }

    #[test]
    fn merges_historical_submissions_and_clears_fetch_plan_when_coverage_is_met() {
        let service = discovery_service();
        let cik = Cik::new("798354");
        let recent_response = SubmissionsResponse {
            cik: cik.as_str().to_string(),
            name: "Example Corp".to_string(),
            tickers: vec!["AAPL".to_string()],
            exchanges: vec!["NYSE".to_string()],
            fiscal_year_end: Some("1231".to_string()),
            forms: vec![],
            filings: sec_client::SubmissionsFilings {
                recent: sec_client::RecentFilingLists {
                    accession_number: vec!["0000798354-25-000010".to_string()],
                    filing_date: vec!["2025-02-01".to_string()],
                    report_date: vec!["2024-12-31".to_string()],
                    form: vec!["10-K".to_string()],
                    primary_document: vec!["form10k.htm".to_string()],
                },
                files: vec![sec_client::SubmissionsFileReference {
                    name: "CIK0000798354-submissions-001.json".to_string(),
                    filing_count: Some(100),
                    filing_from: Some("2014-01-01".to_string()),
                    filing_to: Some("2020-01-01".to_string()),
                }],
            },
        };
        let historical_filing_list = sec_client::RecentFilingLists {
            accession_number: vec!["0000798354-16-000010".to_string()],
            filing_date: vec!["2016-02-01".to_string()],
            report_date: vec!["2015-12-31".to_string()],
            form: vec!["10-K".to_string()],
            primary_document: vec!["form10k.htm".to_string()],
        };

        let plan = service
            .plan_filing_history_from_submissions(
                &cik,
                &recent_response,
                &[historical_filing_list],
                10,
            )
            .expect("history plan should build");

        assert_eq!(plan.selected_filings.len(), 2);
        assert!(plan.coverage.has_requested_year_span);
        assert!(plan.historical_files_to_fetch.is_empty());
    }
}
