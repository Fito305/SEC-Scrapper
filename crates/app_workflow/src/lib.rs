//! End-to-end application workflows.
//!
//! This crate connects the lower-level crates without forcing them to know about each other. The
//! CLI should call these workflows instead of reimplementing orchestration logic.

use accounting_domains::MetricId;
use app_core::AppConfig;
use filing_discovery::{FilingDiscoveryError, FilingDiscoveryService, FilingHistoryCoverage};
use filing_models::{Cik, CompanyId, CompanyIdentity, FilingMetadata, SourceType, Ticker};
use html_extractor::{HtmlExtractionError, HtmlExtractionResult, HtmlExtractor};
use normalization::{
    NormalizationIssue, NormalizationIssueSeverity, NormalizationResult, Normalizer,
};
use sec_client::{
    CompanyTickersResponse, RecentFilingLists, SecClient, SecClientError, SubmissionsResponse,
    ticker_lookup_records_from_response,
};
use std::{collections::BTreeSet, path::Path, time::Instant};
use thiserror::Error;
use tokio::task::JoinSet;
use valuation::{ValuationEngine, ValuationOutput};
use workbook_io::{WorkbookExporter, WorkbookIoError};
use xbrl_extractor::{CompanyFactsResponse, XbrlExtractionError, XbrlExtractor};

const HTML_EXTRACTION_CONCURRENCY: usize = 3;

