//! Versioned `.xlsx` workbook export and import.
//!
//! The workbook format is intentionally explicit:
//!
//! - one worksheet per domain
//! - one row per metric
//! - historical periods across columns
//! - a `schema` worksheet used by import to validate compatibility

use accounting_domains::{DomainName, MetricRegistry};
use calamine::{Reader, open_workbook_auto};
use filing_models::{
    CompanyIdentity, FilingMetadata, MetricValue, NumericValue, PeriodContext, ReportingPeriod,
    TextBlock,
};
use normalization::{
    NormalizationDecision, NormalizationIssueSeverity, NormalizationResult, NormalizationSource,
    NormalizedNumericMetric, NormalizedSourceValue,
};
use rust_xlsxwriter::{Workbook, Worksheet, XlsxError};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use thiserror::Error;
use time::format_description::well_known::Iso8601;
use valuation::ValuationOutput;

pub const WORKBOOK_SCHEMA_VERSION: &str = "0.1.0";
const EXCEL_CELL_TEXT_LIMIT: usize = 32_767;

#[derive(Debug, Error)]
pub enum WorkbookIoError {
    #[error("workbook export failed: {0}")]
    Export(#[from] XlsxError),
    #[error("workbook import failed: {0}")]
    Import(#[from] calamine::Error),
    #[error("workbook schema sheet is missing")]
    MissingSchemaSheet,
    #[error("workbook schema version {found} is not supported; expected {expected}")]
    UnsupportedSchemaVersion { found: String, expected: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorksheetPlan {
    pub domain: DomainName,
    pub sheet_name: &'static str,
    pub description: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeriodColumn {
    pub column_key: String,
    pub column_label: String,
    pub reporting_period: ReportingPeriod,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MetricPeriodValue {
    pub column_key: String,
    pub value: MetricValue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkbookMetricRow {
    pub domain: DomainName,
    pub metric_id: String,
    pub metric_label: String,
    pub subdomain: Option<String>,
    pub values_by_period: Vec<MetricPeriodValue>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkbookSheetData {
    pub domain: DomainName,
    pub rows: Vec<WorkbookMetricRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkbookExportModel {
    pub schema_version: &'static str,
    pub company: CompanyIdentity,
    pub filing_index: Vec<WorkbookFilingRecord>,
    pub period_columns: Vec<PeriodColumn>,
    pub sheets: Vec<WorkbookSheetData>,
    pub provenance_records: Vec<WorkbookProvenanceRecord>,
    pub duplicate_candidate_records: Vec<WorkbookDuplicateCandidateRecord>,
    pub review_issues: Vec<WorkbookReviewIssue>,
    pub coverage_records: Vec<WorkbookCoverageRecord>,
    pub formula_input_records: Vec<WorkbookFormulaInputRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkbookFilingRecord {
    pub accession_number: String,
    pub form_type: String,
    pub filing_date: String,
    pub report_period_end: Option<String>,
    pub primary_document_url: Option<String>,
    pub filing_index_url: Option<String>,
    pub is_amendment: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkbookProvenanceRecord {
    pub metric_id: String,
    pub metric_label: String,
    pub domain: DomainName,
    pub period_key: String,
    pub segment_name: Option<String>,
    pub selected_as_primary: bool,
    pub source: NormalizationSource,
    pub decision: NormalizationDecision,
    pub value: NumericValue,
    pub review_note: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkbookDuplicateCandidateRecord {
    pub metric_id: String,
    pub metric_label: String,
    pub domain: DomainName,
    pub period_key: String,
    pub segment_name: Option<String>,
    pub accession_number: String,
    pub form_type: String,
    pub filing_url: Option<String>,
    pub filing_label: Option<String>,
    pub section_name: Option<String>,
    pub table_name: Option<String>,
    pub row_label: Option<String>,
    pub amount: f64,
    pub unit: String,
    pub source_method: String,
    pub review_note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkbookReviewIssue {
    pub severity: NormalizationIssueSeverity,
    pub code: &'static str,
    pub metric_id: Option<String>,
    pub period_key: Option<String>,
    pub segment_name: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkbookCoverageRecord {
    pub domain: DomainName,
    pub metric_id: String,
    pub metric_label: String,
    pub expected_source: String,
    pub found_primary_values: usize,
    pub found_source_values: usize,
    pub xbrl_source_values: usize,
    pub html_source_values: usize,
    pub period_count: usize,
    pub status: String,
    pub status_note: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkbookFormulaInputRecord {
    pub formula_metric_id: String,
    pub formula_name: String,
    pub formula_period_key: String,
    pub input_metric_id: String,
    pub input_metric_name: String,
    pub input_amount: f64,
    pub input_period_key: String,
    pub input_accession_number: String,
    pub input_source_type: String,
    pub input_source_method: String,
    pub input_xbrl_tag: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkbookImportSummary {
    pub schema_version: String,
    pub sheet_names: Vec<String>,
}

#[derive(Debug, Default)]
pub struct WorkbookExporter;

impl WorkbookExporter {
    pub fn new() -> Self {
        Self
    }

    pub fn build_model(
        &self,
        company: CompanyIdentity,
        normalized: &NormalizationResult,
        valuation_outputs: &[ValuationOutput],
    ) -> WorkbookExportModel {
        self.build_model_with_filings(company, &[], normalized, valuation_outputs)
    }

    pub fn build_model_with_filings(
        &self,
        company: CompanyIdentity,
        filings: &[FilingMetadata],
        normalized: &NormalizationResult,
        valuation_outputs: &[ValuationOutput],
    ) -> WorkbookExportModel {
        let mut period_map: BTreeMap<String, ReportingPeriod> = BTreeMap::new();
        let mut provenance_records = Vec::new();

        for metric in &normalized.numeric_metrics {
            period_map.insert(
                period_key(&metric.value.reporting_period),
                metric.value.reporting_period.clone(),
            );

            for source_value in &metric.source_values {
                period_map.insert(
                    period_key(&source_value.value.reporting_period),
                    source_value.value.reporting_period.clone(),
                );
                provenance_records.push(provenance_record(metric, source_value));
            }
        }

        for metric in &normalized.narrative_metrics {
            let reporting_period = ReportingPeriod {
                context: PeriodContext::Instant { as_of: metric.value.filing_date },
                fiscal_period: None,
                label: Some(metric.value.form_type.as_str().to_string()),
            };
            period_map.insert(period_key(&reporting_period), reporting_period);
        }

        for output in valuation_outputs {
            period_map.insert(
                period_key(&output.value.reporting_period),
                output.value.reporting_period.clone(),
            );
        }

        let coverage_records =
            build_coverage_records(normalized, valuation_outputs, &provenance_records);
        let duplicate_candidate_records = build_duplicate_candidate_records(&provenance_records);
        let formula_input_records = build_formula_input_records(valuation_outputs);
        let period_columns = build_period_columns(period_map);

        let mut rows_by_domain: BTreeMap<DomainName, BTreeMap<String, WorkbookMetricRow>> =
            BTreeMap::new();

        for metric in &normalized.numeric_metrics {
            push_numeric_row(
                &mut rows_by_domain,
                metric.domain,
                metric.metric_id.as_str(),
                &metric.metric_name,
                metric.subdomain.clone(),
                &metric.value,
            );
        }

        for metric in &normalized.narrative_metrics {
            push_text_row(
                &mut rows_by_domain,
                metric.domain,
                metric.metric_id.as_str(),
                &metric.value.title,
                None,
                &metric.value,
            );
        }

        for output in valuation_outputs {
            push_numeric_row(
                &mut rows_by_domain,
                output.domain,
                output.metric_id.as_str(),
                &output.metric_name,
                Some("placeholder_formula".to_string()),
                &output.value,
            );
        }

        let sheets = rows_by_domain
            .into_iter()
            .map(|(domain, rows)| WorkbookSheetData { domain, rows: rows.into_values().collect() })
            .collect();

        WorkbookExportModel {
            schema_version: WORKBOOK_SCHEMA_VERSION,
            company,
            filing_index: filings.iter().map(filing_record).collect(),
            period_columns,
            sheets,
            provenance_records,
            duplicate_candidate_records,
            review_issues: normalized
                .issues
                .iter()
                .map(|issue| WorkbookReviewIssue {
                    severity: issue.severity,
                    code: issue.code,
                    metric_id: issue
                        .metric_id
                        .as_ref()
                        .map(|metric_id| metric_id.as_str().to_string()),
                    period_key: issue.period_key.clone(),
                    segment_name: issue.segment_name.clone(),
                    message: issue.message.clone(),
                })
                .collect(),
            coverage_records,
            formula_input_records,
        }
    }

    pub fn export_to_path(
        &self,
        model: &WorkbookExportModel,
        path: impl AsRef<Path>,
    ) -> Result<(), WorkbookIoError> {
        let mut workbook = Workbook::new();

        write_schema_sheet(&mut workbook, model)?;
        write_company_overview_sheet(&mut workbook, model)?;
        write_filing_index_sheet(&mut workbook, model)?;

        for plan in default_worksheet_plan() {
            if matches!(
                plan.domain,
                DomainName::Schema
                    | DomainName::CompanyOverview
                    | DomainName::FilingIndex
                    | DomainName::Provenance
            ) {
                continue;
            }

            let sheet_data = model.sheets.iter().find(|sheet| sheet.domain == plan.domain);
            write_domain_sheet(&mut workbook, &plan, model, sheet_data)?;
        }

        write_provenance_sheet(&mut workbook, model)?;
        write_duplicate_candidates_sheet(&mut workbook, model)?;
        write_review_warnings_sheet(&mut workbook, model)?;
        write_coverage_summary_sheet(&mut workbook, model)?;
        write_missing_metrics_sheet(&mut workbook, model)?;
        write_formula_inputs_sheet(&mut workbook, model)?;

        workbook.save(path)?;
        Ok(())
    }

    pub fn import_summary(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<WorkbookImportSummary, WorkbookIoError> {
        let mut workbook = open_workbook_auto(path)?;
        let sheet_names = workbook.sheet_names().to_vec();
        let schema_range = workbook
            .worksheet_range(DomainName::Schema.sheet_name())
            .map_err(|_| WorkbookIoError::MissingSchemaSheet)?;

        let schema_version = schema_range
            .rows()
            .find_map(|row| {
                let key = row.first().map(|cell| cell.to_string()).unwrap_or_default();
                let value = row.get(1).map(|cell| cell.to_string()).unwrap_or_default();
                (key == "schema_version").then_some(value)
            })
            .ok_or(WorkbookIoError::MissingSchemaSheet)?;

        if schema_version != WORKBOOK_SCHEMA_VERSION {
            return Err(WorkbookIoError::UnsupportedSchemaVersion {
                found: schema_version,
                expected: WORKBOOK_SCHEMA_VERSION.to_string(),
            });
        }

        Ok(WorkbookImportSummary { schema_version, sheet_names })
    }
}

fn build_period_columns(period_map: BTreeMap<String, ReportingPeriod>) -> Vec<PeriodColumn> {
    let mut period_columns: Vec<PeriodColumn> = period_map
        .into_iter()
        .map(|(column_key, reporting_period)| PeriodColumn {
            column_label: period_label(&reporting_period),
            column_key,
            reporting_period,
        })
        .collect();

    let mut label_counts: HashMap<String, usize> = HashMap::new();
    for period_column in &period_columns {
        *label_counts.entry(period_column.column_label.clone()).or_default() += 1;
    }

    for period_column in &mut period_columns {
        if label_counts.get(&period_column.column_label).copied().unwrap_or_default() > 1 {
            period_column.column_label =
                format!("{} ({})", period_column.column_label, period_column.column_key);
        }
    }

    period_columns
}

fn build_coverage_records(
    normalized: &NormalizationResult,
    valuation_outputs: &[ValuationOutput],
    provenance_records: &[WorkbookProvenanceRecord],
) -> Vec<WorkbookCoverageRecord> {
    let registry = MetricRegistry::default();
    let mut records = Vec::new();
    let computed_formula_metric_ids: BTreeSet<String> =
        valuation_outputs.iter().map(|output| output.metric_id.as_str().to_string()).collect();

    for metric in registry.all() {
        if metric.definition.domain == DomainName::Valuation
            && computed_formula_metric_ids.contains(metric.definition.metric_id.as_str())
        {
            continue;
        }

        let metric_id = metric.definition.metric_id.as_str();
        let primary_values: Vec<&NormalizedNumericMetric> = normalized
            .numeric_metrics
            .iter()
            .filter(|value| value.metric_id.as_str() == metric_id)
            .collect();
        let source_values: Vec<&WorkbookProvenanceRecord> =
            provenance_records.iter().filter(|value| value.metric_id == metric_id).collect();
        let periods: BTreeSet<&str> =
            primary_values.iter().map(|value| value.period_key.as_str()).collect();

        let xbrl_source_values = source_values
            .iter()
            .filter(|record| record.value.provenance.source_type == filing_models::SourceType::Xbrl)
            .count();
        let html_source_values = source_values
            .iter()
            .filter(|record| record.value.provenance.source_type == filing_models::SourceType::Html)
            .count();

        records.push(WorkbookCoverageRecord {
            domain: metric.definition.domain,
            metric_id: metric_id.to_string(),
            metric_label: metric.definition.display_name.clone(),
            expected_source: expected_source_label(metric.definition.domain).to_string(),
            found_primary_values: primary_values.len(),
            found_source_values: source_values.len(),
            xbrl_source_values,
            html_source_values,
            period_count: periods.len(),
            status: coverage_status_for_registry_metric(metric, !primary_values.is_empty()).0,
            status_note: coverage_status_for_registry_metric(metric, !primary_values.is_empty()).1,
        });
    }

    for output in valuation_outputs {
        records.push(WorkbookCoverageRecord {
            domain: output.domain,
            metric_id: output.metric_id.as_str().to_string(),
            metric_label: output.metric_name.clone(),
            expected_source: "valuation_formula".to_string(),
            found_primary_values: 1,
            found_source_values: output.inputs.len(),
            xbrl_source_values: output
                .inputs
                .iter()
                .filter(|input| input.provenance.source_type == filing_models::SourceType::Xbrl)
                .count(),
            html_source_values: output
                .inputs
                .iter()
                .filter(|input| input.provenance.source_type == filing_models::SourceType::Html)
                .count(),
            period_count: 1,
            status: "computed_placeholder".to_string(),
            status_note: "Placeholder valuation formula produced an output. Replace the formula implementation later if you need analyst-specific valuation logic.".to_string(),
        });
    }

    if valuation_outputs.is_empty()
        && normalized.issues.iter().any(|issue| issue.code == "valuation_placeholder_skipped")
    {
        let registry = MetricRegistry::default();
        for metric_id in [
            "valuation.owners_earnings_placeholder",
            "valuation.adjusted_earnings_ratio_placeholder",
        ] {
            if records.iter().any(|record| record.metric_id == metric_id) {
                continue;
            }

            if let Some(metric) = registry.by_id(metric_id) {
                records.push(WorkbookCoverageRecord {
                    domain: metric.definition.domain,
                    metric_id: metric.definition.metric_id.as_str().to_string(),
                    metric_label: metric.definition.display_name.clone(),
                    expected_source: "valuation_formula".to_string(),
                    found_primary_values: 0,
                    found_source_values: 0,
                    xbrl_source_values: 0,
                    html_source_values: 0,
                    period_count: 0,
                    status: "formula_not_computed".to_string(),
                    status_note: "Placeholder valuation formula was not computed because required normalized inputs were unavailable. Review formula_inputs, provenance, and critical metric coverage before replacing the placeholder formula.".to_string(),
                });
            }
        }
    }

    records
}

fn coverage_status_for_registry_metric(
    metric: &accounting_domains::DomainMetric,
    found_primary_value: bool,
) -> (String, String) {
    if found_primary_value {
        return (
            "found".to_string(),
            "Metric has at least one primary extracted value.".to_string(),
        );
    }

    match metric.definition.domain {
        DomainName::RiskFactorsSkeleton => (
            "placeholder_not_extracted".to_string(),
            "Risk factor extraction is intentionally deferred. Add narrative extraction later once the numeric SEC workflow is stable.".to_string(),
        ),
        DomainName::Valuation => (
            "formula_not_computed".to_string(),
            "Valuation rows are placeholders. Add or replace the formula implementation when you finalize the analyst valuation workflow.".to_string(),
        ),
        DomainName::Footnotes | DomainName::Mda => (
            "narrative_placeholder".to_string(),
            "Narrative/text extraction for this metric is planned but not yet implemented in a domain-specific way. Extend the HTML narrative parser and keep provenance attached.".to_string(),
        ),
        _ => (
            "missing_extraction".to_string(),
            "No primary value was extracted from XBRL or conservative HTML fallback. This is a real extraction gap to implement or map later.".to_string(),
        ),
    }
}

fn build_formula_input_records(
    valuation_outputs: &[ValuationOutput],
) -> Vec<WorkbookFormulaInputRecord> {
    let mut records = Vec::new();

    for output in valuation_outputs {
        let formula_period_key = period_key(&output.value.reporting_period);
        for input in &output.inputs {
            records.push(WorkbookFormulaInputRecord {
                formula_metric_id: output.metric_id.as_str().to_string(),
                formula_name: output.metric_name.clone(),
                formula_period_key: formula_period_key.clone(),
                input_metric_id: input.metric_id.as_str().to_string(),
                input_metric_name: input.metric_name.clone(),
                input_amount: input.amount,
                input_period_key: period_key(&input.provenance.reporting_period),
                input_accession_number: input.provenance.accession_number.clone(),
                input_source_type: format!("{:?}", input.provenance.source_type),
                input_source_method: format!("{:?}", input.provenance.source_method),
                input_xbrl_tag: input.provenance.xbrl_tag.clone(),
            });
        }
    }

    records
}

fn build_duplicate_candidate_records(
    provenance_records: &[WorkbookProvenanceRecord],
) -> Vec<WorkbookDuplicateCandidateRecord> {
    let mut records = provenance_records
        .iter()
        .filter(|record| !record.selected_as_primary)
        .filter(|record| record.review_note.is_some())
        .map(|record| {
            let provenance = &record.value.provenance;
            WorkbookDuplicateCandidateRecord {
                metric_id: record.metric_id.clone(),
                metric_label: record.metric_label.clone(),
                domain: record.domain,
                period_key: record.period_key.clone(),
                segment_name: record.segment_name.clone(),
                accession_number: provenance.accession_number.clone(),
                form_type: provenance.form_type.as_str().to_string(),
                filing_url: provenance.filing_url.clone(),
                filing_label: provenance.filing_label.clone(),
                section_name: provenance.source_location.section_name.clone(),
                table_name: provenance.source_location.table_name.clone(),
                row_label: provenance.source_location.row_label.clone(),
                amount: record.value.amount,
                unit: format!("{:?}", record.value.unit),
                source_method: format!("{:?}", provenance.source_method),
                review_note: record.review_note.clone(),
            }
        })
        .collect::<Vec<_>>();

    records.sort_by(|left, right| {
        left.metric_id
            .cmp(&right.metric_id)
            .then_with(|| left.period_key.cmp(&right.period_key))
            .then_with(|| left.accession_number.cmp(&right.accession_number))
            .then_with(|| left.amount.total_cmp(&right.amount))
    });

    records
}

fn expected_source_label(domain: DomainName) -> &'static str {
    match domain {
        DomainName::Footnotes | DomainName::Mda | DomainName::RiskFactorsSkeleton => {
            "html_narrative"
        }
        DomainName::Valuation => "valuation_formula",
        _ => "xbrl_primary_html_fallback",
    }
}

fn filing_record(filing: &FilingMetadata) -> WorkbookFilingRecord {
    WorkbookFilingRecord {
        accession_number: filing.accession_number.clone(),
        form_type: filing.form_type.as_str().to_string(),
        filing_date: filing.filing_date.format(&Iso8601::DATE).unwrap_or_default(),
        report_period_end: filing
            .report_period_end
            .and_then(|date| date.format(&Iso8601::DATE).ok()),
        primary_document_url: filing.filing_urls.primary_document.clone(),
        filing_index_url: filing.filing_urls.html_index.clone(),
        is_amendment: filing.is_amendment,
    }
}

fn provenance_record(
    metric: &NormalizedNumericMetric,
    source_value: &NormalizedSourceValue,
) -> WorkbookProvenanceRecord {
    WorkbookProvenanceRecord {
        metric_id: metric.metric_id.as_str().to_string(),
        metric_label: metric.metric_name.clone(),
        domain: metric.domain,
        period_key: metric.period_key.clone(),
        segment_name: source_value.value.provenance.source_location.segment_name.clone(),
        selected_as_primary: source_value.selected_as_primary,
        source: source_value.source,
        decision: metric.decision,
        value: source_value.value.clone(),
        review_note: source_value.review_note.clone(),
    }
}

fn push_numeric_row(
    rows_by_domain: &mut BTreeMap<DomainName, BTreeMap<String, WorkbookMetricRow>>,
    domain: DomainName,
    metric_id: &str,
    metric_label: &str,
    subdomain: Option<String>,
    value: &NumericValue,
) {
    let rows = rows_by_domain.entry(domain).or_default();
    let (row_metric_id, row_metric_label) =
        workbook_row_identity(domain, metric_id, metric_label, value);
    let row = rows.entry(row_metric_id.clone()).or_insert_with(|| WorkbookMetricRow {
        domain,
        metric_id: row_metric_id,
        metric_label: row_metric_label,
        subdomain,
        values_by_period: Vec::new(),
    });

    row.values_by_period.push(MetricPeriodValue {
        column_key: period_key(&value.reporting_period),
        value: MetricValue::Numeric(value.clone()),
    });
}

fn workbook_row_identity(
    domain: DomainName,
    metric_id: &str,
    metric_label: &str,
    value: &NumericValue,
) -> (String, String) {
    if domain == DomainName::SegmentData {
        if let Some(segment_name) = value.provenance.source_location.segment_name.as_deref() {
            let segment_key = sanitize_row_key_component(segment_name);
            return (
                format!("{metric_id}::{segment_key}"),
                format!("{segment_name} | {metric_label}"),
            );
        }
    }

    (metric_id.to_string(), metric_label.to_string())
}

fn sanitize_row_key_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch.to_ascii_lowercase() } else { '_' })
        .collect()
}

fn push_text_row(
    rows_by_domain: &mut BTreeMap<DomainName, BTreeMap<String, WorkbookMetricRow>>,
    domain: DomainName,
    metric_id: &str,
    metric_label: &str,
    subdomain: Option<String>,
    value: &TextBlock,
) {
    let reporting_period = ReportingPeriod {
        context: PeriodContext::Instant { as_of: value.filing_date },
        fiscal_period: None,
        label: Some(value.form_type.as_str().to_string()),
    };

    let rows = rows_by_domain.entry(domain).or_default();
    let row = rows.entry(metric_id.to_string()).or_insert_with(|| WorkbookMetricRow {
        domain,
        metric_id: metric_id.to_string(),
        metric_label: metric_label.to_string(),
        subdomain,
        values_by_period: Vec::new(),
    });

    row.values_by_period.push(MetricPeriodValue {
        column_key: period_key(&reporting_period),
        value: MetricValue::Text(value.clone()),
    });
}

fn write_schema_sheet(
    workbook: &mut Workbook,
    model: &WorkbookExportModel,
) -> Result<(), WorkbookIoError> {
    let worksheet = workbook.add_worksheet();
    worksheet.set_name(DomainName::Schema.sheet_name())?;
    worksheet.write_string(0, 0, "schema_version")?;
    worksheet.write_string(0, 1, model.schema_version)?;
    worksheet.write_string(1, 0, "period_column_count")?;
    worksheet.write_number(1, 1, model.period_columns.len() as f64)?;
    worksheet.write_string(2, 0, "layout")?;
    worksheet.write_string(2, 1, "one_row_per_metric_periods_as_columns")?;
    Ok(())
}

fn write_company_overview_sheet(
    workbook: &mut Workbook,
    model: &WorkbookExportModel,
) -> Result<(), WorkbookIoError> {
    let worksheet = workbook.add_worksheet();
    worksheet.set_name(DomainName::CompanyOverview.sheet_name())?;
    worksheet.write_string(0, 0, "field")?;
    worksheet.write_string(0, 1, "value")?;
    worksheet.write_string(1, 0, "issuer_name")?;
    worksheet.write_string(1, 1, &model.company.issuer_name)?;
    worksheet.write_string(2, 0, "primary_id")?;
    worksheet.write_string(2, 1, model.company.primary_id.to_string())?;

    if let Some(ticker) = &model.company.ticker {
        worksheet.write_string(3, 0, "ticker")?;
        worksheet.write_string(3, 1, ticker.as_str())?;
    }

    if let Some(cik) = &model.company.cik {
        worksheet.write_string(4, 0, "cik")?;
        worksheet.write_string(4, 1, cik.as_str())?;
    }

    Ok(())
}

fn write_filing_index_sheet(
    workbook: &mut Workbook,
    model: &WorkbookExportModel,
) -> Result<(), WorkbookIoError> {
    let worksheet = workbook.add_worksheet();
    worksheet.set_name(DomainName::FilingIndex.sheet_name())?;

    let headers = [
        "accession_number",
        "form_type",
        "filing_date",
        "report_period_end",
        "primary_document_url",
        "filing_index_url",
        "is_amendment",
    ];

    for (column, header) in headers.iter().enumerate() {
        worksheet.write_string(0, column as u16, *header)?;
    }

    for (index, filing) in model.filing_index.iter().enumerate() {
        let row = (index + 1) as u32;
        worksheet.write_string(row, 0, &filing.accession_number)?;
        worksheet.write_string(row, 1, &filing.form_type)?;
        worksheet.write_string(row, 2, &filing.filing_date)?;
        worksheet.write_string(row, 3, filing.report_period_end.as_deref().unwrap_or_default())?;
        worksheet.write_string(
            row,
            4,
            filing.primary_document_url.as_deref().unwrap_or_default(),
        )?;
        worksheet.write_string(row, 5, filing.filing_index_url.as_deref().unwrap_or_default())?;
        worksheet.write_string(row, 6, bool_label(filing.is_amendment))?;
    }

    Ok(())
}

fn write_provenance_sheet(
    workbook: &mut Workbook,
    model: &WorkbookExportModel,
) -> Result<(), WorkbookIoError> {
    let worksheet = workbook.add_worksheet();
    worksheet.set_name(DomainName::Provenance.sheet_name())?;

    let headers = [
        "metric_id",
        "metric_label",
        "domain",
        "period_key",
        "segment_name",
        "selected_as_primary",
        "normalization_source",
        "normalization_decision",
        "amount",
        "unit",
        "accession_number",
        "form_type",
        "source_method",
        "xbrl_tag",
        "filing_url",
        "filing_label",
        "row_label",
        "review_note",
    ];

    for (column, header) in headers.iter().enumerate() {
        worksheet.write_string(0, column as u16, *header)?;
    }

    for (index, record) in model.provenance_records.iter().enumerate() {
        let row = (index + 1) as u32;
        let provenance = &record.value.provenance;

        worksheet.write_string(row, 0, &record.metric_id)?;
        worksheet.write_string(row, 1, &record.metric_label)?;
        worksheet.write_string(row, 2, record.domain.sheet_name())?;
        worksheet.write_string(row, 3, &record.period_key)?;
        worksheet.write_string(row, 4, record.segment_name.as_deref().unwrap_or_default())?;
        worksheet.write_string(row, 5, bool_label(record.selected_as_primary))?;
        worksheet.write_string(row, 6, normalization_source_label(record.source))?;
        worksheet.write_string(row, 7, normalization_decision_label(record.decision))?;
        worksheet.write_number(row, 8, record.value.amount)?;
        worksheet.write_string(row, 9, format!("{:?}", record.value.unit))?;
        worksheet.write_string(row, 10, &provenance.accession_number)?;
        worksheet.write_string(row, 11, provenance.form_type.as_str())?;
        worksheet.write_string(row, 12, format!("{:?}", provenance.source_method))?;
        worksheet.write_string(row, 13, provenance.xbrl_tag.as_deref().unwrap_or_default())?;
        worksheet.write_string(row, 14, provenance.filing_url.as_deref().unwrap_or_default())?;
        worksheet.write_string(row, 15, provenance.filing_label.as_deref().unwrap_or_default())?;
        worksheet.write_string(
            row,
            16,
            provenance.source_location.row_label.as_deref().unwrap_or_default(),
        )?;
        worksheet.write_string(row, 17, record.review_note.as_deref().unwrap_or_default())?;
    }

    Ok(())
}

fn write_duplicate_candidates_sheet(
    workbook: &mut Workbook,
    model: &WorkbookExportModel,
) -> Result<(), WorkbookIoError> {
    let worksheet = workbook.add_worksheet();
    worksheet.set_name("duplicate_candidates")?;

    let headers = [
        "metric_id",
        "metric_label",
        "domain",
        "period_key",
        "segment_name",
        "accession_number",
        "form_type",
        "filing_label",
        "section_name",
        "table_name",
        "row_label",
        "amount",
        "unit",
        "source_method",
        "filing_url",
        "review_note",
    ];

    for (column, header) in headers.iter().enumerate() {
        worksheet.write_string(0, column as u16, *header)?;
    }

    for (index, record) in model.duplicate_candidate_records.iter().enumerate() {
        let row = (index + 1) as u32;
        worksheet.write_string(row, 0, &record.metric_id)?;
        worksheet.write_string(row, 1, &record.metric_label)?;
        worksheet.write_string(row, 2, record.domain.sheet_name())?;
        worksheet.write_string(row, 3, &record.period_key)?;
        worksheet.write_string(row, 4, record.segment_name.as_deref().unwrap_or_default())?;
        worksheet.write_string(row, 5, &record.accession_number)?;
        worksheet.write_string(row, 6, &record.form_type)?;
        worksheet.write_string(row, 7, record.filing_label.as_deref().unwrap_or_default())?;
        worksheet.write_string(row, 8, record.section_name.as_deref().unwrap_or_default())?;
        worksheet.write_string(row, 9, record.table_name.as_deref().unwrap_or_default())?;
        worksheet.write_string(row, 10, record.row_label.as_deref().unwrap_or_default())?;
        worksheet.write_number(row, 11, record.amount)?;
        worksheet.write_string(row, 12, &record.unit)?;
        worksheet.write_string(row, 13, &record.source_method)?;
        worksheet.write_string(row, 14, record.filing_url.as_deref().unwrap_or_default())?;
        worksheet.write_string(row, 15, record.review_note.as_deref().unwrap_or_default())?;
    }

    Ok(())
}

fn write_review_warnings_sheet(
    workbook: &mut Workbook,
    model: &WorkbookExportModel,
) -> Result<(), WorkbookIoError> {
    let worksheet = workbook.add_worksheet();
    worksheet.set_name("review_warnings")?;

    worksheet.write_string(0, 0, "severity")?;
    worksheet.write_string(0, 1, "code")?;
    worksheet.write_string(0, 2, "metric_id")?;
    worksheet.write_string(0, 3, "period_key")?;
    worksheet.write_string(0, 4, "segment_name")?;
    worksheet.write_string(0, 5, "message")?;

    for (index, issue) in model.review_issues.iter().enumerate() {
        let row = (index + 1) as u32;
        worksheet.write_string(row, 0, issue_severity_label(issue.severity))?;
        worksheet.write_string(row, 1, issue.code)?;
        worksheet.write_string(row, 2, issue.metric_id.as_deref().unwrap_or_default())?;
        worksheet.write_string(row, 3, issue.period_key.as_deref().unwrap_or_default())?;
        worksheet.write_string(row, 4, issue.segment_name.as_deref().unwrap_or_default())?;
        worksheet.write_string(row, 5, &issue.message)?;
    }

    Ok(())
}

fn write_coverage_summary_sheet(
    workbook: &mut Workbook,
    model: &WorkbookExportModel,
) -> Result<(), WorkbookIoError> {
    let worksheet = workbook.add_worksheet();
    worksheet.set_name("coverage_summary")?;

    let headers = [
        "domain",
        "metric_id",
        "metric_label",
        "expected_source",
        "status",
        "status_note",
        "found_primary_values",
        "found_source_values",
        "xbrl_source_values",
        "html_source_values",
        "period_count",
    ];

    for (column, header) in headers.iter().enumerate() {
        worksheet.write_string(0, column as u16, *header)?;
    }

    for (index, record) in model.coverage_records.iter().enumerate() {
        let row = (index + 1) as u32;
        worksheet.write_string(row, 0, record.domain.sheet_name())?;
        worksheet.write_string(row, 1, &record.metric_id)?;
        worksheet.write_string(row, 2, &record.metric_label)?;
        worksheet.write_string(row, 3, &record.expected_source)?;
        worksheet.write_string(row, 4, &record.status)?;
        worksheet.write_string(row, 5, truncate_for_excel_cell(&record.status_note))?;
        worksheet.write_number(row, 6, record.found_primary_values as f64)?;
        worksheet.write_number(row, 7, record.found_source_values as f64)?;
        worksheet.write_number(row, 8, record.xbrl_source_values as f64)?;
        worksheet.write_number(row, 9, record.html_source_values as f64)?;
        worksheet.write_number(row, 10, record.period_count as f64)?;
    }

    Ok(())
}

fn write_missing_metrics_sheet(
    workbook: &mut Workbook,
    model: &WorkbookExportModel,
) -> Result<(), WorkbookIoError> {
    let worksheet = workbook.add_worksheet();
    worksheet.set_name("missing_metrics")?;

    let headers =
        ["domain", "metric_id", "metric_label", "expected_source", "status", "status_note"];
    for (column, header) in headers.iter().enumerate() {
        worksheet.write_string(0, column as u16, *header)?;
    }

    for (index, record) in model
        .coverage_records
        .iter()
        .filter(|record| record.status != "found" && record.status != "computed_placeholder")
        .enumerate()
    {
        let row = (index + 1) as u32;
        worksheet.write_string(row, 0, record.domain.sheet_name())?;
        worksheet.write_string(row, 1, &record.metric_id)?;
        worksheet.write_string(row, 2, &record.metric_label)?;
        worksheet.write_string(row, 3, &record.expected_source)?;
        worksheet.write_string(row, 4, &record.status)?;
        worksheet.write_string(row, 5, truncate_for_excel_cell(&record.status_note))?;
    }

    Ok(())
}

fn write_formula_inputs_sheet(
    workbook: &mut Workbook,
    model: &WorkbookExportModel,
) -> Result<(), WorkbookIoError> {
    let worksheet = workbook.add_worksheet();
    worksheet.set_name("formula_inputs")?;

    let headers = [
        "formula_metric_id",
        "formula_name",
        "formula_period_key",
        "input_metric_id",
        "input_metric_name",
        "input_amount",
        "input_period_key",
        "input_accession_number",
        "input_source_type",
        "input_source_method",
        "input_xbrl_tag",
    ];

    for (column, header) in headers.iter().enumerate() {
        worksheet.write_string(0, column as u16, *header)?;
    }

    for (index, record) in model.formula_input_records.iter().enumerate() {
        let row = (index + 1) as u32;
        worksheet.write_string(row, 0, &record.formula_metric_id)?;
        worksheet.write_string(row, 1, &record.formula_name)?;
        worksheet.write_string(row, 2, &record.formula_period_key)?;
        worksheet.write_string(row, 3, &record.input_metric_id)?;
        worksheet.write_string(row, 4, &record.input_metric_name)?;
        worksheet.write_number(row, 5, record.input_amount)?;
        worksheet.write_string(row, 6, &record.input_period_key)?;
        worksheet.write_string(row, 7, &record.input_accession_number)?;
        worksheet.write_string(row, 8, &record.input_source_type)?;
        worksheet.write_string(row, 9, &record.input_source_method)?;
        worksheet.write_string(row, 10, record.input_xbrl_tag.as_deref().unwrap_or_default())?;
    }

    Ok(())
}

fn write_domain_sheet(
    workbook: &mut Workbook,
    plan: &WorksheetPlan,
    model: &WorkbookExportModel,
    sheet_data: Option<&WorkbookSheetData>,
) -> Result<(), WorkbookIoError> {
    let worksheet = workbook.add_worksheet();
    worksheet.set_name(plan.sheet_name)?;
    write_domain_headers(worksheet, model)?;

    if let Some(sheet_data) = sheet_data {
        for (row_index, row) in sheet_data.rows.iter().enumerate() {
            let row_number = (row_index + 1) as u32;
            worksheet.write_string(row_number, 0, &row.metric_id)?;
            worksheet.write_string(row_number, 1, &row.metric_label)?;
            worksheet.write_string(row_number, 2, row.subdomain.as_deref().unwrap_or_default())?;

            let values_by_period: BTreeMap<&str, &MetricValue> = row
                .values_by_period
                .iter()
                .map(|value| (value.column_key.as_str(), &value.value))
                .collect();

            for (period_index, period_column) in model.period_columns.iter().enumerate() {
                let column_number = (period_index + 3) as u16;
                if let Some(value) = values_by_period.get(period_column.column_key.as_str()) {
                    write_metric_value(worksheet, row_number, column_number, value)?;
                }
            }
        }
    }

    Ok(())
}

fn bool_label(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

fn normalization_source_label(source: NormalizationSource) -> &'static str {
    match source {
        NormalizationSource::XbrlPrimary => "xbrl",
        NormalizationSource::HtmlFallback => "html",
        NormalizationSource::NarrativeHtml => "narrative_html",
    }
}

fn normalization_decision_label(decision: NormalizationDecision) -> &'static str {
    match decision {
        NormalizationDecision::XbrlOnly => "xbrl_only",
        NormalizationDecision::HtmlOnly => "html_only",
        NormalizationDecision::PreferXbrlKeepHtmlAlternative => "prefer_xbrl_keep_html_alternative",
    }
}

fn issue_severity_label(severity: NormalizationIssueSeverity) -> &'static str {
    match severity {
        NormalizationIssueSeverity::Info => "info",
        NormalizationIssueSeverity::Warning => "warning",
        NormalizationIssueSeverity::Error => "error",
    }
}

fn write_domain_headers(
    worksheet: &mut Worksheet,
    model: &WorkbookExportModel,
) -> Result<(), WorkbookIoError> {
    worksheet.write_string(0, 0, "metric_id")?;
    worksheet.write_string(0, 1, "metric_label")?;
    worksheet.write_string(0, 2, "subdomain")?;

    for (index, period_column) in model.period_columns.iter().enumerate() {
        worksheet.write_string(0, (index + 3) as u16, &period_column.column_label)?;
    }

    Ok(())
}

fn write_metric_value(
    worksheet: &mut Worksheet,
    row: u32,
    column: u16,
    value: &MetricValue,
) -> Result<(), WorkbookIoError> {
    match value {
        MetricValue::Numeric(value) => worksheet.write_number(row, column, value.amount)?,
        MetricValue::Text(value) => {
            worksheet.write_string(row, column, truncate_for_excel_cell(&value.content))?
        }
    };

    Ok(())
}

fn truncate_for_excel_cell(value: &str) -> &str {
    if value.len() <= EXCEL_CELL_TEXT_LIMIT {
        return value;
    }

    let mut end = EXCEL_CELL_TEXT_LIMIT;
    while !value.is_char_boundary(end) {
        end -= 1;
    }

    &value[..end]
}

fn period_key(reporting_period: &ReportingPeriod) -> String {
    match &reporting_period.context {
        PeriodContext::Instant { as_of } => {
            as_of.format(&Iso8601::DATE).unwrap_or_else(|_| "unknown_period".to_string())
        }
        PeriodContext::Duration { start, end } => {
            let start =
                start.format(&Iso8601::DATE).unwrap_or_else(|_| "unknown_start".to_string());
            let end = end.format(&Iso8601::DATE).unwrap_or_else(|_| "unknown_end".to_string());
            format!("{start}_to_{end}")
        }
    }
}

fn period_label(reporting_period: &ReportingPeriod) -> String {
    if let Some(fiscal_period) = &reporting_period.fiscal_period {
        if let Some(label) = &reporting_period.label {
            if label.eq_ignore_ascii_case("FY") {
                return format!("FY{}", fiscal_period.fiscal_year);
            }
        }

        if let Some(quarter) = &fiscal_period.fiscal_quarter {
            let quarter_label = match quarter {
                filing_models::FiscalQuarter::Q1 => "Q1",
                filing_models::FiscalQuarter::Q2 => "Q2",
                filing_models::FiscalQuarter::Q3 => "Q3",
                filing_models::FiscalQuarter::Q4 => "Q4",
            };

            return format!("{quarter_label} {}", fiscal_period.fiscal_year);
        }

        return format!("FY{}", fiscal_period.fiscal_year);
    }

    if let Some(label) = &reporting_period.label {
        if !label.trim().is_empty() {
            return label.clone();
        }
    }

    period_key(reporting_period)
}

pub fn default_worksheet_plan() -> Vec<WorksheetPlan> {
    vec![
        WorksheetPlan {
            domain: DomainName::CompanyOverview,
            sheet_name: DomainName::CompanyOverview.sheet_name(),
            description: "Issuer identity and reporting context",
        },
        WorksheetPlan {
            domain: DomainName::FilingIndex,
            sheet_name: DomainName::FilingIndex.sheet_name(),
            description: "Per-filing metadata, accession numbers, and source references",
        },
        WorksheetPlan {
            domain: DomainName::BalanceSheet,
            sheet_name: DomainName::BalanceSheet.sheet_name(),
            description: "Balance sheet metrics with one row per metric",
        },
        WorksheetPlan {
            domain: DomainName::IncomeStatement,
            sheet_name: DomainName::IncomeStatement.sheet_name(),
            description: "Income statement metrics with one row per metric",
        },
        WorksheetPlan {
            domain: DomainName::CashFlow,
            sheet_name: DomainName::CashFlow.sheet_name(),
            description: "Cash flow metrics with one row per metric",
        },
        WorksheetPlan {
            domain: DomainName::ShareholdersEquity,
            sheet_name: DomainName::ShareholdersEquity.sheet_name(),
            description: "Shareholders' equity metrics with one row per metric",
        },
        WorksheetPlan {
            domain: DomainName::SegmentData,
            sheet_name: DomainName::SegmentData.sheet_name(),
            description: "Segment-level disclosures grouped by domain",
        },
        WorksheetPlan {
            domain: DomainName::DebtAndCredit,
            sheet_name: DomainName::DebtAndCredit.sheet_name(),
            description: "Debt and credit facility metrics",
        },
        WorksheetPlan {
            domain: DomainName::DerivativesAndSecurities,
            sheet_name: DomainName::DerivativesAndSecurities.sheet_name(),
            description: "Derivative and debt security metrics",
        },
        WorksheetPlan {
            domain: DomainName::EquityCompensation,
            sheet_name: DomainName::EquityCompensation.sheet_name(),
            description: "Equity compensation metrics grouped into one domain",
        },
        WorksheetPlan {
            domain: DomainName::Footnotes,
            sheet_name: DomainName::Footnotes.sheet_name(),
            description: "Footnote text and metadata",
        },
        WorksheetPlan {
            domain: DomainName::Mda,
            sheet_name: DomainName::Mda.sheet_name(),
            description: "MD&A text and metadata",
        },
        WorksheetPlan {
            domain: DomainName::Valuation,
            sheet_name: DomainName::Valuation.sheet_name(),
            description: "Placeholder valuation outputs and later user-defined formulas",
        },
        WorksheetPlan {
            domain: DomainName::Provenance,
            sheet_name: DomainName::Provenance.sheet_name(),
            description: "Audit trail details and source references",
        },
        WorksheetPlan {
            domain: DomainName::Schema,
            sheet_name: DomainName::Schema.sheet_name(),
            description: "Workbook schema and version metadata",
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use accounting_domains::MetricId;
    use filing_models::{
        Cik, CompanyId, FilingForm, FilingSourceMethod, FilingUrls, FiscalPeriod, FiscalQuarter,
        MeasurementUnit, NumericValue, Provenance, SignConvention, SourceLocator, SourceType,
        Ticker, ValueScale,
    };
    use normalization::{
        NormalizationDecision, NormalizationSource, NormalizedNarrativeMetric,
        NormalizedNumericMetric, NormalizedSourceValue,
    };
    use std::time::{SystemTime, UNIX_EPOCH};
    use time::macros::date;

    fn sample_company() -> CompanyIdentity {
        CompanyIdentity {
            primary_id: CompanyId::Cik(Cik::new("798354")),
            ticker: Some(Ticker::new("TEST")),
            cik: Some(Cik::new("798354")),
            issuer_name: "Example Corp".to_string(),
            exchange: Some("NYSE".to_string()),
            reported_currency: Some("USD".to_string()),
            fiscal_year_end: Some("1231".to_string()),
        }
    }

    fn sample_reporting_period() -> ReportingPeriod {
        ReportingPeriod {
            context: PeriodContext::Instant { as_of: date!(2024 - 12 - 31) },
            fiscal_period: Some(FiscalPeriod {
                fiscal_year: 2024,
                fiscal_quarter: Some(FiscalQuarter::Q4),
            }),
            label: Some("FY".to_string()),
        }
    }

    fn sample_numeric(metric_id: &str, amount: f64) -> NormalizedNumericMetric {
        let value = NumericValue {
            amount,
            unit: MeasurementUnit::Currency("USD".to_string()),
            scale: ValueScale::Raw,
            sign_convention: SignConvention::AsReported,
            label: Some("Net Income".to_string()),
            reporting_period: sample_reporting_period(),
            provenance: Provenance {
                accession_number: "0000798354-25-000010".to_string(),
                filing_url: Some("https://example.test/filing.htm".to_string()),
                form_type: FilingForm::Form10K,
                source_type: SourceType::Xbrl,
                source_method: FilingSourceMethod::ApiXbrlFacts,
                source_location: SourceLocator {
                    section_name: Some("income_statement".to_string()),
                    table_name: Some("income_statement".to_string()),
                    row_label: Some("Net Income".to_string()),
                    cell_reference: None,
                    segment_name: None,
                },
                xbrl_tag: Some("NetIncomeLoss".to_string()),
                filing_label: Some("Net Income".to_string()),
                reporting_period: sample_reporting_period(),
                unit: MeasurementUnit::Currency("USD".to_string()),
                scale: ValueScale::Raw,
            },
        };

        NormalizedNumericMetric {
            metric_id: MetricId::new(metric_id),
            period_key: "2024-12-31".to_string(),
            domain: DomainName::IncomeStatement,
            metric_name: "Net Income".to_string(),
            subdomain: Some("totals".to_string()),
            value: value.clone(),
            primary_source: NormalizationSource::XbrlPrimary,
            decision: NormalizationDecision::XbrlOnly,
            alternative_value: None,
            source_values: vec![NormalizedSourceValue {
                value,
                source: NormalizationSource::XbrlPrimary,
                selected_as_primary: true,
                review_note: Some("selected primary value".to_string()),
            }],
        }
    }

    fn sample_normalization() -> NormalizationResult {
        NormalizationResult {
            numeric_metrics: vec![sample_numeric("income_statement.net_income", 100.0)],
            narrative_metrics: Vec::<NormalizedNarrativeMetric>::new(),
            issues: Vec::new(),
        }
    }

    fn sample_valuation_output() -> ValuationOutput {
        let metric = sample_numeric("valuation.owners_earnings_placeholder", 2500.0);
        ValuationOutput {
            metric_id: MetricId::new("valuation.owners_earnings_placeholder"),
            metric_name: "Owner's Earnings Placeholder".to_string(),
            domain: DomainName::Valuation,
            value: metric.value,
            inputs: Vec::new(),
            comment: "test",
        }
    }

    fn sample_filing() -> FilingMetadata {
        FilingMetadata {
            accession_number: "0000798354-25-000010".to_string(),
            form_type: FilingForm::Form10K,
            filing_date: date!(2025 - 02 - 01),
            report_period_end: Some(date!(2024 - 12 - 31)),
            fiscal_period: Some(FiscalPeriod {
                fiscal_year: 2024,
                fiscal_quarter: Some(FiscalQuarter::Q4),
            }),
            filing_urls: FilingUrls {
                filing_detail: Some(
                    "https://www.sec.gov/Archives/edgar/data/798354/000079835425000010/"
                        .to_string(),
                ),
                primary_document: Some(
                    "https://www.sec.gov/Archives/edgar/data/798354/000079835425000010/form10k.htm"
                        .to_string(),
                ),
                xbrl_instance: None,
                html_index: Some(
                    "https://www.sec.gov/Archives/edgar/data/798354/000079835425000010/"
                        .to_string(),
                ),
            },
            source_types: vec![SourceType::Html, SourceType::Xbrl],
            is_amendment: false,
        }
    }

    #[test]
    fn builds_export_model_with_period_columns_and_domain_rows() {
        let exporter = WorkbookExporter::new();
        let model = exporter.build_model(
            sample_company(),
            &sample_normalization(),
            &[sample_valuation_output()],
        );

        assert_eq!(model.schema_version, WORKBOOK_SCHEMA_VERSION);
        assert_eq!(model.period_columns.len(), 1);
        assert_eq!(model.period_columns[0].column_label, "FY2024");
        assert!(model.sheets.iter().any(|sheet| sheet.domain == DomainName::IncomeStatement));
        assert!(model.sheets.iter().any(|sheet| sheet.domain == DomainName::Valuation));
        assert_eq!(model.provenance_records.len(), 1);
        assert!(model.duplicate_candidate_records.is_empty());
        assert!(
            model
                .coverage_records
                .iter()
                .any(|record| record.metric_id == "income_statement.net_income"
                    && record.status == "found")
        );
    }

    #[test]
    fn builds_export_model_with_filing_index_records() {
        let exporter = WorkbookExporter::new();
        let model = exporter.build_model_with_filings(
            sample_company(),
            &[sample_filing()],
            &sample_normalization(),
            &[sample_valuation_output()],
        );

        assert_eq!(model.filing_index.len(), 1);
        assert_eq!(model.filing_index[0].accession_number, "0000798354-25-000010");
        assert_eq!(model.filing_index[0].form_type, "10-K");
    }

    #[test]
    fn workbook_builds_multiple_period_columns_when_html_contributes_history() {
        let exporter = WorkbookExporter::new();
        let mut normalized = sample_normalization();
        let mut prior_period_metric = sample_numeric("income_statement.revenue", 90.0);
        prior_period_metric.period_key = "2023-12-31".to_string();
        prior_period_metric.metric_name = "Revenue".to_string();
        prior_period_metric.value.reporting_period = ReportingPeriod {
            context: PeriodContext::Instant { as_of: date!(2023 - 12 - 31) },
            fiscal_period: Some(FiscalPeriod {
                fiscal_year: 2023,
                fiscal_quarter: Some(FiscalQuarter::Q4),
            }),
            label: Some("FY".to_string()),
        };
        prior_period_metric.value.provenance.reporting_period =
            prior_period_metric.value.reporting_period.clone();
        prior_period_metric.primary_source = NormalizationSource::HtmlFallback;
        prior_period_metric.decision = NormalizationDecision::HtmlOnly;
        prior_period_metric.source_values[0].source = NormalizationSource::HtmlFallback;
        prior_period_metric.source_values[0].selected_as_primary = true;
        normalized.numeric_metrics.push(prior_period_metric);

        let model =
            exporter.build_model(sample_company(), &normalized, &[sample_valuation_output()]);

        assert!(model.period_columns.iter().any(|column| column.column_label == "FY2023"));
        assert!(model.period_columns.iter().any(|column| column.column_label == "FY2024"));
    }

    #[test]
    fn builds_duplicate_candidate_records_for_non_primary_review_values() {
        let exporter = WorkbookExporter::new();
        let mut normalized = sample_normalization();
        let metric = normalized
            .numeric_metrics
            .first_mut()
            .expect("sample normalization should contain one metric");

        let mut duplicate_value = metric.value.clone();
        duplicate_value.amount = 95.0;
        duplicate_value.provenance.source_type = SourceType::Html;
        duplicate_value.provenance.source_method = FilingSourceMethod::FilingHtml;
        duplicate_value.provenance.source_location.section_name = Some("Note 5".to_string());
        duplicate_value.provenance.source_location.table_name =
            Some("Note 5 - Operating Results".to_string());
        duplicate_value.provenance.source_location.row_label = Some("Net Income".to_string());
        duplicate_value.provenance.filing_label = Some("Net Income".to_string());

        metric.source_values.push(NormalizedSourceValue {
            value: duplicate_value,
            source: NormalizationSource::HtmlFallback,
            selected_as_primary: false,
            review_note: Some("retained duplicate source value for review".to_string()),
        });

        let model =
            exporter.build_model(sample_company(), &normalized, &[sample_valuation_output()]);

        assert_eq!(model.duplicate_candidate_records.len(), 1);
        assert_eq!(model.duplicate_candidate_records[0].metric_id, "income_statement.net_income");
        assert_eq!(model.duplicate_candidate_records[0].section_name.as_deref(), Some("Note 5"));
        assert_eq!(model.duplicate_candidate_records[0].amount, 95.0);
    }

    #[test]
    fn segment_rows_export_one_row_per_segment_and_metric() {
        let exporter = WorkbookExporter::new();
        let mut normalized = sample_normalization();
        normalized.numeric_metrics.clear();

        let mut consumer_metric = sample_numeric("segment_data.segment_revenue", 600.0);
        consumer_metric.domain = DomainName::SegmentData;
        consumer_metric.metric_name = "Segment Revenue".to_string();
        consumer_metric.subdomain = Some("segment_results".to_string());
        consumer_metric.value.provenance.source_location.segment_name =
            Some("Consumer Segment".to_string());
        consumer_metric.source_values[0].value.provenance.source_location.segment_name =
            Some("Consumer Segment".to_string());

        let mut industrial_metric = sample_numeric("segment_data.segment_revenue", 400.0);
        industrial_metric.domain = DomainName::SegmentData;
        industrial_metric.metric_name = "Segment Revenue".to_string();
        industrial_metric.subdomain = Some("segment_results".to_string());
        industrial_metric.value.provenance.source_location.segment_name =
            Some("Industrial Segment".to_string());
        industrial_metric.source_values[0].value.provenance.source_location.segment_name =
            Some("Industrial Segment".to_string());

        normalized.numeric_metrics.push(consumer_metric);
        normalized.numeric_metrics.push(industrial_metric);

        let model =
            exporter.build_model(sample_company(), &normalized, &[sample_valuation_output()]);

        let segment_sheet = model
            .sheets
            .iter()
            .find(|sheet| sheet.domain == DomainName::SegmentData)
            .expect("segment sheet should be present");

        assert_eq!(segment_sheet.rows.len(), 2);
        let mut labels: Vec<_> =
            segment_sheet.rows.iter().map(|row| row.metric_label.clone()).collect();
        labels.sort();
        assert_eq!(
            labels,
            vec![
                "Consumer Segment | Segment Revenue".to_string(),
                "Industrial Segment | Segment Revenue".to_string()
            ]
        );
    }

    #[test]
    fn exports_and_imports_workbook_schema_summary() {
        let exporter = WorkbookExporter::new();
        let model = exporter.build_model(
            sample_company(),
            &sample_normalization(),
            &[sample_valuation_output()],
        );
        let path = std::env::temp_dir().join(format!(
            "sec_edgar_scraper_test_{}.xlsx",
            SystemTime::now().duration_since(UNIX_EPOCH).expect("clock should be valid").as_nanos()
        ));

        exporter.export_to_path(&model, &path).expect("workbook should export");

        let summary = exporter.import_summary(&path).expect("workbook schema should import");

        assert_eq!(summary.schema_version, WORKBOOK_SCHEMA_VERSION);
        assert!(summary.sheet_names.contains(&DomainName::Schema.sheet_name().to_string()));
        assert!(
            summary.sheet_names.contains(&DomainName::IncomeStatement.sheet_name().to_string())
        );
        assert!(summary.sheet_names.contains(&DomainName::FilingIndex.sheet_name().to_string()));
        assert!(summary.sheet_names.contains(&DomainName::Provenance.sheet_name().to_string()));
        assert!(summary.sheet_names.contains(&"duplicate_candidates".to_string()));
        assert!(summary.sheet_names.contains(&"review_warnings".to_string()));
        assert!(summary.sheet_names.contains(&"coverage_summary".to_string()));
        assert!(summary.sheet_names.contains(&"missing_metrics".to_string()));
        assert!(summary.sheet_names.contains(&"formula_inputs".to_string()));

        let _ = std::fs::remove_file(path);
    }
}