#[derive(Debug, Error)]
pub enum WorkflowError {
    #[error("SEC client error: {0}")]
    SecClient(#[from] SecClientError),
    #[error("filing discovery error: {0}")]
    FilingDiscovery(#[from] FilingDiscoveryError),
    #[error("XBRL extraction error: {0}")]
    Xbrl(#[from] XbrlExtractionError),
    #[error("HTML extraction error: {0}")]
    Html(#[from] HtmlExtractionError),
    #[error("workbook error: {0}")]
    Workbook(#[from] WorkbookIoError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchExportRequest {
    pub company_id: CompanyId,
    pub years: u8,
    pub include_html_fallback: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FetchExportSummary {
    pub company: CompanyIdentity,
    pub selected_filings: Vec<FilingMetadata>,
    pub coverage: FilingHistoryCoverage,
    pub xbrl_metric_count: usize,
    pub html_fallback_metric_count: usize,
    pub narrative_section_count: usize,
    pub normalized_metric_count: usize,
    pub valuation_output_count: usize,
    pub review_issue_count: usize,
    pub stage_timings_ms: FetchExportStageTimings,
    pub slowest_html_filings: Vec<HtmlFilingTiming>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FetchExportStageTimings {
    pub resolve_cik_ms: u128,
    pub discover_filings_ms: u128,
    pub fetch_company_facts_ms: u128,
    pub extract_xbrl_ms: u128,
    pub extract_html_ms: u128,
    pub normalize_ms: u128,
    pub valuation_ms: u128,
    pub workbook_export_ms: u128,
    pub total_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlFilingTiming {
    pub accession_number: String,
    pub form_type: String,
    pub download_ms: u128,
    pub extract_ms: u128,
    pub total_ms: u128,
}

#[derive(Debug, Clone, PartialEq)]
struct HtmlFilingBatch {
    numeric_fallbacks: Vec<html_extractor::ExtractedHtmlMetricValue>,
    narrative_sections: Vec<html_extractor::ExtractedNarrativeSection>,
    issues: Vec<NormalizationIssue>,
    timing: Option<HtmlFilingTiming>,
}

pub fn export_fixture_dataset_to_path(
    company: CompanyIdentity,
    filings: Vec<FilingMetadata>,
    company_facts_payload: &str,
    html_documents: Vec<String>,
    output_path: impl AsRef<Path>,
) -> Result<FetchExportSummary, WorkflowError> {
    let xbrl_extractor = XbrlExtractor::default();
    let html_extractor = HtmlExtractor::default();
    let normalizer = Normalizer::new();
    let valuation_engine = ValuationEngine::new();
    let workbook_exporter = WorkbookExporter::new();

    let company_facts = xbrl_extractor.parse_company_facts_json(company_facts_payload)?;
    let xbrl_metrics = xbrl_extractor.extract_for_filings(&company_facts, &filings)?;

    let mut html_result = HtmlExtractionResult::default();
    for (filing, html) in filings.iter().zip(html_documents.iter()) {
        let extracted = html_extractor.extract(html, filing)?;
        html_result.numeric_fallbacks.extend(extracted.numeric_fallbacks);
        html_result.narrative_sections.extend(extracted.narrative_sections);
    }

    keep_html_only_where_xbrl_is_missing(&xbrl_metrics, &mut html_result);
    let mut normalized = normalizer.normalize(&xbrl_metrics, &html_result);
    let (valuation_outputs, valuation_issues) =
        compute_placeholder_valuation_outputs(&valuation_engine, &normalized);
    normalized.issues.extend(valuation_issues);
    append_analyst_review_issues(&mut normalized);

    let review_issue_count = normalized.issues.len();
    let workbook = workbook_exporter.build_model_with_filings(
        company.clone(),
        &filings,
        &normalized,
        &valuation_outputs,
    );
    workbook_exporter.export_to_path(&workbook, output_path)?;

    Ok(FetchExportSummary {
        company,
        selected_filings: filings,
        coverage: FilingHistoryCoverage {
            requested_years: 0,
            earliest_required_year: None,
            earliest_selected_year: None,
            latest_selected_year: None,
            has_requested_year_span: false,
        },
        xbrl_metric_count: xbrl_metrics.len(),
        html_fallback_metric_count: html_result.numeric_fallbacks.len(),
        narrative_section_count: html_result.narrative_sections.len(),
        normalized_metric_count: normalized.numeric_metrics.len(),
        valuation_output_count: valuation_outputs.len(),
        review_issue_count,
        stage_timings_ms: FetchExportStageTimings::default(),
        slowest_html_filings: Vec::new(),
    })
}

#[derive(Debug)]
pub struct SecFetchExportWorkflow {
    sec_client: SecClient,
    filing_discovery: FilingDiscoveryService,
    xbrl_extractor: XbrlExtractor,
    html_extractor: HtmlExtractor,
    normalizer: Normalizer,
    valuation_engine: ValuationEngine,
    workbook_exporter: WorkbookExporter,
}

impl SecFetchExportWorkflow {
    pub fn from_config(config: &AppConfig) -> Result<Self, WorkflowError> {
        let sec_client = SecClient::from_app_config(config)?;
        Ok(Self::new(sec_client))
    }

    pub fn new(sec_client: SecClient) -> Self {
        Self {
            filing_discovery: FilingDiscoveryService::new(sec_client.endpoints().clone()),
            sec_client,
            xbrl_extractor: XbrlExtractor::default(),
            html_extractor: HtmlExtractor::default(),
            normalizer: Normalizer::new(),
            valuation_engine: ValuationEngine::new(),
            workbook_exporter: WorkbookExporter::new(),
        }
    }

    pub async fn fetch_export_to_path(
        &self,
        request: FetchExportRequest,
        output_path: impl AsRef<Path>,
    ) -> Result<FetchExportSummary, WorkflowError> {
        let total_started = Instant::now();

        let started = Instant::now();
        let cik = self.resolve_cik(&request.company_id).await?;
        let resolve_cik_ms = started.elapsed().as_millis();

        let started = Instant::now();
        let discovery_result =
            self.discover_filings(&cik, request.company_id.clone(), request.years).await?;
        let discover_filings_ms = started.elapsed().as_millis();

        let started = Instant::now();
        let company_facts = self.fetch_company_facts(&cik).await?;
        let fetch_company_facts_ms = started.elapsed().as_millis();

        let started = Instant::now();
        let xbrl_metrics =
            self.xbrl_extractor.extract_for_filings(&company_facts, &discovery_result.filings)?;
        let extract_xbrl_ms = started.elapsed().as_millis();

        let started = Instant::now();
        let (mut html_result, html_issues, slowest_html_filings) = if request.include_html_fallback
        {
            self.extract_html_for_filings(&discovery_result.filings).await
        } else {
            (HtmlExtractionResult::default(), Vec::new(), Vec::new())
        };
        let extract_html_ms = started.elapsed().as_millis();

        keep_html_only_where_xbrl_is_missing(&xbrl_metrics, &mut html_result);

        let started = Instant::now();
        let mut normalized = self.normalizer.normalize(&xbrl_metrics, &html_result);
        normalized.issues.extend(html_issues);
        let normalize_ms = started.elapsed().as_millis();

        let started = Instant::now();
        let (valuation_outputs, valuation_issues) = self.compute_valuation_outputs(&normalized);
        normalized.issues.extend(valuation_issues);
        append_analyst_review_issues(&mut normalized);
        let valuation_ms = started.elapsed().as_millis();

        let review_issue_count = normalized.issues.len();
        let workbook = self.workbook_exporter.build_model_with_filings(
            discovery_result.company.clone(),
            &discovery_result.filings,
            &normalized,
            &valuation_outputs,
        );

        let started = Instant::now();
        self.workbook_exporter.export_to_path(&workbook, output_path)?;
        let workbook_export_ms = started.elapsed().as_millis();

        let stage_timings_ms = FetchExportStageTimings {
            resolve_cik_ms,
            discover_filings_ms,
            fetch_company_facts_ms,
            extract_xbrl_ms,
            extract_html_ms,
            normalize_ms,
            valuation_ms,
            workbook_export_ms,
            total_ms: total_started.elapsed().as_millis(),
        };

        Ok(FetchExportSummary {
            company: discovery_result.company,
            selected_filings: discovery_result.filings,
            coverage: discovery_result.coverage,
            xbrl_metric_count: xbrl_metrics.len(),
            html_fallback_metric_count: html_result.numeric_fallbacks.len(),
            narrative_section_count: html_result.narrative_sections.len(),
            normalized_metric_count: normalized.numeric_metrics.len(),
            valuation_output_count: valuation_outputs.len(),
            review_issue_count,
            stage_timings_ms,
            slowest_html_filings,
        })
    }

    async fn resolve_cik(&self, company_id: &CompanyId) -> Result<Cik, WorkflowError> {
        match company_id {
            CompanyId::Cik(cik) => Ok(cik.clone()),
            CompanyId::Ticker(ticker) => self.resolve_ticker(ticker).await,
        }
    }

    async fn resolve_ticker(&self, ticker: &Ticker) -> Result<Cik, WorkflowError> {
        let request = self.sec_client.company_tickers_request();
        let response: CompanyTickersResponse = self.sec_client.get_json(&request).await?;
        let records = ticker_lookup_records_from_response(response);
        Ok(self.filing_discovery.resolve_cik_from_ticker(ticker, &records)?)
    }

    async fn discover_filings(
        &self,
        cik: &Cik,
        company_id: CompanyId,
        years: u8,
    ) -> Result<DiscoveredFilingSet, WorkflowError> {
        let recent_request = self.sec_client.submissions_request(cik);
        let recent_response: SubmissionsResponse =
            self.sec_client.get_json(&recent_request).await?;
        let mut plan = self.filing_discovery.plan_filing_history_from_submissions(
            cik,
            &recent_response,
            &[],
            years,
        )?;

        let mut historical_filing_lists = Vec::new();
        if !plan.historical_files_to_fetch.is_empty() {
            for file in &plan.historical_files_to_fetch {
                let request = self.sec_client.submissions_file_request(&file.name);
                historical_filing_lists
                    .push(self.sec_client.get_json::<RecentFilingLists>(&request).await?);
            }

            plan = self.filing_discovery.plan_filing_history_from_submissions(
                cik,
                &recent_response,
                &historical_filing_lists,
                years,
            )?;
        }

        Ok(DiscoveredFilingSet {
            company: self
                .filing_discovery
                .company_identity_from_submissions(company_id, &recent_response),
            filings: plan.selected_filings,
            coverage: plan.coverage,
        })
    }

    async fn fetch_company_facts(&self, cik: &Cik) -> Result<CompanyFactsResponse, WorkflowError> {
        let request = self.sec_client.company_facts_request(cik);
        Ok(self.sec_client.get_json(&request).await?)
    }

    async fn extract_html_for_filings(
        &self,
        filings: &[FilingMetadata],
    ) -> (HtmlExtractionResult, Vec<NormalizationIssue>, Vec<HtmlFilingTiming>) {
        let mut combined = HtmlExtractionResult::default();
        let mut issues = Vec::new();
        let mut timings = Vec::new();
        let mut join_set = JoinSet::new();

        for filing in filings.iter().cloned() {
            if filing.filing_urls.primary_document.is_none() {
                continue;
            }

            let sec_client = self.sec_client.clone();
            let html_extractor = self.html_extractor.clone();
            join_set.spawn(async move {
                extract_single_html_filing(sec_client, html_extractor, filing).await
            });

            if join_set.len() >= HTML_EXTRACTION_CONCURRENCY {
                if let Some(batch) = join_set.join_next().await {
                    merge_html_filing_batch(batch.expect("html extraction task should not panic"), &mut combined, &mut issues, &mut timings);
                }
            }
        }

        while let Some(batch) = join_set.join_next().await {
            merge_html_filing_batch(batch.expect("html extraction task should not panic"), &mut combined, &mut issues, &mut timings);
        }

        timings = top_slowest_html_timings(timings);

        (combined, issues, timings)
    }

    fn compute_valuation_outputs(
        &self,
        normalized: &NormalizationResult,
    ) -> (Vec<ValuationOutput>, Vec<NormalizationIssue>) {
        compute_placeholder_valuation_outputs(&self.valuation_engine, normalized)
    }
}

fn compute_placeholder_valuation_outputs(
    valuation_engine: &ValuationEngine,
    normalized: &NormalizationResult,
) -> (Vec<ValuationOutput>, Vec<NormalizationIssue>) {
    // Valuation formulas are placeholders. Missing formula inputs should not prevent the SEC
    // extraction workbook from being produced; the workbook review sheets/provenance remain the
    // source of truth for what data was available.
    match valuation_engine.compute_placeholder_outputs(normalized) {
        Ok(outputs) => (outputs, Vec::new()),
        Err(error) => (
            Vec::new(),
            vec![NormalizationIssue {
                severity: NormalizationIssueSeverity::Warning,
                code: "valuation_placeholder_skipped",
                metric_id: None,
                period_key: None,
                segment_name: None,
                message: format!(
                    "Placeholder valuation outputs were skipped because required inputs were unavailable: {error}"
                ),
            }],
        ),
    }
}

fn append_analyst_review_issues(normalized: &mut NormalizationResult) {
    for metric_id in CRITICAL_ANALYST_METRICS {
        let matching: Vec<_> = normalized
            .numeric_metrics
            .iter()
            .filter(|metric| metric.metric_id.as_str() == *metric_id)
            .collect();

        if matching.is_empty() {
            normalized.issues.push(NormalizationIssue {
                severity: NormalizationIssueSeverity::Warning,
                code: "analyst_critical_metric_missing",
                metric_id: Some(MetricId::new(*metric_id)),
                period_key: None,
                segment_name: None,
                message: format!(
                    "Critical analyst metric {metric_id} was not found in XBRL or conservative HTML fallback. Review missing_metrics and consider adding exact XBRL aliases before relying on the workbook."
                ),
            });
            continue;
        }

        for metric in matching {
            if metric.primary_source == normalization::NormalizationSource::HtmlFallback
                && metric.value.provenance.source_type != SourceType::Xbrl
            {
                normalized.issues.push(NormalizationIssue {
                    severity: NormalizationIssueSeverity::Warning,
                    code: "analyst_critical_metric_html_only",
                    metric_id: Some(metric.metric_id.clone()),
                    period_key: Some(metric.period_key.clone()),
                    segment_name: metric
                        .value
                        .provenance
                        .source_location
                        .segment_name
                        .clone(),
                    message: format!(
                        "Critical analyst metric {} for period {} was sourced only from conservative HTML fallback. Verify the filing label/provenance before using it in analysis.",
                        metric.metric_id.as_str(),
                        metric.period_key
                    ),
                });
            }
        }
    }
}

const CRITICAL_ANALYST_METRICS: &[&str] = &[
    "balance_sheet.cash_and_equivalents",
    "balance_sheet.total_assets",
    "balance_sheet.total_liabilities",
    "balance_sheet.total_equity",
    "income_statement.revenue",
    "income_statement.operating_income",
    "income_statement.net_income",
    "cash_flow.net_cash_from_operations",
    "cash_flow.capital_expenditures",
    "cash_flow.stock_repurchases",
    "shareholders_equity.shares_outstanding",
    "equity_compensation.stock_based_comp_expense",
];

fn keep_html_only_where_xbrl_is_missing(
    xbrl_metrics: &[xbrl_extractor::ExtractedMetricValue],
    html_result: &mut HtmlExtractionResult,
) {
    // HTML fallback can now emit multiple explicit periods from one table.
    // Only drop an HTML value when XBRL already covers the same metric and the
    // same reporting period shape:
    // - instants still de-duplicate by period end, which preserves the existing
    //   cross-context behavior for balance-sheet style tables
    // - durations de-duplicate by exact start + end so overlapping same-end
    //   values like 3M and 6M stay distinct
    let xbrl_instant_end_periods: BTreeSet<String> = xbrl_metrics
        .iter()
        .map(|metric| {
            format!(
                "{}::{}",
                metric.metric_id.as_str(),
                reporting_period_end_key(&metric.numeric_value.reporting_period)
            )
        })
        .collect();
    let xbrl_duration_periods: BTreeSet<String> = xbrl_metrics
        .iter()
        .filter_map(|metric| match &metric.numeric_value.reporting_period.context {
            filing_models::PeriodContext::Duration { .. } => Some(format!(
                "{}::{}",
                metric.metric_id.as_str(),
                reporting_period_duration_key(&metric.numeric_value.reporting_period)
            )),
            filing_models::PeriodContext::Instant { .. } => None,
        })
        .collect();

    html_result.numeric_fallbacks.retain(|metric| {
        match &metric.numeric_value.reporting_period.context {
            filing_models::PeriodContext::Instant { .. } => {
                let key = format!(
                    "{}::{}",
                    metric.metric_id.as_str(),
                    reporting_period_end_key(&metric.numeric_value.reporting_period)
                );
                !xbrl_instant_end_periods.contains(&key)
            }
            filing_models::PeriodContext::Duration { .. } => {
                let key = format!(
                    "{}::{}",
                    metric.metric_id.as_str(),
                    reporting_period_duration_key(&metric.numeric_value.reporting_period)
                );
                !xbrl_duration_periods.contains(&key)
            }
        }
    });
}

fn reporting_period_end_key(reporting_period: &filing_models::ReportingPeriod) -> String {
    match &reporting_period.context {
        filing_models::PeriodContext::Instant { as_of } => as_of.to_string(),
        filing_models::PeriodContext::Duration { end, .. } => end.to_string(),
    }
}

fn reporting_period_duration_key(reporting_period: &filing_models::ReportingPeriod) -> String {
    match &reporting_period.context {
        filing_models::PeriodContext::Instant { as_of } => as_of.to_string(),
        filing_models::PeriodContext::Duration { start, end } => {
            format!("{start}::{end}")
        }
    }
}

fn html_warning_issue(filing: &FilingMetadata, message: String) -> NormalizationIssue {
    NormalizationIssue {
        severity: NormalizationIssueSeverity::Warning,
        code: "html_fallback_skipped",
        metric_id: None,
        period_key: filing.report_period_end.map(|date| date.to_string()),
        segment_name: None,
        message: format!("{} ({})", message, filing.accession_number),
    }
}

async fn extract_single_html_filing(
    sec_client: SecClient,
    html_extractor: HtmlExtractor,
    filing: FilingMetadata,
) -> HtmlFilingBatch {
    let Some(primary_document_url) = &filing.filing_urls.primary_document else {
        return HtmlFilingBatch {
            numeric_fallbacks: Vec::new(),
            narrative_sections: Vec::new(),
            issues: Vec::new(),
            timing: None,
        };
    };

    let request = sec_client::SecRequest {
        endpoint_class: sec_client::EndpointClass::FilingDocument,
        source_method: filing_models::FilingSourceMethod::FilingHtml,
        url: primary_document_url.clone(),
        description: format!("primary filing document {}", filing.accession_number),
    };

    let started = Instant::now();
    let html = match sec_client.get_text(&request).await {
        Ok(html) => html,
        Err(error) => {
            return HtmlFilingBatch {
                numeric_fallbacks: Vec::new(),
                narrative_sections: Vec::new(),
                issues: vec![html_warning_issue(
                    &filing,
                    format!("HTML fallback download failed and was skipped: {error}"),
                )],
                timing: None,
            };
        }
    };
    let download_ms = started.elapsed().as_millis();

    let started = Instant::now();
    let extracted = match html_extractor.extract(&html, &filing) {
        Ok(extracted) => extracted,
        Err(error) => {
            return HtmlFilingBatch {
                numeric_fallbacks: Vec::new(),
                narrative_sections: Vec::new(),
                issues: vec![html_warning_issue(
                    &filing,
                    format!("HTML fallback extraction failed and was skipped: {error}"),
                )],
                timing: None,
            };
        }
    };
    let extract_ms = started.elapsed().as_millis();

    HtmlFilingBatch {
        numeric_fallbacks: extracted.numeric_fallbacks,
        narrative_sections: extracted.narrative_sections,
        issues: Vec::new(),
        timing: Some(HtmlFilingTiming {
            accession_number: filing.accession_number,
            form_type: filing.form_type.as_str().to_string(),
            download_ms,
            extract_ms,
            total_ms: download_ms + extract_ms,
        }),
    }
}

fn merge_html_filing_batch(
    batch: HtmlFilingBatch,
    combined: &mut HtmlExtractionResult,
    issues: &mut Vec<NormalizationIssue>,
    timings: &mut Vec<HtmlFilingTiming>,
) {
    combined.numeric_fallbacks.extend(batch.numeric_fallbacks);
    combined.narrative_sections.extend(batch.narrative_sections);
    issues.extend(batch.issues);
    if let Some(timing) = batch.timing {
        timings.push(timing);
    }
}

fn top_slowest_html_timings(mut timings: Vec<HtmlFilingTiming>) -> Vec<HtmlFilingTiming> {
    timings.sort_by(|left, right| right.total_ms.cmp(&left.total_ms));
    timings.truncate(5);
    timings
}

#[derive(Debug, Clone, PartialEq)]
struct DiscoveredFilingSet {
    company: CompanyIdentity,
    filings: Vec<FilingMetadata>,
    coverage: FilingHistoryCoverage,
}

#[cfg(test)]
mod tests {
    use super::*;
    use filing_models::{
        FilingForm, FilingSourceMethod, FilingUrls, MeasurementUnit, NumericValue, PeriodContext,
        Provenance, ReportingPeriod, SignConvention, SourceLocator, SourceType, ValueScale,
    };
    use html_extractor::ExtractedHtmlMetricValue;
    use std::time::{SystemTime, UNIX_EPOCH};
    use time::macros::date;
    use xbrl_extractor::ExtractedMetricValue;

    fn fixture_company() -> CompanyIdentity {
        CompanyIdentity {
            primary_id: CompanyId::Cik(Cik::new("798354")),
            ticker: Some(Ticker::new("FI")),
            cik: Some(Cik::new("798354")),
            issuer_name: "FISERV INC".to_string(),
            exchange: Some("NYSE".to_string()),
            reported_currency: Some("USD".to_string()),
            fiscal_year_end: Some("1231".to_string()),
        }
    }

    fn fixture_filing() -> FilingMetadata {
        FilingMetadata {
            accession_number: "0000798354-25-000010".to_string(),
            form_type: FilingForm::Form10K,
            filing_date: date!(2025 - 02 - 20),
            report_period_end: Some(date!(2024 - 12 - 31)),
            fiscal_period: None,
            filing_urls: FilingUrls {
                filing_detail: Some("https://example.test/index.html".to_string()),
                primary_document: Some("https://example.test/form10k.htm".to_string()),
                xbrl_instance: None,
                html_index: None,
            },
            source_types: vec![SourceType::Xbrl, SourceType::Html],
            is_amendment: false,
        }
    }

    fn numeric_value_with_period(context: PeriodContext) -> NumericValue {
        let reporting_period = ReportingPeriod { context, fiscal_period: None, label: None };

        NumericValue {
            amount: 100.0,
            unit: MeasurementUnit::Currency("USD".to_string()),
            scale: ValueScale::Raw,
            sign_convention: SignConvention::AsReported,
            label: Some("Revenue".to_string()),
            reporting_period: reporting_period.clone(),
            provenance: Provenance {
                accession_number: "0000798354-25-000010".to_string(),
                filing_url: Some("https://example.test/filing.htm".to_string()),
                form_type: FilingForm::Form10K,
                source_type: SourceType::Xbrl,
                source_method: FilingSourceMethod::ApiXbrlFacts,
                source_location: SourceLocator {
                    section_name: Some("test".to_string()),
                    table_name: Some("test".to_string()),
                    row_label: Some("Revenue".to_string()),
                    cell_reference: None,
                    segment_name: None,
                },
                xbrl_tag: Some("RevenueFromContractWithCustomerExcludingAssessedTax".to_string()),
                filing_label: Some("Revenue".to_string()),
                reporting_period,
                unit: MeasurementUnit::Currency("USD".to_string()),
                scale: ValueScale::Raw,
            },
        }
    }

    #[test]
    fn fixture_export_pipeline_builds_importable_workbook_without_live_sec() {
        let company_facts_payload =
            include_str!("../../../fixtures/0000798354/companyfacts_sample.json");
        let filing_html = include_str!("../../../fixtures/0000798354/filing_sample.html");
        let output = std::env::temp_dir().join(format!(
            "sec_edgar_fixture_fetch_export_{}.xlsx",
            SystemTime::now().duration_since(UNIX_EPOCH).expect("clock should be valid").as_nanos()
        ));

        let summary = export_fixture_dataset_to_path(
            fixture_company(),
            vec![fixture_filing()],
            company_facts_payload,
            vec![filing_html.to_string()],
            &output,
        )
        .expect("fixture pipeline should export workbook");

        assert!(summary.xbrl_metric_count > 0);
        assert!(summary.normalized_metric_count > 0);
        assert!(summary.review_issue_count > 0);

        let workbook_summary = WorkbookExporter::new()
            .import_summary(&output)
            .expect("fixture workbook should import");
        assert!(workbook_summary.sheet_names.contains(&"coverage_summary".to_string()));
        assert!(workbook_summary.sheet_names.contains(&"formula_inputs".to_string()));

        let _ = std::fs::remove_file(output);
    }

    #[test]
    fn analyst_review_flags_missing_critical_metrics() {
        let mut normalized = NormalizationResult::default();

        append_analyst_review_issues(&mut normalized);

        assert!(normalized.issues.iter().any(|issue| {
            issue.code == "analyst_critical_metric_missing"
                && issue.metric_id.as_ref().map(|id| id.as_str())
                    == Some("income_statement.revenue")
        }));
    }

    #[test]
    fn analyst_review_does_not_flag_inline_xbrl_backed_html_primary_as_html_only() {
        let reporting_period = ReportingPeriod {
            context: PeriodContext::Duration {
                start: date!(2024 - 04 - 01),
                end: date!(2024 - 06 - 30),
            },
            fiscal_period: None,
            label: None,
        };
        let mut normalized = NormalizationResult {
            numeric_metrics: vec![normalization::NormalizedNumericMetric {
                metric_id: MetricId::new("income_statement.revenue"),
                period_key: "2024-04-01_to_2024-06-30".to_string(),
                domain: accounting_domains::DomainName::IncomeStatement,
                metric_name: "Revenue".to_string(),
                subdomain: Some("operating_results".to_string()),
                value: NumericValue {
                    amount: 8080.0,
                    unit: MeasurementUnit::Currency("USD".to_string()),
                    scale: ValueScale::Millions,
                    sign_convention: SignConvention::AsReported,
                    label: Some("Revenue".to_string()),
                    reporting_period: reporting_period.clone(),
                    provenance: Provenance {
                        accession_number: "0000000000-24-000001".to_string(),
                        filing_url: Some("https://example.test/inline.htm".to_string()),
                        form_type: FilingForm::Form10Q,
                        source_type: SourceType::Xbrl,
                        source_method: FilingSourceMethod::FilingHtml,
                        source_location: SourceLocator {
                            section_name: Some("inline_xbrl_core".to_string()),
                            table_name: Some("inline_xbrl_core".to_string()),
                            row_label: Some("Revenue".to_string()),
                            cell_reference: Some("core_revenue_context".to_string()),
                            segment_name: None,
                        },
                        xbrl_tag: Some("us-gaap:Revenues".to_string()),
                        filing_label: Some("Revenue".to_string()),
                        reporting_period: reporting_period.clone(),
                        unit: MeasurementUnit::Currency("USD".to_string()),
                        scale: ValueScale::Millions,
                    },
                },
                primary_source: normalization::NormalizationSource::HtmlFallback,
                decision: normalization::NormalizationDecision::HtmlOnly,
                alternative_value: None,
                source_values: Vec::new(),
            }],
            ..NormalizationResult::default()
        };

        append_analyst_review_issues(&mut normalized);

        assert!(normalized.issues.iter().all(|issue| {
            !(issue.code == "analyst_critical_metric_html_only"
                && issue.metric_id.as_ref().map(|id| id.as_str())
                    == Some("income_statement.revenue"))
        }));
    }

    #[test]
    fn html_fallback_is_removed_when_xbrl_covers_same_metric_and_period_end() {
        let xbrl_metrics = vec![ExtractedMetricValue {
            metric_id: MetricId::new("income_statement.revenue"),
            metric_name: "Revenue".to_string(),
            domain: accounting_domains::DomainName::IncomeStatement,
            subdomain: Some("operating_results".to_string()),
            xbrl_tag: "RevenueFromContractWithCustomerExcludingAssessedTax".to_string(),
            numeric_value: numeric_value_with_period(PeriodContext::Duration {
                start: date!(2024 - 01 - 01),
                end: date!(2024 - 12 - 31),
            }),
        }];
        let mut html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![ExtractedHtmlMetricValue {
                metric_id: MetricId::new("income_statement.revenue"),
                metric_name: "Revenue".to_string(),
                domain: accounting_domains::DomainName::IncomeStatement,
                subdomain: Some("operating_results".to_string()),
                numeric_value: numeric_value_with_period(PeriodContext::Instant {
                    as_of: date!(2024 - 12 - 31),
                }),
            }],
            narrative_sections: Vec::new(),
        };

        keep_html_only_where_xbrl_is_missing(&xbrl_metrics, &mut html_result);

        assert!(html_result.numeric_fallbacks.is_empty());
    }

    #[test]
    fn html_historical_periods_survive_when_xbrl_only_covers_current_period() {
        let xbrl_metrics = vec![ExtractedMetricValue {
            metric_id: MetricId::new("income_statement.revenue"),
            metric_name: "Revenue".to_string(),
            domain: accounting_domains::DomainName::IncomeStatement,
            subdomain: Some("operating_results".to_string()),
            xbrl_tag: "RevenueFromContractWithCustomerExcludingAssessedTax".to_string(),
            numeric_value: numeric_value_with_period(PeriodContext::Duration {
                start: date!(2025 - 01 - 01),
                end: date!(2025 - 03 - 31),
            }),
        }];
        let mut html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("income_statement.revenue"),
                    metric_name: "Revenue".to_string(),
                    domain: accounting_domains::DomainName::IncomeStatement,
                    subdomain: Some("operating_results".to_string()),
                    numeric_value: numeric_value_with_period(PeriodContext::Duration {
                        start: date!(2025 - 01 - 01),
                        end: date!(2025 - 03 - 31),
                    }),
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("income_statement.revenue"),
                    metric_name: "Revenue".to_string(),
                    domain: accounting_domains::DomainName::IncomeStatement,
                    subdomain: Some("operating_results".to_string()),
                    numeric_value: numeric_value_with_period(PeriodContext::Duration {
                        start: date!(2024 - 01 - 01),
                        end: date!(2024 - 03 - 31),
                    }),
                },
            ],
            narrative_sections: Vec::new(),
        };

        keep_html_only_where_xbrl_is_missing(&xbrl_metrics, &mut html_result);

        assert_eq!(html_result.numeric_fallbacks.len(), 1);
        assert!(matches!(
            html_result.numeric_fallbacks[0].numeric_value.reporting_period.context,
            PeriodContext::Duration { start, end }
                if start == date!(2024 - 01 - 01) && end == date!(2024 - 03 - 31)
        ));
    }

    #[test]
    fn html_filing_timings_keep_slowest_five_in_descending_order() {
        let timings = top_slowest_html_timings(vec![
            HtmlFilingTiming {
                accession_number: "a".to_string(),
                form_type: "10-Q".to_string(),
                download_ms: 10,
                extract_ms: 10,
                total_ms: 20,
            },
            HtmlFilingTiming {
                accession_number: "b".to_string(),
                form_type: "10-Q".to_string(),
                download_ms: 5,
                extract_ms: 95,
                total_ms: 100,
            },
            HtmlFilingTiming {
                accession_number: "c".to_string(),
                form_type: "10-K".to_string(),
                download_ms: 10,
                extract_ms: 70,
                total_ms: 80,
            },
            HtmlFilingTiming {
                accession_number: "d".to_string(),
                form_type: "10-Q".to_string(),
                download_ms: 10,
                extract_ms: 50,
                total_ms: 60,
            },
            HtmlFilingTiming {
                accession_number: "e".to_string(),
                form_type: "10-K".to_string(),
                download_ms: 10,
                extract_ms: 40,
                total_ms: 50,
            },
            HtmlFilingTiming {
                accession_number: "f".to_string(),
                form_type: "10-Q".to_string(),
                download_ms: 10,
                extract_ms: 30,
                total_ms: 40,
            },
        ]);

        let accessions = timings
            .iter()
            .map(|timing| timing.accession_number.as_str())
            .collect::<Vec<_>>();

        assert_eq!(accessions, vec!["b", "c", "d", "e", "f"]);
    }

    #[test]
    fn html_duration_periods_with_same_end_but_different_start_survive_deduplication() {
        let xbrl_metrics = vec![ExtractedMetricValue {
            metric_id: MetricId::new("income_statement.revenue"),
            metric_name: "Revenue".to_string(),
            domain: accounting_domains::DomainName::IncomeStatement,
            subdomain: Some("operating_results".to_string()),
            xbrl_tag: "RevenueFromContractWithCustomerExcludingAssessedTax".to_string(),
            numeric_value: numeric_value_with_period(PeriodContext::Duration {
                start: date!(2025 - 04 - 01),
                end: date!(2025 - 06 - 30),
            }),
        }];
        let mut html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("income_statement.revenue"),
                    metric_name: "Revenue".to_string(),
                    domain: accounting_domains::DomainName::IncomeStatement,
                    subdomain: Some("operating_results".to_string()),
                    numeric_value: numeric_value_with_period(PeriodContext::Duration {
                        start: date!(2025 - 04 - 01),
                        end: date!(2025 - 06 - 30),
                    }),
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("income_statement.revenue"),
                    metric_name: "Revenue".to_string(),
                    domain: accounting_domains::DomainName::IncomeStatement,
                    subdomain: Some("operating_results".to_string()),
                    numeric_value: numeric_value_with_period(PeriodContext::Duration {
                        start: date!(2025 - 01 - 01),
                        end: date!(2025 - 06 - 30),
                    }),
                },
            ],
            narrative_sections: Vec::new(),
        };

        keep_html_only_where_xbrl_is_missing(&xbrl_metrics, &mut html_result);

        assert_eq!(html_result.numeric_fallbacks.len(), 1);
        assert!(matches!(
            html_result.numeric_fallbacks[0].numeric_value.reporting_period.context,
            PeriodContext::Duration { start, end }
                if start == date!(2025 - 01 - 01) && end == date!(2025 - 06 - 30)
        ));
    }

    #[tokio::test]
    #[ignore = "live SEC integration test; run explicitly with SEC_EDGAR_USER_AGENT configured"]
    async fn live_fetch_export_for_reference_cik() {
        let config = AppConfig::from_env();
        assert!(
            !config.sec.user_agent.contains("configure-before-production-use"),
            "set SEC_EDGAR_USER_AGENT before running live SEC tests"
        );

        let workflow = SecFetchExportWorkflow::from_config(&config)
            .expect("workflow should initialize from config");
        let output = std::env::temp_dir().join(format!(
            "sec_edgar_live_fetch_export_{}.xlsx",
            SystemTime::now().duration_since(UNIX_EPOCH).expect("clock should be valid").as_nanos()
        ));

        let summary = workflow
            .fetch_export_to_path(
                FetchExportRequest {
                    company_id: CompanyId::Cik(Cik::new("798354")),
                    years: 1,
                    include_html_fallback: false,
                },
                &output,
            )
            .await
            .expect("live SEC fetch/export should complete");

        assert!(!summary.selected_filings.is_empty());
        assert!(summary.xbrl_metric_count > 0);

        let _ = std::fs::remove_file(output);
    }
}
