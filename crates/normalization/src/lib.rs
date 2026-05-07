//! Normalization and source reconciliation.
//!
//! This layer keeps data domain-first while preserving enough source detail for later review.
//! Numeric values prefer XBRL when available, fall back to HTML when XBRL is missing, and keep the
//! alternative source visible instead of discarding it silently.

use accounting_domains::{DomainName, MetricId};
use filing_models::{MetricValue, NumericValue, TextBlock};
use html_extractor::{ExtractedHtmlMetricValue, ExtractedNarrativeSection, HtmlExtractionResult};
use std::collections::BTreeMap;
use time::format_description::well_known::Iso8601;
use xbrl_extractor::ExtractedMetricValue;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormalizationSource {
    XbrlPrimary,
    HtmlFallback,
    NarrativeHtml,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormalizationDecision {
    XbrlOnly,
    HtmlOnly,
    PreferXbrlKeepHtmlAlternative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormalizationIssueSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizationIssue {
    pub severity: NormalizationIssueSeverity,
    pub code: &'static str,
    pub metric_id: Option<MetricId>,
    pub period_key: Option<String>,
    pub segment_name: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedSourceValue {
    pub value: NumericValue,
    pub source: NormalizationSource,
    pub selected_as_primary: bool,
    pub review_note: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedNumericMetric {
    pub metric_id: MetricId,
    pub period_key: String,
    pub domain: DomainName,
    pub metric_name: String,
    pub subdomain: Option<String>,
    pub value: NumericValue,
    pub primary_source: NormalizationSource,
    pub decision: NormalizationDecision,
    pub alternative_value: Option<NumericValue>,
    pub source_values: Vec<NormalizedSourceValue>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedNarrativeMetric {
    pub metric_id: MetricId,
    pub domain: DomainName,
    pub value: TextBlock,
    pub primary_source: NormalizationSource,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct NormalizationResult {
    pub numeric_metrics: Vec<NormalizedNumericMetric>,
    pub narrative_metrics: Vec<NormalizedNarrativeMetric>,
    pub issues: Vec<NormalizationIssue>,
}

#[derive(Debug, Default)]
pub struct Normalizer;

impl Normalizer {
    pub fn new() -> Self {
        Self
    }

    pub fn normalize(
        &self,
        xbrl_metrics: &[ExtractedMetricValue],
        html_result: &HtmlExtractionResult,
    ) -> NormalizationResult {
        let mut by_metric_period: BTreeMap<String, MetricCandidates> = BTreeMap::new();

        for metric in xbrl_metrics {
            let period_key = normalized_period_key(&metric.numeric_value.reporting_period);
            let entry = by_metric_period
                .entry(candidate_key(
                    metric.metric_id.as_str(),
                    &period_key,
                    metric.domain,
                    &metric.numeric_value,
                ))
                .or_insert_with(|| MetricCandidates::from_xbrl_metadata(metric, period_key));
            entry.xbrl.push(metric.numeric_value.clone());
            entry.domain = metric.domain;
            entry.metric_name = metric.metric_name.clone();
            entry.subdomain = metric.subdomain.clone();
        }

        for metric in &html_result.numeric_fallbacks {
            if metric.domain == DomainName::SegmentData
                && metric.numeric_value.provenance.source_location.segment_name.is_none()
            {
                continue;
            }
            let period_key = normalized_period_key(&metric.numeric_value.reporting_period);
            let entry = by_metric_period
                .entry(candidate_key(
                    metric.metric_id.as_str(),
                    &period_key,
                    metric.domain,
                    &metric.numeric_value,
                ))
                .or_insert_with(|| MetricCandidates::from_html_metadata(metric, period_key));
            entry.html.push(metric.numeric_value.clone());
            entry.domain = metric.domain;
            entry.metric_name = metric.metric_name.clone();
            entry.subdomain = metric.subdomain.clone();
        }

        let mut issues = Vec::new();
        let mut numeric_metrics: Vec<NormalizedNumericMetric> = by_metric_period
            .into_values()
            .filter_map(|candidates| normalize_numeric_metric(candidates, &mut issues))
            .collect();

        numeric_metrics.extend(derive_income_statement_metrics(&numeric_metrics));
        numeric_metrics.extend(derive_share_count_metrics(&numeric_metrics));

        numeric_metrics.sort_by(|left, right| {
            left.metric_id
                .as_str()
                .cmp(right.metric_id.as_str())
                .then_with(|| left.period_key.cmp(&right.period_key))
                .then_with(|| left.metric_name.cmp(&right.metric_name))
        });

        let mut narrative_metrics: Vec<NormalizedNarrativeMetric> =
            html_result.narrative_sections.iter().filter_map(normalize_narrative_metric).collect();

        narrative_metrics.sort_by(|left, right| {
            left.metric_id
                .as_str()
                .cmp(right.metric_id.as_str())
                .then_with(|| left.domain.sheet_name().cmp(right.domain.sheet_name()))
        });

        NormalizationResult { numeric_metrics, narrative_metrics, issues }
    }
}

fn derive_income_statement_metrics(
    numeric_metrics: &[NormalizedNumericMetric],
) -> Vec<NormalizedNumericMetric> {
    if numeric_metrics
        .iter()
        .any(|metric| metric.metric_id.as_str() == "income_statement.gross_profit")
    {
        return Vec::new();
    }

    let revenue_by_period = numeric_metrics
        .iter()
        .filter(|metric| metric.metric_id.as_str() == "income_statement.revenue")
        .map(|metric| (metric.period_key.clone(), metric))
        .collect::<BTreeMap<_, _>>();
    let cogs_by_period = numeric_metrics
        .iter()
        .filter(|metric| metric.metric_id.as_str() == "income_statement.cost_of_goods_sold")
        .map(|metric| (metric.period_key.clone(), metric))
        .collect::<BTreeMap<_, _>>();
    let cogs_by_end_date = numeric_metrics
        .iter()
        .filter(|metric| metric.metric_id.as_str() == "income_statement.cost_of_goods_sold")
        .map(|metric| (reporting_period_end_key(&metric.value.reporting_period), metric))
        .collect::<BTreeMap<_, _>>();

    let mut derived = Vec::new();
    for (period_key, revenue_metric) in revenue_by_period {
        let cogs_metric = if let Some(metric) = cogs_by_period.get(&period_key) {
            Some(*metric)
        } else {
            cogs_by_end_date
                .get(&reporting_period_end_key(&revenue_metric.value.reporting_period))
                .copied()
        };
        let Some(cogs_metric) = cogs_metric else {
            continue;
        };

        let mut derived_value = revenue_metric.value.clone();
        derived_value.amount = revenue_metric.value.amount - cogs_metric.value.amount;
        derived_value.label = Some("Derived gross profit".to_string());
        derived_value.provenance.filing_label = Some("Derived gross profit".to_string());
        derived_value.provenance.source_location.row_label =
            Some("Derived gross profit".to_string());
        derived_value.provenance.source_location.section_name =
            Some("derived_income_statement_metrics".to_string());
        derived_value.provenance.source_location.table_name =
            Some("derived_income_statement_metrics".to_string());
        // Gross profit is derived only when the filing does not expose a direct fact. This keeps
        // the analyst workbook usable while making the derivation explicit in provenance. If you
        // later want a stricter direct-fact-only policy, remove this helper and leave the metric
        // in missing_metrics instead.
        derived_value.provenance.xbrl_tag = Some("derived_from_revenue_minus_cogs".to_string());

        derived.push(NormalizedNumericMetric {
            metric_id: MetricId::new("income_statement.gross_profit"),
            period_key,
            domain: DomainName::IncomeStatement,
            metric_name: "Gross Profit".to_string(),
            subdomain: Some("profitability".to_string()),
            value: derived_value.clone(),
            primary_source: NormalizationSource::XbrlPrimary,
            decision: NormalizationDecision::XbrlOnly,
            alternative_value: None,
            source_values: vec![NormalizedSourceValue {
                value: derived_value,
                source: NormalizationSource::XbrlPrimary,
                selected_as_primary: true,
                review_note: Some("derived from revenue minus cost_of_goods_sold".to_string()),
            }],
        });
    }

    derived
}

fn reporting_period_end_key(reporting_period: &filing_models::ReportingPeriod) -> String {
    match reporting_period.context {
        filing_models::PeriodContext::Instant { as_of } => {
            as_of.format(&Iso8601::DATE).unwrap_or_default()
        }
        filing_models::PeriodContext::Duration { end, .. } => {
            end.format(&Iso8601::DATE).unwrap_or_default()
        }
    }
}

fn derive_share_count_metrics(
    numeric_metrics: &[NormalizedNumericMetric],
) -> Vec<NormalizedNumericMetric> {
    if numeric_metrics.iter().any(|metric| {
        metric.metric_id.as_str() == "equity_compensation.net_change_shares_outstanding"
    }) {
        return Vec::new();
    }

    let mut shares_outstanding = numeric_metrics
        .iter()
        .filter(|metric| metric.metric_id.as_str() == "shareholders_equity.shares_outstanding")
        .filter_map(|metric| match metric.value.reporting_period.context {
            filing_models::PeriodContext::Instant { as_of } => Some((as_of, metric)),
            filing_models::PeriodContext::Duration { .. } => None,
        })
        .collect::<Vec<_>>();

    shares_outstanding.sort_by_key(|(as_of, _)| *as_of);

    let mut derived = Vec::new();
    for window in shares_outstanding.windows(2) {
        let [previous, current] = window else {
            continue;
        };
        let (_, previous_metric) = previous;
        let (_, current_metric) = current;
        let delta = current_metric.value.amount - previous_metric.value.amount;
        let mut derived_value = current_metric.value.clone();
        derived_value.amount = delta;
        derived_value.label = Some("Derived net change in shares outstanding".to_string());
        derived_value.provenance.filing_label =
            Some("Derived net change in shares outstanding".to_string());
        derived_value.provenance.source_location.row_label =
            Some("Derived net change in shares outstanding".to_string());
        derived_value.provenance.source_location.section_name =
            Some("derived_share_count_metrics".to_string());
        derived_value.provenance.source_location.table_name =
            Some("derived_share_count_metrics".to_string());
        // This metric is intentionally derived from consecutive shares_outstanding values because
        // many issuers do not expose a direct SEC fact for net share-count change. If you later
        // want a different methodology, such as buybacks + issuance roll-forward logic, replace
        // this function and keep the derived provenance marker so workbook review stays clear.
        derived_value.provenance.xbrl_tag =
            Some("derived_from_shares_outstanding_delta".to_string());

        derived.push(NormalizedNumericMetric {
            metric_id: MetricId::new("equity_compensation.net_change_shares_outstanding"),
            period_key: normalized_period_key(&derived_value.reporting_period),
            domain: DomainName::EquityCompensation,
            metric_name: "Net Change in Shares Outstanding".to_string(),
            subdomain: Some("share_counts".to_string()),
            value: derived_value.clone(),
            primary_source: NormalizationSource::XbrlPrimary,
            decision: NormalizationDecision::XbrlOnly,
            alternative_value: None,
            source_values: vec![NormalizedSourceValue {
                value: derived_value,
                source: NormalizationSource::XbrlPrimary,
                selected_as_primary: true,
                review_note: Some(
                    "derived from consecutive shareholders_equity.shares_outstanding values"
                        .to_string(),
                ),
            }],
        });
    }

    derived
}

#[derive(Debug, Clone)]
struct MetricCandidates {
    metric_id: String,
    period_key: String,
    domain: DomainName,
    metric_name: String,
    subdomain: Option<String>,
    xbrl: Vec<NumericValue>,
    html: Vec<NumericValue>,
}

impl MetricCandidates {
    fn from_xbrl_metadata(metric: &ExtractedMetricValue, period_key: String) -> Self {
        Self {
            metric_id: metric.metric_id.as_str().to_string(),
            period_key,
            domain: metric.domain,
            metric_name: metric.metric_name.clone(),
            subdomain: metric.subdomain.clone(),
            xbrl: Vec::new(),
            html: Vec::new(),
        }
    }

    fn from_html_metadata(metric: &ExtractedHtmlMetricValue, period_key: String) -> Self {
        Self {
            metric_id: metric.metric_id.as_str().to_string(),
            period_key,
            domain: metric.domain,
            metric_name: metric.metric_name.clone(),
            subdomain: metric.subdomain.clone(),
            xbrl: Vec::new(),
            html: Vec::new(),
        }
    }
}

fn normalize_numeric_metric(
    mut candidates: MetricCandidates,
    issues: &mut Vec<NormalizationIssue>,
) -> Option<NormalizedNumericMetric> {
    prune_html_outlier_values(&candidates.metric_id, &mut candidates.html);
    rank_source_values(&candidates.metric_id, &mut candidates.xbrl);
    rank_source_values(&candidates.metric_id, &mut candidates.html);
    promote_segment_inline_xbrl_aggregate_totals(&candidates.metric_id, &mut candidates.html);
    prefer_same_accession_notes_and_bonds_total(&candidates.metric_id, &mut candidates.html);
    prune_metric_specific_history_duplicates(&candidates.metric_id, &mut candidates.html);
    collapse_same_accession_duplicates(candidates.domain, &mut candidates.xbrl);
    collapse_same_accession_duplicates(candidates.domain, &mut candidates.html);

    record_duplicate_issue(issues, &candidates, NormalizationSource::XbrlPrimary, &candidates.xbrl);
    record_duplicate_issue(
        issues,
        &candidates,
        NormalizationSource::HtmlFallback,
        &candidates.html,
    );

    let xbrl = candidates.xbrl.first().cloned();
    let html = candidates.html.first().cloned();
    let preferred_segment_name = preferred_segment_name(&candidates);

    if let (Some(xbrl), Some(html)) = (&xbrl, &html) {
        if amounts_differ(xbrl.amount, html.amount) {
            issues.push(NormalizationIssue {
                severity: NormalizationIssueSeverity::Warning,
                code: "xbrl_html_value_conflict",
                metric_id: Some(MetricId::new(candidates.metric_id.clone())),
                period_key: Some(candidates.period_key.clone()),
                segment_name: preferred_segment_name.as_ref().cloned(),
                message: "XBRL and HTML produced different values. XBRL was selected and HTML was retained for review.".to_string(),
            });
        }
    }

    match (xbrl, html) {
        (Some(mut xbrl), Some(mut html)) => {
            apply_preferred_segment_name(&mut xbrl, preferred_segment_name.as_deref());
            apply_preferred_segment_name(&mut html, preferred_segment_name.as_deref());

            Some(NormalizedNumericMetric {
                metric_id: MetricId::new(candidates.metric_id.clone()),
                period_key: candidates.period_key,
                domain: candidates.domain,
                metric_name: candidates.metric_name,
                subdomain: candidates.subdomain,
                value: xbrl,
                primary_source: NormalizationSource::XbrlPrimary,
                decision: NormalizationDecision::PreferXbrlKeepHtmlAlternative,
                alternative_value: Some(html.clone()),
                source_values: source_values_for_reconciled_sources(
                    candidates.xbrl,
                    candidates.html,
                    NormalizationSource::XbrlPrimary,
                ),
            })
        }
        (Some(mut xbrl), None) => {
            apply_preferred_segment_name(&mut xbrl, preferred_segment_name.as_deref());

            Some(NormalizedNumericMetric {
                metric_id: MetricId::new(candidates.metric_id.clone()),
                period_key: candidates.period_key,
                domain: candidates.domain,
                metric_name: candidates.metric_name,
                subdomain: candidates.subdomain,
                value: xbrl,
                primary_source: NormalizationSource::XbrlPrimary,
                decision: NormalizationDecision::XbrlOnly,
                alternative_value: None,
                source_values: source_values_for_single_source(
                    candidates.xbrl,
                    NormalizationSource::XbrlPrimary,
                ),
            })
        }
        (None, Some(mut html)) => {
            apply_preferred_segment_name(&mut html, preferred_segment_name.as_deref());

            Some(NormalizedNumericMetric {
                metric_id: MetricId::new(candidates.metric_id.clone()),
                period_key: candidates.period_key,
                domain: candidates.domain,
                metric_name: candidates.metric_name,
                subdomain: candidates.subdomain,
                value: html,
                primary_source: NormalizationSource::HtmlFallback,
                decision: NormalizationDecision::HtmlOnly,
                alternative_value: None,
                source_values: source_values_for_single_source(
                    candidates.html,
                    NormalizationSource::HtmlFallback,
                ),
            })
        }
        (None, None) => None,
    }
}

fn candidate_key(
    metric_id: &str,
    period_key: &str,
    domain: DomainName,
    value: &NumericValue,
) -> String {
    if domain == DomainName::SegmentData {
        let segment_name = value
            .provenance
            .source_location
            .segment_name
            .as_deref()
            .map(canonical_segment_group_key)
            .unwrap_or_else(|| "unassigned_segment".to_string());
        format!("{metric_id}::{period_key}::{segment_name}")
    } else {
        format!("{metric_id}::{period_key}")
    }
}

fn canonical_segment_group_key(segment_name: &str) -> String {
    let normalized = segment_name
        .replace("Healthc Care", "Healthcare")
        .replace("healthc care", "healthcare")
        .replace('&', " and ");
    let normalized = normalized
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character.is_ascii_whitespace() {
                character.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>();

    normalized
        .split_whitespace()
        .filter(|token| *token != "reportable" && *token != "member" && *token != "segment")
        .collect::<Vec<_>>()
        .join(" ")
}

fn preferred_segment_name(candidates: &MetricCandidates) -> Option<String> {
    if candidates.domain != DomainName::SegmentData {
        return None;
    }

    candidates
        .xbrl
        .iter()
        .chain(candidates.html.iter())
        .filter_map(|value| value.provenance.source_location.segment_name.as_deref())
        .map(clean_segment_display_name)
        .min_by(|left, right| {
            segment_display_rank(left)
                .cmp(&segment_display_rank(right))
                .then_with(|| left.cmp(right))
        })
}

fn clean_segment_display_name(segment_name: &str) -> String {
    segment_name
        .replace("Healthc Care", "Healthcare")
        .replace("healthc care", "Healthcare")
        .replace("Reportable Segment", "Segment")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn segment_display_rank(segment_name: &str) -> (u8, usize) {
    let lowercase = segment_name.to_ascii_lowercase();
    let has_reportable = lowercase.contains("reportable");
    let starts_with_company_prefix =
        lowercase.starts_with("boeing ") || lowercase.starts_with("g e ");

    (u8::from(has_reportable) + u8::from(starts_with_company_prefix), segment_name.len())
}

fn apply_preferred_segment_name(value: &mut NumericValue, preferred_segment_name: Option<&str>) {
    if let Some(segment_name) = preferred_segment_name {
        value.provenance.source_location.segment_name = Some(segment_name.to_string());
    }
}

pub fn normalized_period_key(reporting_period: &filing_models::ReportingPeriod) -> String {
    match &reporting_period.context {
        filing_models::PeriodContext::Instant { as_of } => {
            as_of.format(&Iso8601::DATE).unwrap_or_else(|_| "unknown_period".to_string())
        }
        filing_models::PeriodContext::Duration { start, end } => {
            let start =
                start.format(&Iso8601::DATE).unwrap_or_else(|_| "unknown_start".to_string());
            let end = end.format(&Iso8601::DATE).unwrap_or_else(|_| "unknown_end".to_string());
            format!("{start}_to_{end}")
        }
    }
}

fn record_duplicate_issue(
    issues: &mut Vec<NormalizationIssue>,
    candidates: &MetricCandidates,
    source: NormalizationSource,
    values: &[NumericValue],
) {
    let count = values.len();
    if count <= 1 {
        return;
    }

    if duplicate_values_are_identical(values) {
        return;
    }

    if suppress_history_duplicate_issue(&candidates.metric_id, values) {
        return;
    }

    let source_name = match source {
        NormalizationSource::XbrlPrimary => "XBRL",
        NormalizationSource::HtmlFallback => "HTML",
        NormalizationSource::NarrativeHtml => "narrative HTML",
    };

    issues.push(NormalizationIssue {
        severity: NormalizationIssueSeverity::Warning,
        code: "duplicate_source_values",
        metric_id: Some(MetricId::new(candidates.metric_id.clone())),
        period_key: Some(candidates.period_key.clone()),
        segment_name: preferred_segment_name(candidates),
        message: format!("{source_name} produced {count} values for the same metric and period. The first value was selected and the rest were retained in provenance for review."),
    });
}

fn duplicate_values_are_identical(values: &[NumericValue]) -> bool {
    let Some(first) = values.first() else {
        return true;
    };

    values.iter().all(|value| {
        !amounts_differ(first.amount, value.amount)
            && first.unit == value.unit
            && first.scale == value.scale
            && first.reporting_period == value.reporting_period
    })
}

fn suppress_history_duplicate_issue(metric_id: &str, values: &[NumericValue]) -> bool {
    if values.len() < 2 {
        return false;
    }

    if suppress_segment_inline_xbrl_component_duplicate_issue(metric_id, values) {
        return true;
    }

    if suppress_same_accession_segment_duplicate_issue(metric_id, values) {
        return true;
    }

    if suppress_repeated_segment_value_sets_across_filings(metric_id, values) {
        return true;
    }

    if suppress_same_accession_notes_and_bonds_duplicate_issue(metric_id, values) {
        return true;
    }

    let supports_history_suppression = metric_id.starts_with("segment_data.")
        || metric_id == "debt_and_credit.notes_and_bonds";
    if !supports_history_suppression {
        return false;
    }

    let mut ranks = values
        .iter()
        .map(|value| history_rank(metric_id, value))
        .collect::<Vec<_>>();
    ranks.sort();

    let Some(best_rank) = ranks.first() else {
        return false;
    };

    let best_rank_count = ranks.iter().filter(|rank| *rank == best_rank).count();
    best_rank_count == 1
}

fn suppress_same_accession_segment_duplicate_issue(
    metric_id: &str,
    values: &[NumericValue],
) -> bool {
    if !metric_id.starts_with("segment_data.") || values.len() < 2 {
        return false;
    }

    let Some(first) = values.first() else {
        return false;
    };

    values
        .iter()
        .all(|value| value.provenance.accession_number == first.provenance.accession_number)
}

fn suppress_repeated_segment_value_sets_across_filings(
    metric_id: &str,
    values: &[NumericValue],
) -> bool {
    if !metric_id.starts_with("segment_data.") || values.len() < 4 {
        return false;
    }

    let mut by_accession: std::collections::BTreeMap<&str, Vec<&NumericValue>> =
        std::collections::BTreeMap::new();
    for value in values {
        by_accession
            .entry(value.provenance.accession_number.as_str())
            .or_default()
            .push(value);
    }

    if by_accession.len() < 2 {
        return false;
    }

    let mut patterns = by_accession
        .values()
        .map(|group| {
            let mut amounts = group.iter().map(|value| value.amount).collect::<Vec<_>>();
            amounts.sort_by(|left, right| left.total_cmp(right));
            amounts
        })
        .collect::<Vec<_>>();

    patterns.dedup_by(|left, right| {
        left.len() == right.len()
            && left
                .iter()
                .zip(right.iter())
                .all(|(l, r)| !amounts_differ(*l, *r))
    });

    patterns.len() == 1
}

fn suppress_same_accession_notes_and_bonds_duplicate_issue(
    metric_id: &str,
    values: &[NumericValue],
) -> bool {
    if metric_id != "debt_and_credit.notes_and_bonds" || values.len() != 2 {
        return false;
    }

    let Some(first) = values.first() else {
        return false;
    };

    values
        .iter()
        .all(|value| value.provenance.accession_number == first.provenance.accession_number)
}

fn suppress_segment_inline_xbrl_component_duplicate_issue(
    metric_id: &str,
    values: &[NumericValue],
) -> bool {
    if !metric_id.starts_with("segment_data.") {
        return false;
    }

    let representative_values = segment_inline_xbrl_representative_values(values);
    if representative_values.is_empty() {
        return false;
    }

    if representative_values.len() == 1 || duplicate_values_are_identical(&representative_values) {
        return true;
    }

    let mut ranks = representative_values
        .iter()
        .map(|value| history_rank(metric_id, value))
        .collect::<Vec<_>>();
    ranks.sort();

    let Some(best_rank) = ranks.first() else {
        return false;
    };

    ranks.iter().filter(|rank| *rank == best_rank).count() == 1
}

fn promote_segment_inline_xbrl_aggregate_totals(metric_id: &str, values: &mut [NumericValue]) {
    if !metric_id.starts_with("segment_data.") || values.len() < 2 {
        return;
    }

    let aggregate_keys = segment_inline_xbrl_aggregate_keys(values);
    if aggregate_keys.is_empty() {
        return;
    }

    values.sort_by(|left, right| {
        let left_rank = u8::from(!aggregate_keys.contains(&segment_inline_xbrl_value_key(left)));
        let right_rank =
            u8::from(!aggregate_keys.contains(&segment_inline_xbrl_value_key(right)));

        left_rank
            .cmp(&right_rank)
            .then_with(|| history_rank(metric_id, left).cmp(&history_rank(metric_id, right)))
            .then_with(|| left.provenance.accession_number.cmp(&right.provenance.accession_number))
    });
}

fn prefer_same_accession_notes_and_bonds_total(metric_id: &str, values: &mut [NumericValue]) {
    if metric_id != "debt_and_credit.notes_and_bonds" || values.len() < 2 {
        return;
    }

    values.sort_by(|left, right| {
        let accession_cmp = left.provenance.accession_number.cmp(&right.provenance.accession_number);
        if accession_cmp != std::cmp::Ordering::Equal {
            return history_rank(metric_id, left).cmp(&history_rank(metric_id, right));
        }

        right
            .amount
            .abs()
            .total_cmp(&left.amount.abs())
            .then_with(|| history_rank(metric_id, left).cmp(&history_rank(metric_id, right)))
            .then_with(|| {
                left.provenance
                    .filing_label
                    .as_deref()
                    .unwrap_or_default()
                    .cmp(right.provenance.filing_label.as_deref().unwrap_or_default())
            })
    });
}

fn segment_inline_xbrl_representative_values(values: &[NumericValue]) -> Vec<NumericValue> {
    let aggregate_keys = segment_inline_xbrl_aggregate_keys(values);
    if aggregate_keys.is_empty() {
        return Vec::new();
    }

    let mut representatives = Vec::new();
    let mut seen_accessions = std::collections::BTreeSet::new();

    for value in values {
        if !aggregate_keys.contains(&segment_inline_xbrl_value_key(value)) {
            continue;
        }

        if seen_accessions.insert(value.provenance.accession_number.clone()) {
            representatives.push(value.clone());
        }
    }

    representatives
}

fn segment_inline_xbrl_aggregate_keys(
    values: &[NumericValue],
) -> std::collections::BTreeSet<String> {
    let mut groups: std::collections::BTreeMap<String, Vec<&NumericValue>> =
        std::collections::BTreeMap::new();

    for value in values {
        if !is_inline_xbrl_segment_value(value) {
            continue;
        }
        groups
            .entry(value.provenance.accession_number.clone())
            .or_default()
            .push(value);
    }

    let mut aggregate_keys = std::collections::BTreeSet::new();
    for group_values in groups.into_values() {
        let Some(total) = segment_inline_xbrl_aggregate_total(group_values.as_slice()) else {
            continue;
        };
        aggregate_keys.insert(segment_inline_xbrl_value_key(total));
    }

    aggregate_keys
}

fn segment_inline_xbrl_aggregate_total<'a>(
    values: &[&'a NumericValue],
) -> Option<&'a NumericValue> {
    if values.len() < 3 {
        return None;
    }

    let mut sorted = values.to_vec();
    sorted.sort_by(|left, right| right.amount.total_cmp(&left.amount));

    let total = *sorted.first()?;
    let components = &sorted[1..];
    if components.is_empty() {
        return None;
    }

    let component_sum = components.iter().map(|value| value.amount).sum::<f64>();
    if !amounts_differ(total.amount, component_sum) {
        Some(total)
    } else {
        None
    }
}

fn is_inline_xbrl_segment_value(value: &NumericValue) -> bool {
    value.provenance.source_location.section_name.as_deref() == Some("inline_xbrl_segment")
        && value.provenance.source_location.table_name.as_deref() == Some("inline_xbrl_segment")
}

fn segment_inline_xbrl_value_key(value: &NumericValue) -> String {
    format!(
        "{}::{}::{}::{}",
        value.provenance.accession_number,
        normalized_period_key(&value.reporting_period),
        value.provenance
            .source_location
            .segment_name
            .as_deref()
            .unwrap_or_default(),
        value.amount
    )
}

fn collapse_same_accession_duplicates(domain: DomainName, values: &mut Vec<NumericValue>) {
    if values.len() < 2 {
        return;
    }

    let mut deduped: Vec<NumericValue> = Vec::with_capacity(values.len());
    for value in values.drain(..) {
        let is_duplicate = deduped.iter().any(|existing| {
            existing.provenance.accession_number == value.provenance.accession_number
                && !amounts_differ(existing.amount, value.amount)
                && existing.unit == value.unit
                && existing.scale == value.scale
                && existing.reporting_period == value.reporting_period
                && (domain != DomainName::SegmentData
                    || existing.provenance.source_location.segment_name
                        == value.provenance.source_location.segment_name)
        });

        if !is_duplicate {
            deduped.push(value);
        }
    }

    *values = deduped;
}

fn rank_source_values(metric_id: &str, values: &mut [NumericValue]) {
    values.sort_by(|left, right| {
        source_rank(metric_id, left)
            .cmp(&source_rank(metric_id, right))
            .then_with(|| history_rank(metric_id, left).cmp(&history_rank(metric_id, right)))
            .then_with(|| {
                left.provenance
                    .filing_label
                    .as_deref()
                    .unwrap_or_default()
                    .cmp(right.provenance.filing_label.as_deref().unwrap_or_default())
            })
    });
}

fn prune_metric_specific_history_duplicates(metric_id: &str, values: &mut Vec<NumericValue>) {
    if metric_id != "derivatives_and_securities.derivative_gain_loss" || values.len() < 2 {
        return;
    }

    let best_rank = values.iter().map(derivative_gain_loss_history_rank).min().unwrap_or((
        i32::MAX,
        u8::MAX,
        String::new(),
    ));

    values.retain(|value| derivative_gain_loss_history_rank(value) == best_rank);
}

fn segment_history_rank(metric_id: &str, value: &NumericValue) -> (i32, String) {
    if !metric_id.starts_with("segment_data.") {
        return (0, value.provenance.accession_number.clone());
    }

    let reporting_year = match value.reporting_period.context {
        filing_models::PeriodContext::Instant { as_of } => as_of.year(),
        filing_models::PeriodContext::Duration { end, .. } => end.year(),
    };
    let filing_year =
        accession_filing_year(&value.provenance.accession_number).unwrap_or(reporting_year);
    ((filing_year - reporting_year).abs(), value.provenance.accession_number.clone())
}

fn history_rank(metric_id: &str, value: &NumericValue) -> (i32, String) {
    if metric_id.starts_with("segment_data.") {
        return segment_history_rank(metric_id, value);
    }

    if metric_id == "debt_and_credit.notes_and_bonds" {
        let reporting_year = match value.reporting_period.context {
            filing_models::PeriodContext::Instant { as_of } => as_of.year(),
            filing_models::PeriodContext::Duration { end, .. } => end.year(),
        };
        let filing_year =
            accession_filing_year(&value.provenance.accession_number).unwrap_or(reporting_year);
        return ((filing_year - reporting_year).abs(), value.provenance.accession_number.clone());
    }

    (0, value.provenance.accession_number.clone())
}

fn accession_filing_year(accession_number: &str) -> Option<i32> {
    let filing_year = accession_number.split('-').nth(1)?;
    let two_digit_year: i32 = filing_year.parse().ok()?;
    Some(2000 + two_digit_year)
}

fn derivative_gain_loss_history_rank(value: &NumericValue) -> (i32, u8, String) {
    let reporting_year = match value.reporting_period.context {
        filing_models::PeriodContext::Instant { as_of } => as_of.year(),
        filing_models::PeriodContext::Duration { end, .. } => end.year(),
    };
    let filing_year =
        accession_filing_year(&value.provenance.accession_number).unwrap_or(reporting_year);
    let year_delta = (filing_year - reporting_year).abs();
    let form_penalty = match (&value.reporting_period.context, &value.provenance.form_type) {
        (
            filing_models::PeriodContext::Duration { start, end },
            filing_models::FilingForm::Form10K,
        ) if start.month() == time::Month::January
            && start.day() == 1
            && end.month() == time::Month::December
            && end.day() == 31 =>
        {
            0
        }
        (
            filing_models::PeriodContext::Duration { start, end },
            filing_models::FilingForm::Form10Q,
        ) if !(start.month() == time::Month::January
            && start.day() == 1
            && end.month() == time::Month::December
            && end.day() == 31) =>
        {
            0
        }
        _ => 1,
    };

    (year_delta, form_penalty, value.provenance.accession_number.clone())
}

fn prune_html_outlier_values(metric_id: &str, values: &mut Vec<NumericValue>) {
    prune_context_mismatches(metric_id, values);

    if values.len() < 3 {
        return;
    }

    let dominant_amount = values.iter().map(|value| value.amount.abs()).fold(0.0_f64, f64::max);

    if dominant_amount >= 1000.0 {
        values.retain(|value| {
            let amount = value.amount.abs();
            amount >= 1000.0 || amount >= dominant_amount * 0.25
        });
    }

    if values.len() < 3 {
        return;
    }

    let mut has_large_duplicate_pair = false;
    for (left_index, left) in values.iter().enumerate() {
        for right in values.iter().skip(left_index + 1) {
            if !amounts_differ(left.amount, right.amount)
                && left.amount.abs() >= 10.0
                && left.unit == right.unit
                && left.scale == right.scale
                && left.reporting_period == right.reporting_period
            {
                has_large_duplicate_pair = true;
                break;
            }
        }
        if has_large_duplicate_pair {
            break;
        }
    }

    if metric_id == "income_statement.cost_of_goods_sold" {
        prune_annual_quarter_fragments(values);
    }

    if !has_large_duplicate_pair {
        return;
    }

    values.retain(|value| value.amount.abs() >= 10.0);
}

fn source_rank(metric_id: &str, value: &NumericValue) -> u8 {
    match value.provenance.source_type {
        filing_models::SourceType::Xbrl => 0,
        filing_models::SourceType::Html => html_source_rank(metric_id, value),
        filing_models::SourceType::Text => 4,
        filing_models::SourceType::WorkbookImport => 5,
    }
}

fn html_source_rank(metric_id: &str, value: &NumericValue) -> u8 {
    let label = value
        .provenance
        .source_location
        .row_label
        .as_deref()
        .or(value.label.as_deref())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if metric_id == "income_statement.cost_of_goods_sold"
        && context_contains(value, "percent of net sales")
    {
        return 5;
    }

    if metric_id == "debt_and_credit.interest_rate" {
        return debt_interest_rate_rank(&label);
    }

    if metric_id == "balance_sheet.long_term_debt" {
        return long_term_debt_rank(value);
    }

    if label.starts_with("total ") || label.starts_with("net cash ") {
        1
    } else if metric_id == "income_statement.cost_of_goods_sold"
        && statement_style_income_context(value)
    {
        1
    } else {
        2
    }
}

fn debt_interest_rate_rank(label: &str) -> u8 {
    if label.contains("weighted average") {
        1
    } else if label.contains("fixed rate debt")
        || label.contains("fixed-rate debt")
        || label.contains("floating rate debt")
        || label.contains("floating-rate debt")
        || label.contains("current portion of long term debt")
        || label.contains("short term borrowings")
    {
        2
    } else if label.contains("note") || label.contains("bond") || label.contains("debenture") {
        3
    } else {
        4
    }
}

fn long_term_debt_rank(value: &NumericValue) -> u8 {
    if context_contains(value, "selected consolidated balance sheets")
        || context_contains(value, "balance sheets data")
        || context_contains(value, "balance sheet")
    {
        1
    } else if context_contains(value, "assets and liabilities measured at fair value")
        || context_contains(value, "fair value hierarchy")
        || context_contains(value, "derivative netting adjustments")
    {
        5
    } else {
        3
    }
}

fn prune_context_mismatches(metric_id: &str, values: &mut Vec<NumericValue>) {
    if metric_id == "income_statement.cost_of_goods_sold" {
        values.retain(|value| !context_contains(value, "percent of net sales"));
        prune_cost_of_goods_sold_context_mismatches(values);
    } else if metric_id == "balance_sheet.long_term_debt" {
        let has_non_fair_value_candidate = values.iter().any(|value| {
            !context_contains(value, "assets and liabilities measured at fair value")
                && !context_contains(value, "fair value hierarchy")
                && !context_contains(value, "derivative netting adjustments")
        });

        if has_non_fair_value_candidate {
            values.retain(|value| {
                !context_contains(value, "assets and liabilities measured at fair value")
                    && !context_contains(value, "fair value hierarchy")
                    && !context_contains(value, "derivative netting adjustments")
            });
        }
    } else if metric_id == "debt_and_credit.interest_rate" {
        let has_non_fair_value_candidate = values.iter().any(|value| {
            !context_contains(value, "assets and liabilities measured at fair value")
                && !context_contains(value, "fair value hierarchy")
                && !context_contains(value, "derivative netting adjustments")
        });

        if has_non_fair_value_candidate {
            values.retain(|value| {
                !context_contains(value, "assets and liabilities measured at fair value")
                    && !context_contains(value, "fair value hierarchy")
                    && !context_contains(value, "derivative netting adjustments")
            });
        }
    }
}

fn prune_cost_of_goods_sold_context_mismatches(values: &mut Vec<NumericValue>) {
    if values.len() < 2 {
        return;
    }

    let has_three_month_candidate = values.iter().any(|value| {
        context_contains(value, "three months ended")
            && !context_contains(value, "(continued)")
    });
    if has_three_month_candidate {
        values.retain(|value| {
            !context_contains(value, "six months ended")
                && !context_contains(value, "nine months ended")
                && !context_contains(value, "(continued)")
        });
        if values.len() < 2 {
            return;
        }
    }

    let has_primary_statement_candidate = values.iter().any(|value| {
        statement_style_income_context(value) && !context_contains(value, "(continued)")
    });
    if has_primary_statement_candidate {
        values.retain(|value| !context_contains(value, "(continued)"));
    }
}

fn prune_annual_quarter_fragments(values: &mut Vec<NumericValue>) {
    if values.len() < 2 {
        return;
    }

    let Some(dominant_amount) =
        values.iter().map(|value| value.amount.abs()).max_by(|left, right| left.total_cmp(right))
    else {
        return;
    };

    let is_annual_html_group = values.iter().all(|value| {
        value.provenance.form_type == filing_models::FilingForm::Form10K
            && matches!(
                value.reporting_period.context,
                filing_models::PeriodContext::Instant { .. }
            )
    });

    if !is_annual_html_group || dominant_amount < 1000.0 {
        return;
    }

    let smaller_values =
        values.iter().filter(|value| value.amount.abs() < dominant_amount).collect::<Vec<_>>();
    if smaller_values.is_empty() {
        return;
    }

    // Some annual HTML tables repeat quarter fragments next to the full-year amount. When one
    // amount clearly dominates the group, keep the annual value on the statement sheet and leave
    // the extractor free to recover quarter values under their own real periods elsewhere.
    if smaller_values.iter().all(|value| value.amount.abs() <= dominant_amount * 0.4) {
        values.retain(|value| value.amount.abs() == dominant_amount);
    }
}

fn statement_style_income_context(value: &NumericValue) -> bool {
    context_contains(value, "year quarter")
        || context_contains(value, "millions, except per-share amounts")
        || context_contains(value, "statement of income")
        || context_contains(value, "statement of operations")
        || context_contains(value, "statement of earnings")
}

fn context_contains(value: &NumericValue, needle: &str) -> bool {
    [
        value.provenance.source_location.section_name.as_deref(),
        value.provenance.source_location.table_name.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|context| context.to_ascii_lowercase().contains(needle))
}

fn amounts_differ(left: f64, right: f64) -> bool {
    (left - right).abs() > 0.0001
}

fn source_values_for_reconciled_sources(
    xbrl_values: Vec<NumericValue>,
    html_values: Vec<NumericValue>,
    primary_source: NormalizationSource,
) -> Vec<NormalizedSourceValue> {
    let mut source_values = Vec::new();
    push_source_values(
        &mut source_values,
        xbrl_values,
        NormalizationSource::XbrlPrimary,
        primary_source == NormalizationSource::XbrlPrimary,
    );
    push_source_values(
        &mut source_values,
        html_values,
        NormalizationSource::HtmlFallback,
        primary_source == NormalizationSource::HtmlFallback,
    );
    source_values
}

fn source_values_for_single_source(
    values: Vec<NumericValue>,
    source: NormalizationSource,
) -> Vec<NormalizedSourceValue> {
    let mut source_values = Vec::new();
    push_source_values(&mut source_values, values, source, true);
    source_values
}

fn push_source_values(
    source_values: &mut Vec<NormalizedSourceValue>,
    values: Vec<NumericValue>,
    source: NormalizationSource,
    first_is_primary: bool,
) {
    for (index, value) in values.into_iter().enumerate() {
        let selected_as_primary = first_is_primary && index == 0;
        let review_note = match (selected_as_primary, index) {
            (true, _) => Some("selected primary value".to_string()),
            (false, 0) => Some("retained alternative source value".to_string()),
            (false, _) => Some("retained duplicate source value for review".to_string()),
        };

        source_values.push(NormalizedSourceValue {
            value,
            source,
            selected_as_primary,
            review_note,
        });
    }
}

fn normalize_narrative_metric(
    section: &ExtractedNarrativeSection,
) -> Option<NormalizedNarrativeMetric> {
    match &section.value {
        MetricValue::Text(text) => Some(NormalizedNarrativeMetric {
            metric_id: section.metric_id.clone(),
            domain: section.domain,
            value: text.clone(),
            primary_source: NormalizationSource::NarrativeHtml,
        }),
        MetricValue::Numeric(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filing_models::{
        FilingForm, FilingSourceMethod, FiscalPeriod, FiscalQuarter, MetricValue, PeriodContext,
        Provenance, ReportingPeriod, SignConvention, SourceLocator, SourceType, TextBlock,
        ValueScale,
    };
    use time::macros::date;

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

    fn sample_numeric_value(source_type: SourceType, amount: f64) -> NumericValue {
        let is_xbrl = matches!(source_type, SourceType::Xbrl);
        let source_method = match source_type {
            SourceType::Xbrl => FilingSourceMethod::ApiXbrlFacts,
            _ => FilingSourceMethod::FilingHtml,
        };

        NumericValue {
            amount,
            unit: filing_models::MeasurementUnit::Currency("USD".to_string()),
            scale: ValueScale::Raw,
            sign_convention: SignConvention::AsReported,
            label: Some("Revenue".to_string()),
            reporting_period: sample_reporting_period(),
            provenance: Provenance {
                accession_number: "0000798354-25-000010".to_string(),
                filing_url: Some("https://example.test/10k.htm".to_string()),
                form_type: FilingForm::Form10K,
                source_type,
                source_method,
                source_location: SourceLocator {
                    section_name: Some("test".to_string()),
                    table_name: Some("test".to_string()),
                    row_label: Some("Revenue".to_string()),
                    cell_reference: None,
                    segment_name: None,
                },
                xbrl_tag: if is_xbrl {
                    Some("RevenueFromContractWithCustomerExcludingAssessedTax".to_string())
                } else {
                    None
                },
                filing_label: Some("Revenue".to_string()),
                reporting_period: sample_reporting_period(),
                unit: filing_models::MeasurementUnit::Currency("USD".to_string()),
                scale: ValueScale::Raw,
            },
        }
    }

    fn sample_numeric_value_for_year(
        source_type: SourceType,
        amount: f64,
        year: i32,
    ) -> NumericValue {
        let mut value = sample_numeric_value(source_type, amount);
        let reporting_period = ReportingPeriod {
            context: PeriodContext::Instant {
                as_of: time::Date::from_calendar_date(year, time::Month::December, 31)
                    .expect("sample date should be valid"),
            },
            fiscal_period: Some(FiscalPeriod {
                fiscal_year: year,
                fiscal_quarter: Some(FiscalQuarter::Q4),
            }),
            label: Some("FY".to_string()),
        };
        value.reporting_period = reporting_period.clone();
        value.provenance.reporting_period = reporting_period;
        value
    }

    fn sample_duration_reporting_period(start_year: i32, end_year: i32) -> ReportingPeriod {
        ReportingPeriod {
            context: PeriodContext::Duration {
                start: time::Date::from_calendar_date(start_year, time::Month::January, 1)
                    .expect("sample start date should be valid"),
                end: time::Date::from_calendar_date(end_year, time::Month::December, 31)
                    .expect("sample end date should be valid"),
            },
            fiscal_period: Some(FiscalPeriod { fiscal_year: end_year, fiscal_quarter: None }),
            label: Some("FY".to_string()),
        }
    }

    fn sample_segment_value(
        source_type: SourceType,
        amount: f64,
        segment_name: &str,
        filing_label: &str,
        accession_number: &str,
    ) -> NumericValue {
        let is_xbrl = matches!(source_type, SourceType::Xbrl);
        let source_method = match source_type {
            SourceType::Xbrl => FilingSourceMethod::ApiXbrlFacts,
            _ => FilingSourceMethod::FilingHtml,
        };
        let reporting_period = sample_duration_reporting_period(2023, 2023);

        NumericValue {
            amount,
            unit: filing_models::MeasurementUnit::Currency("USD".to_string()),
            scale: ValueScale::Raw,
            sign_convention: SignConvention::AsReported,
            label: Some("Segment Revenue".to_string()),
            reporting_period: reporting_period.clone(),
            provenance: Provenance {
                accession_number: accession_number.to_string(),
                filing_url: Some("https://example.test/segment.htm".to_string()),
                form_type: FilingForm::Form10K,
                source_type,
                source_method,
                source_location: SourceLocator {
                    section_name: Some("Segment Note".to_string()),
                    table_name: Some("Segment Revenue".to_string()),
                    row_label: Some("Revenue".to_string()),
                    cell_reference: None,
                    segment_name: Some(segment_name.to_string()),
                },
                xbrl_tag: if is_xbrl { Some("Revenues".to_string()) } else { None },
                filing_label: Some(filing_label.to_string()),
                reporting_period,
                unit: filing_models::MeasurementUnit::Currency("USD".to_string()),
                scale: ValueScale::Raw,
            },
        }
    }

    fn set_reporting_period(value: &mut NumericValue, reporting_period: ReportingPeriod) {
        value.reporting_period = reporting_period.clone();
        value.provenance.reporting_period = reporting_period;
    }

    fn sample_derivative_gain_loss_value(
        amount: f64,
        accession_number: &str,
        form_type: FilingForm,
        start_year: i32,
        start_month: time::Month,
        start_day: u8,
        end_year: i32,
        end_month: time::Month,
        end_day: u8,
    ) -> NumericValue {
        let reporting_period = ReportingPeriod {
            context: PeriodContext::Duration {
                start: time::Date::from_calendar_date(start_year, start_month, start_day)
                    .expect("sample derivative start date should be valid"),
                end: time::Date::from_calendar_date(end_year, end_month, end_day)
                    .expect("sample derivative end date should be valid"),
            },
            fiscal_period: None,
            label: None,
        };

        let mut value = sample_numeric_value(SourceType::Html, amount);
        value.label = Some("Derivative Gain or Loss".to_string());
        value.reporting_period = reporting_period.clone();
        value.provenance.accession_number = accession_number.to_string();
        value.provenance.form_type = form_type;
        value.provenance.filing_label = Some("Derivative Gain or Loss".to_string());
        value.provenance.source_location.section_name = Some("inline_xbrl_derivative".to_string());
        value.provenance.source_location.table_name = Some("inline_xbrl_derivative".to_string());
        value.provenance.source_location.row_label = Some("Derivative Gain or Loss".to_string());
        value.provenance.reporting_period = reporting_period;
        value
    }

    fn sample_shares_outstanding_value(
        amount: f64,
        year: i32,
        accession_number: &str,
    ) -> NumericValue {
        let reporting_period = ReportingPeriod {
            context: PeriodContext::Instant {
                as_of: time::Date::from_calendar_date(year, time::Month::December, 31)
                    .expect("sample shares outstanding date should be valid"),
            },
            fiscal_period: Some(FiscalPeriod {
                fiscal_year: year,
                fiscal_quarter: Some(FiscalQuarter::Q4),
            }),
            label: Some("FY".to_string()),
        };

        NumericValue {
            amount,
            unit: filing_models::MeasurementUnit::Shares,
            scale: ValueScale::Raw,
            sign_convention: SignConvention::AsReported,
            label: Some("Shares Outstanding".to_string()),
            reporting_period: reporting_period.clone(),
            provenance: Provenance {
                accession_number: accession_number.to_string(),
                filing_url: Some("https://example.test/shares.htm".to_string()),
                form_type: FilingForm::Form10K,
                source_type: SourceType::Xbrl,
                source_method: FilingSourceMethod::ApiXbrlFacts,
                source_location: SourceLocator {
                    section_name: Some("shareholders_equity".to_string()),
                    table_name: Some("shareholders_equity".to_string()),
                    row_label: Some("Shares Outstanding".to_string()),
                    cell_reference: None,
                    segment_name: None,
                },
                xbrl_tag: Some("EntityCommonStockSharesOutstanding".to_string()),
                filing_label: Some("Shares Outstanding".to_string()),
                reporting_period,
                unit: filing_models::MeasurementUnit::Shares,
                scale: ValueScale::Raw,
            },
        }
    }

    fn sample_income_statement_value(
        metric_label: &str,
        amount: f64,
        xbrl_tag: &str,
    ) -> NumericValue {
        let mut value = sample_numeric_value(SourceType::Xbrl, amount);
        value.label = Some(metric_label.to_string());
        value.provenance.filing_label = Some(metric_label.to_string());
        value.provenance.source_location.row_label = Some(metric_label.to_string());
        value.provenance.xbrl_tag = Some(xbrl_tag.to_string());
        value
    }

    #[test]
    fn prefers_xbrl_but_keeps_html_alternative() {
        let normalizer = Normalizer::new();
        let xbrl_metrics = vec![ExtractedMetricValue {
            metric_id: MetricId::new("income_statement.revenue"),
            metric_name: "Revenue".to_string(),
            domain: DomainName::IncomeStatement,
            subdomain: Some("operating_results".to_string()),
            xbrl_tag: "RevenueFromContractWithCustomerExcludingAssessedTax".to_string(),
            numeric_value: sample_numeric_value(SourceType::Xbrl, 980.0),
        }];
        let html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![ExtractedHtmlMetricValue {
                metric_id: MetricId::new("income_statement.revenue"),
                metric_name: "Revenue".to_string(),
                domain: DomainName::IncomeStatement,
                subdomain: Some("operating_results".to_string()),
                numeric_value: sample_numeric_value(SourceType::Html, 970.0),
            }],
            narrative_sections: Vec::new(),
        };

        let normalized = normalizer.normalize(&xbrl_metrics, &html_result);
        assert_eq!(normalized.numeric_metrics.len(), 1);
        let metric = &normalized.numeric_metrics[0];
        assert_eq!(metric.primary_source, NormalizationSource::XbrlPrimary);
        assert_eq!(metric.decision, NormalizationDecision::PreferXbrlKeepHtmlAlternative);
        assert_eq!(metric.value.amount, 980.0);
        assert_eq!(metric.alternative_value.as_ref().map(|v| v.amount), Some(970.0));
        assert_eq!(metric.source_values.len(), 2);
        assert_eq!(normalized.issues.len(), 1);
    }

    #[test]
    fn keeps_same_metric_for_multiple_periods() {
        let normalizer = Normalizer::new();
        let xbrl_metrics = vec![
            ExtractedMetricValue {
                metric_id: MetricId::new("income_statement.revenue"),
                metric_name: "Revenue".to_string(),
                domain: DomainName::IncomeStatement,
                subdomain: Some("operating_results".to_string()),
                xbrl_tag: "RevenueFromContractWithCustomerExcludingAssessedTax".to_string(),
                numeric_value: sample_numeric_value_for_year(SourceType::Xbrl, 980.0, 2024),
            },
            ExtractedMetricValue {
                metric_id: MetricId::new("income_statement.revenue"),
                metric_name: "Revenue".to_string(),
                domain: DomainName::IncomeStatement,
                subdomain: Some("operating_results".to_string()),
                xbrl_tag: "RevenueFromContractWithCustomerExcludingAssessedTax".to_string(),
                numeric_value: sample_numeric_value_for_year(SourceType::Xbrl, 900.0, 2023),
            },
        ];

        let normalized = normalizer.normalize(&xbrl_metrics, &HtmlExtractionResult::default());

        assert_eq!(normalized.numeric_metrics.len(), 2);
        assert_eq!(normalized.numeric_metrics[0].period_key, "2023-12-31");
        assert_eq!(normalized.numeric_metrics[1].period_key, "2024-12-31");
    }

    #[test]
    fn falls_back_to_html_when_xbrl_is_missing() {
        let normalizer = Normalizer::new();
        let html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![ExtractedHtmlMetricValue {
                metric_id: MetricId::new("debt_and_credit.revolver_balance"),
                metric_name: "Revolver Balance".to_string(),
                domain: DomainName::DebtAndCredit,
                subdomain: Some("credit_facilities".to_string()),
                numeric_value: sample_numeric_value(SourceType::Html, 45.0),
            }],
            narrative_sections: Vec::new(),
        };

        let normalized = normalizer.normalize(&[], &html_result);
        assert_eq!(normalized.numeric_metrics.len(), 1);
        let metric = &normalized.numeric_metrics[0];
        assert_eq!(metric.primary_source, NormalizationSource::HtmlFallback);
        assert_eq!(metric.decision, NormalizationDecision::HtmlOnly);
        assert_eq!(metric.value.amount, 45.0);
    }

    #[test]
    fn identical_duplicate_source_values_do_not_create_warning_noise() {
        let normalizer = Normalizer::new();
        let xbrl_metrics = vec![
            ExtractedMetricValue {
                metric_id: MetricId::new("income_statement.revenue"),
                metric_name: "Revenue".to_string(),
                domain: DomainName::IncomeStatement,
                subdomain: Some("operating_results".to_string()),
                xbrl_tag: "RevenueFromContractWithCustomerExcludingAssessedTax".to_string(),
                numeric_value: sample_numeric_value(SourceType::Xbrl, 980.0),
            },
            ExtractedMetricValue {
                metric_id: MetricId::new("income_statement.revenue"),
                metric_name: "Revenue".to_string(),
                domain: DomainName::IncomeStatement,
                subdomain: Some("operating_results".to_string()),
                xbrl_tag: "RevenueFromContractWithCustomerExcludingAssessedTax".to_string(),
                numeric_value: sample_numeric_value(SourceType::Xbrl, 980.0),
            },
        ];

        let normalized = normalizer.normalize(&xbrl_metrics, &HtmlExtractionResult::default());

        assert_eq!(normalized.numeric_metrics.len(), 1);
        assert_eq!(normalized.numeric_metrics[0].source_values.len(), 1);
        assert!(normalized.issues.iter().all(|issue| issue.code != "duplicate_source_values"));
    }

    #[test]
    fn differing_duplicate_source_values_still_create_warning() {
        let normalizer = Normalizer::new();
        let xbrl_metrics = vec![
            ExtractedMetricValue {
                metric_id: MetricId::new("income_statement.revenue"),
                metric_name: "Revenue".to_string(),
                domain: DomainName::IncomeStatement,
                subdomain: Some("operating_results".to_string()),
                xbrl_tag: "RevenueFromContractWithCustomerExcludingAssessedTax".to_string(),
                numeric_value: sample_numeric_value(SourceType::Xbrl, 980.0),
            },
            ExtractedMetricValue {
                metric_id: MetricId::new("income_statement.revenue"),
                metric_name: "Revenue".to_string(),
                domain: DomainName::IncomeStatement,
                subdomain: Some("operating_results".to_string()),
                xbrl_tag: "RevenueFromContractWithCustomerExcludingAssessedTax".to_string(),
                numeric_value: sample_numeric_value(SourceType::Xbrl, 990.0),
            },
        ];

        let normalized = normalizer.normalize(&xbrl_metrics, &HtmlExtractionResult::default());

        assert!(normalized.issues.iter().any(|issue| issue.code == "duplicate_source_values"));
    }

    #[test]
    fn html_duplicate_source_values_rank_total_labels_before_weaker_rows() {
        let normalizer = Normalizer::new();
        let mut weak_value = sample_numeric_value(SourceType::Html, 1.0);
        weak_value.label = Some("Revenue".to_string());
        weak_value.provenance.filing_label = Some("Revenue".to_string());
        weak_value.provenance.source_location.row_label = Some("Revenue".to_string());

        let mut stronger_value = sample_numeric_value(SourceType::Html, 980.0);
        stronger_value.label = Some("Total revenue".to_string());
        stronger_value.provenance.filing_label = Some("Total revenue".to_string());
        stronger_value.provenance.source_location.row_label = Some("Total revenue".to_string());

        let html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("income_statement.revenue"),
                    metric_name: "Revenue".to_string(),
                    domain: DomainName::IncomeStatement,
                    subdomain: Some("operating_results".to_string()),
                    numeric_value: weak_value,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("income_statement.revenue"),
                    metric_name: "Revenue".to_string(),
                    domain: DomainName::IncomeStatement,
                    subdomain: Some("operating_results".to_string()),
                    numeric_value: stronger_value,
                },
            ],
            narrative_sections: Vec::new(),
        };

        let normalized = normalizer.normalize(&[], &html_result);

        assert_eq!(normalized.numeric_metrics[0].value.amount, 980.0);
        assert!(normalized.issues.iter().any(|issue| issue.code == "duplicate_source_values"));
    }

    #[test]
    fn html_duplicate_groups_drop_small_outlier_when_large_value_is_duplicated() {
        let normalizer = Normalizer::new();
        let mut main_left = sample_numeric_value(SourceType::Html, 511.0);
        main_left.label = Some("Revolving credit facility".to_string());
        main_left.provenance.filing_label = Some("Revolving credit facility".to_string());
        main_left.provenance.source_location.row_label =
            Some("Revolving credit facility".to_string());

        let mut main_right = sample_numeric_value(SourceType::Html, 511.0);
        main_right.label = Some("Revolving credit facility".to_string());
        main_right.provenance.filing_label = Some("Revolving credit facility".to_string());
        main_right.provenance.source_location.row_label =
            Some("Revolving credit facility".to_string());

        let mut outlier = sample_numeric_value(SourceType::Html, 1.18);
        outlier.label = Some("Revolving credit facility".to_string());
        outlier.provenance.filing_label = Some("Revolving credit facility".to_string());
        outlier.provenance.source_location.row_label =
            Some("Revolving credit facility".to_string());

        let html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("debt_and_credit.revolver_balance"),
                    metric_name: "Revolver Balance".to_string(),
                    domain: DomainName::DebtAndCredit,
                    subdomain: Some("credit_facilities".to_string()),
                    numeric_value: main_left,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("debt_and_credit.revolver_balance"),
                    metric_name: "Revolver Balance".to_string(),
                    domain: DomainName::DebtAndCredit,
                    subdomain: Some("credit_facilities".to_string()),
                    numeric_value: main_right,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("debt_and_credit.revolver_balance"),
                    metric_name: "Revolver Balance".to_string(),
                    domain: DomainName::DebtAndCredit,
                    subdomain: Some("credit_facilities".to_string()),
                    numeric_value: outlier,
                },
            ],
            narrative_sections: Vec::new(),
        };

        let normalized = normalizer.normalize(&[], &html_result);

        assert_eq!(normalized.numeric_metrics[0].value.amount, 511.0);
        assert!(normalized.issues.iter().all(|issue| issue.code != "duplicate_source_values"));
    }

    #[test]
    fn html_duplicate_groups_drop_small_artifacts_when_one_large_statement_value_dominates() {
        let normalizer = Normalizer::new();

        let mut dominant = sample_numeric_value(SourceType::Html, 4007.0);
        dominant.label = Some("Cost of sales".to_string());
        dominant.provenance.filing_label = Some("Cost of sales".to_string());
        dominant.provenance.source_location.row_label = Some("Cost of sales".to_string());

        let mut small_percent_like = sample_numeric_value(SourceType::Html, 51.2);
        small_percent_like.label = Some("Cost of sales".to_string());
        small_percent_like.provenance.filing_label = Some("Cost of sales".to_string());
        small_percent_like.provenance.source_location.row_label = Some("Cost of sales".to_string());

        let mut small_note_like = sample_numeric_value(SourceType::Html, 86.0);
        small_note_like.label = Some("Cost of sales".to_string());
        small_note_like.provenance.filing_label = Some("Cost of sales".to_string());
        small_note_like.provenance.source_location.row_label = Some("Cost of sales".to_string());

        let html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("income_statement.cost_of_goods_sold"),
                    metric_name: "Cost of Goods Sold".to_string(),
                    domain: DomainName::IncomeStatement,
                    subdomain: Some("operating_results".to_string()),
                    numeric_value: dominant,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("income_statement.cost_of_goods_sold"),
                    metric_name: "Cost of Goods Sold".to_string(),
                    domain: DomainName::IncomeStatement,
                    subdomain: Some("operating_results".to_string()),
                    numeric_value: small_percent_like,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("income_statement.cost_of_goods_sold"),
                    metric_name: "Cost of Goods Sold".to_string(),
                    domain: DomainName::IncomeStatement,
                    subdomain: Some("operating_results".to_string()),
                    numeric_value: small_note_like,
                },
            ],
            narrative_sections: Vec::new(),
        };

        let normalized = normalizer.normalize(&[], &html_result);

        assert_eq!(normalized.numeric_metrics[0].value.amount, 4007.0);
        assert!(normalized.issues.iter().all(|issue| issue.code != "duplicate_source_values"));
    }

    #[test]
    fn cost_of_goods_sold_discards_percent_of_sales_context_rows() {
        let normalizer = Normalizer::new();

        let mut primary_value = sample_numeric_value(SourceType::Html, 4613.0);
        primary_value.label = Some("Cost of sales".to_string());
        primary_value.provenance.filing_label = Some("Cost of sales".to_string());
        primary_value.provenance.source_location.row_label = Some("Cost of sales".to_string());
        primary_value.provenance.source_location.section_name =
            Some("Three months ended March 31".to_string());
        primary_value.provenance.source_location.table_name =
            Some("Three months ended March 31".to_string());

        let mut percent_context_value = sample_numeric_value(SourceType::Html, 57.4);
        percent_context_value.label = Some("Cost of sales".to_string());
        percent_context_value.provenance.filing_label = Some("Cost of sales".to_string());
        percent_context_value.provenance.source_location.row_label =
            Some("Cost of sales".to_string());
        percent_context_value.provenance.source_location.section_name =
            Some("Three months ended March 31, (Percent of net sales) Change".to_string());
        percent_context_value.provenance.source_location.table_name =
            Some("Three months ended March 31, (Percent of net sales) Change".to_string());

        let html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("income_statement.cost_of_goods_sold"),
                    metric_name: "Cost of Goods Sold".to_string(),
                    domain: DomainName::IncomeStatement,
                    subdomain: Some("operating_results".to_string()),
                    numeric_value: primary_value,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("income_statement.cost_of_goods_sold"),
                    metric_name: "Cost of Goods Sold".to_string(),
                    domain: DomainName::IncomeStatement,
                    subdomain: Some("operating_results".to_string()),
                    numeric_value: percent_context_value,
                },
            ],
            narrative_sections: Vec::new(),
        };

        let normalized = normalizer.normalize(&[], &html_result);

        assert_eq!(normalized.numeric_metrics[0].value.amount, 4613.0);
        assert_eq!(normalized.numeric_metrics[0].source_values.len(), 1);
        assert!(normalized.issues.iter().all(|issue| issue.code != "duplicate_source_values"));
    }

    #[test]
    fn cost_of_goods_sold_prefers_full_year_amount_over_quarter_fragments_in_annual_html_group() {
        let normalizer = Normalizer::new();

        let mut annual_value = sample_numeric_value(SourceType::Html, 16001.0);
        annual_value.label = Some("Cost of sales".to_string());
        annual_value.provenance.filing_label = Some("Cost of sales".to_string());
        annual_value.provenance.source_location.row_label = Some("Cost of sales".to_string());
        annual_value.provenance.source_location.section_name = Some(
            "(Millions, except per-share amounts) First Second Third Fourth Year Quarter Quarter Quarter Quarter"
                .to_string(),
        );
        annual_value.provenance.source_location.table_name =
            annual_value.provenance.source_location.section_name.clone();

        let mut quarter_fragment_left = annual_value.clone();
        quarter_fragment_left.amount = 3869.0;
        quarter_fragment_left.label = Some("Cost of sales".to_string());

        let mut quarter_fragment_right = annual_value.clone();
        quarter_fragment_right.amount = 3678.0;
        quarter_fragment_right.label = Some("Cost of sales".to_string());

        let html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("income_statement.cost_of_goods_sold"),
                    metric_name: "Cost of Goods Sold".to_string(),
                    domain: DomainName::IncomeStatement,
                    subdomain: Some("operating_results".to_string()),
                    numeric_value: annual_value,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("income_statement.cost_of_goods_sold"),
                    metric_name: "Cost of Goods Sold".to_string(),
                    domain: DomainName::IncomeStatement,
                    subdomain: Some("operating_results".to_string()),
                    numeric_value: quarter_fragment_left,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("income_statement.cost_of_goods_sold"),
                    metric_name: "Cost of Goods Sold".to_string(),
                    domain: DomainName::IncomeStatement,
                    subdomain: Some("operating_results".to_string()),
                    numeric_value: quarter_fragment_right,
                },
            ],
            narrative_sections: Vec::new(),
        };

        let normalized = normalizer.normalize(&[], &html_result);

        assert_eq!(normalized.numeric_metrics[0].value.amount, 16001.0);
        assert_eq!(normalized.numeric_metrics[0].source_values.len(), 1);
        assert!(normalized.issues.iter().all(|issue| issue.code != "duplicate_source_values"));
    }

    #[test]
    fn cost_of_goods_sold_prefers_three_month_statement_over_six_month_and_continued_rows() {
        let normalizer = Normalizer::new();

        let mut three_month = sample_numeric_value(SourceType::Html, 14_358.0);
        three_month.label = Some("Cost of goods sold".to_string());
        three_month.provenance.filing_label = Some("Cost of goods sold".to_string());
        three_month.provenance.source_location.row_label = Some("Cost of goods sold".to_string());
        three_month.provenance.source_location.section_name =
            Some("STATEMENT OF EARNINGS (LOSS) Three months ended June 30".to_string());
        three_month.provenance.source_location.table_name =
            three_month.provenance.source_location.section_name.clone();

        let mut six_month = three_month.clone();
        six_month.amount = 27_888.0;
        six_month.provenance.source_location.section_name =
            Some("STATEMENT OF EARNINGS (LOSS) Six months ended June 30".to_string());
        six_month.provenance.source_location.table_name =
            six_month.provenance.source_location.section_name.clone();

        let mut continued = three_month.clone();
        continued.amount = 27_983.0;
        continued.provenance.source_location.section_name =
            Some("STATEMENT OF EARNINGS (LOSS) (CONTINUED) Six months ended June 30".to_string());
        continued.provenance.source_location.table_name =
            continued.provenance.source_location.section_name.clone();

        let html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("income_statement.cost_of_goods_sold"),
                    metric_name: "Cost of Goods Sold".to_string(),
                    domain: DomainName::IncomeStatement,
                    subdomain: Some("operating_results".to_string()),
                    numeric_value: three_month,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("income_statement.cost_of_goods_sold"),
                    metric_name: "Cost of Goods Sold".to_string(),
                    domain: DomainName::IncomeStatement,
                    subdomain: Some("operating_results".to_string()),
                    numeric_value: six_month,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("income_statement.cost_of_goods_sold"),
                    metric_name: "Cost of Goods Sold".to_string(),
                    domain: DomainName::IncomeStatement,
                    subdomain: Some("operating_results".to_string()),
                    numeric_value: continued,
                },
            ],
            narrative_sections: Vec::new(),
        };

        let normalized = normalizer.normalize(&[], &html_result);

        assert_eq!(normalized.numeric_metrics[0].value.amount, 14_358.0);
        assert_eq!(normalized.numeric_metrics[0].source_values.len(), 1);
        assert!(normalized.issues.iter().all(|issue| issue.code != "duplicate_source_values"));
    }

    #[test]
    fn cost_of_goods_sold_prefers_primary_statement_over_continued_table() {
        let normalizer = Normalizer::new();

        let mut primary = sample_numeric_value(SourceType::Html, 50_244.0);
        primary.label = Some("Cost of goods sold".to_string());
        primary.provenance.filing_label = Some("Cost of goods sold".to_string());
        primary.provenance.source_location.row_label = Some("Cost of goods sold".to_string());
        primary.provenance.source_location.section_name =
            Some("STATEMENT OF EARNINGS (LOSS) Consolidated".to_string());
        primary.provenance.source_location.table_name =
            primary.provenance.source_location.section_name.clone();

        let mut continued = primary.clone();
        continued.amount = 50_265.0;
        continued.provenance.source_location.section_name =
            Some("STATEMENT OF EARNINGS (LOSS) (CONTINUED) GE(a) GE Capital".to_string());
        continued.provenance.source_location.table_name =
            continued.provenance.source_location.section_name.clone();

        let html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("income_statement.cost_of_goods_sold"),
                    metric_name: "Cost of Goods Sold".to_string(),
                    domain: DomainName::IncomeStatement,
                    subdomain: Some("operating_results".to_string()),
                    numeric_value: primary,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("income_statement.cost_of_goods_sold"),
                    metric_name: "Cost of Goods Sold".to_string(),
                    domain: DomainName::IncomeStatement,
                    subdomain: Some("operating_results".to_string()),
                    numeric_value: continued,
                },
            ],
            narrative_sections: Vec::new(),
        };

        let normalized = normalizer.normalize(&[], &html_result);

        assert_eq!(normalized.numeric_metrics[0].value.amount, 50_244.0);
        assert_eq!(normalized.numeric_metrics[0].source_values.len(), 1);
        assert!(normalized.issues.iter().all(|issue| issue.code != "duplicate_source_values"));
    }

    #[test]
    fn debt_interest_rate_prefers_summary_rows_over_individual_note_rows() {
        let normalizer = Normalizer::new();

        let mut summary_rate = sample_numeric_value(SourceType::Html, 3.07);
        summary_rate.label = Some("Fixed-rate debt".to_string());
        summary_rate.provenance.filing_label = Some("Fixed-rate debt".to_string());
        summary_rate.provenance.source_location.row_label = Some("Fixed-rate debt".to_string());

        let mut note_rate = sample_numeric_value(SourceType::Html, 2.02);
        note_rate.label = Some("Registered note ($ 750 million)".to_string());
        note_rate.provenance.filing_label = Some("Registered note ($ 750 million)".to_string());
        note_rate.provenance.source_location.row_label =
            Some("Registered note ($ 750 million)".to_string());

        let html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("debt_and_credit.interest_rate"),
                    metric_name: "Debt Interest Rate".to_string(),
                    domain: DomainName::DebtAndCredit,
                    subdomain: Some("rates".to_string()),
                    numeric_value: note_rate,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("debt_and_credit.interest_rate"),
                    metric_name: "Debt Interest Rate".to_string(),
                    domain: DomainName::DebtAndCredit,
                    subdomain: Some("rates".to_string()),
                    numeric_value: summary_rate,
                },
            ],
            narrative_sections: Vec::new(),
        };

        let normalized = normalizer.normalize(&[], &html_result);

        assert_eq!(normalized.numeric_metrics[0].value.amount, 3.07);
        assert!(normalized.issues.iter().any(|issue| issue.code == "duplicate_source_values"));
    }

    #[test]
    fn notes_and_bonds_prefers_larger_same_accession_total() {
        let normalizer = Normalizer::new();

        let mut smaller = sample_numeric_value(SourceType::Html, 14_762.0);
        smaller.label = Some("Senior notes".to_string());
        smaller.provenance.filing_label = Some("Senior notes".to_string());
        smaller.provenance.source_location.row_label = Some("Senior notes".to_string());
        smaller.provenance.accession_number = "0000040545-20-000009".to_string();

        let mut larger = smaller.clone();
        larger.amount = 25_371.0;

        let html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("debt_and_credit.notes_and_bonds"),
                    metric_name: "Notes and Bonds".to_string(),
                    domain: DomainName::DebtAndCredit,
                    subdomain: Some("funding_structure".to_string()),
                    numeric_value: smaller,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("debt_and_credit.notes_and_bonds"),
                    metric_name: "Notes and Bonds".to_string(),
                    domain: DomainName::DebtAndCredit,
                    subdomain: Some("funding_structure".to_string()),
                    numeric_value: larger,
                },
            ],
            narrative_sections: Vec::new(),
        };

        let normalized = normalizer.normalize(&[], &html_result);

        assert_eq!(normalized.numeric_metrics[0].value.amount, 25_371.0);
    }

    #[test]
    fn notes_and_bonds_same_accession_pair_stays_in_provenance_without_main_warning() {
        let normalizer = Normalizer::new();

        let mut smaller = sample_numeric_value(SourceType::Html, 14_762.0);
        smaller.label = Some("Senior notes".to_string());
        smaller.provenance.filing_label = Some("Senior notes".to_string());
        smaller.provenance.source_location.row_label = Some("Senior notes".to_string());
        smaller.provenance.accession_number = "0000040545-20-000009".to_string();

        let mut larger = smaller.clone();
        larger.amount = 25_371.0;

        let html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("debt_and_credit.notes_and_bonds"),
                    metric_name: "Notes and Bonds".to_string(),
                    domain: DomainName::DebtAndCredit,
                    subdomain: Some("funding_structure".to_string()),
                    numeric_value: smaller,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("debt_and_credit.notes_and_bonds"),
                    metric_name: "Notes and Bonds".to_string(),
                    domain: DomainName::DebtAndCredit,
                    subdomain: Some("funding_structure".to_string()),
                    numeric_value: larger,
                },
            ],
            narrative_sections: Vec::new(),
        };

        let normalized = normalizer.normalize(&[], &html_result);

        assert_eq!(normalized.numeric_metrics.len(), 1);
        assert_eq!(normalized.numeric_metrics[0].source_values.len(), 2);
        assert!(
            normalized.issues.iter().all(|issue| issue.code != "duplicate_source_values"),
            "same-accession debt note alternates should remain reviewable in provenance without main warning noise"
        );
    }

    #[test]
    fn same_accession_segment_alternates_stay_in_provenance_without_main_warning() {
        let normalizer = Normalizer::new();

        let mut dominant = sample_segment_value(
            SourceType::Html,
            86_789.0,
            "Industrial Segment",
            "2020 10-K",
            "0000040545-20-000009",
        );
        let period = sample_duration_reporting_period(2018, 2018);
        set_reporting_period(&mut dominant, period.clone());

        let mut alternate = dominant.clone();
        alternate.amount = 86_075.0;

        let html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("segment_data.segment_revenue"),
                    metric_name: "Segment Revenue".to_string(),
                    domain: DomainName::SegmentData,
                    subdomain: Some("segment_results".to_string()),
                    numeric_value: dominant,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("segment_data.segment_revenue"),
                    metric_name: "Segment Revenue".to_string(),
                    domain: DomainName::SegmentData,
                    subdomain: Some("segment_results".to_string()),
                    numeric_value: alternate,
                },
            ],
            narrative_sections: Vec::new(),
        };

        let normalized = normalizer.normalize(&[], &html_result);

        assert_eq!(normalized.numeric_metrics.len(), 1);
        assert_eq!(normalized.numeric_metrics[0].source_values.len(), 2);
        assert!(
            normalized.issues.iter().all(|issue| issue.code != "duplicate_source_values"),
            "same-accession segment alternates should remain reviewable in provenance without main warning noise"
        );
    }

    #[test]
    fn repeated_segment_value_sets_across_filings_do_not_create_main_warning_noise() {
        let normalizer = Normalizer::new();
        let period = sample_duration_reporting_period(2020, 2020);

        let mut first_primary = sample_segment_value(
            SourceType::Html,
            17_589.0,
            "Power Segment",
            "2022 10-K",
            "0000040545-22-000008",
        );
        set_reporting_period(&mut first_primary, period.clone());

        let mut first_alternate = first_primary.clone();
        first_alternate.amount = 17_237.0;

        let mut second_primary = sample_segment_value(
            SourceType::Html,
            17_589.0,
            "Power Segment",
            "2023 10-K",
            "0000040545-23-000023",
        );
        set_reporting_period(&mut second_primary, period.clone());

        let mut second_alternate = second_primary.clone();
        second_alternate.amount = 17_237.0;

        let html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("segment_data.segment_revenue"),
                    metric_name: "Segment Revenue".to_string(),
                    domain: DomainName::SegmentData,
                    subdomain: Some("segment_results".to_string()),
                    numeric_value: first_primary,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("segment_data.segment_revenue"),
                    metric_name: "Segment Revenue".to_string(),
                    domain: DomainName::SegmentData,
                    subdomain: Some("segment_results".to_string()),
                    numeric_value: first_alternate,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("segment_data.segment_revenue"),
                    metric_name: "Segment Revenue".to_string(),
                    domain: DomainName::SegmentData,
                    subdomain: Some("segment_results".to_string()),
                    numeric_value: second_primary,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("segment_data.segment_revenue"),
                    metric_name: "Segment Revenue".to_string(),
                    domain: DomainName::SegmentData,
                    subdomain: Some("segment_results".to_string()),
                    numeric_value: second_alternate,
                },
            ],
            narrative_sections: Vec::new(),
        };

        let normalized = normalizer.normalize(&[], &html_result);

        assert_eq!(normalized.numeric_metrics.len(), 1);
        assert_eq!(normalized.numeric_metrics[0].source_values.len(), 4);
        assert!(
            normalized.issues.iter().all(|issue| issue.code != "duplicate_source_values"),
            "repeated identical same-segment amount sets across later filings should remain reviewable in provenance without main warning noise"
        );
    }

    #[test]
    fn identical_same_accession_revenue_values_collapse_before_warning() {
        let normalizer = Normalizer::new();

        let mut first = sample_numeric_value(SourceType::Html, 28_831.0);
        first.label = Some("Revenue".to_string());
        first.provenance.filing_label = Some("Revenue".to_string());
        first.provenance.source_location.row_label = Some("Revenue".to_string());
        first.provenance.accession_number = "0000040545-19-000053".to_string();
        first.provenance.xbrl_tag = Some("us-gaap:Revenues".to_string());
        set_reporting_period(
            &mut first,
            ReportingPeriod {
                context: PeriodContext::Duration {
                    start: date!(2019 - 04 - 01),
                    end: date!(2019 - 06 - 30),
                },
                fiscal_period: None,
                label: None,
            },
        );

        let second = first.clone();
        let third = first.clone();

        let html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("income_statement.revenue"),
                    metric_name: "Revenue".to_string(),
                    domain: DomainName::IncomeStatement,
                    subdomain: Some("operating_results".to_string()),
                    numeric_value: first,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("income_statement.revenue"),
                    metric_name: "Revenue".to_string(),
                    domain: DomainName::IncomeStatement,
                    subdomain: Some("operating_results".to_string()),
                    numeric_value: second,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("income_statement.revenue"),
                    metric_name: "Revenue".to_string(),
                    domain: DomainName::IncomeStatement,
                    subdomain: Some("operating_results".to_string()),
                    numeric_value: third,
                },
            ],
            narrative_sections: Vec::new(),
        };

        let normalized = normalizer.normalize(&[], &html_result);

        assert_eq!(normalized.numeric_metrics.len(), 1);
        assert_eq!(normalized.numeric_metrics[0].source_values.len(), 1);
        assert!(
            normalized.issues.iter().all(|issue| issue.code != "duplicate_source_values"),
            "identical same-accession revenue values should collapse before warning generation"
        );
    }

    #[test]
    fn long_term_debt_prefers_balance_sheet_context_over_fair_value_context() {
        let normalizer = Normalizer::new();

        let mut balance_sheet_value = sample_numeric_value(SourceType::Html, 292224.0);
        balance_sheet_value.label = Some("Long-term debt".to_string());
        balance_sheet_value.provenance.filing_label =
            Some("Selected Consolidated balance sheets data".to_string());
        balance_sheet_value.provenance.source_location.section_name =
            Some("Selected Consolidated balance sheets data".to_string());
        balance_sheet_value.provenance.source_location.table_name =
            Some("Long-term debt".to_string());
        balance_sheet_value.provenance.source_location.row_label =
            Some("Long-term debt".to_string());

        let mut fair_value_value = sample_numeric_value(SourceType::Html, 31394.0);
        fair_value_value.label = Some("Long-term debt".to_string());
        fair_value_value.provenance.filing_label =
            Some("Assets and liabilities measured at fair value on a recurring basis".to_string());
        fair_value_value.provenance.source_location.section_name =
            Some("Assets and liabilities measured at fair value on a recurring basis".to_string());
        fair_value_value.provenance.source_location.table_name =
            Some("Long-term debt".to_string());
        fair_value_value.provenance.source_location.row_label =
            Some("Long-term debt".to_string());

        let html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("balance_sheet.long_term_debt"),
                    metric_name: "Long-Term Debt".to_string(),
                    domain: DomainName::BalanceSheet,
                    subdomain: Some("liabilities".to_string()),
                    numeric_value: fair_value_value,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("balance_sheet.long_term_debt"),
                    metric_name: "Long-Term Debt".to_string(),
                    domain: DomainName::BalanceSheet,
                    subdomain: Some("liabilities".to_string()),
                    numeric_value: balance_sheet_value,
                },
            ],
            narrative_sections: Vec::new(),
        };

        let normalized = normalizer.normalize(&[], &html_result);

        assert_eq!(normalized.numeric_metrics[0].value.amount, 292224.0);
        assert_eq!(
            normalized.numeric_metrics[0]
                .value
                .provenance
                .source_location
                .section_name
                .as_deref(),
            Some("Selected Consolidated balance sheets data")
        );
    }

    #[test]
    fn later_financial_funding_repeats_stay_in_review_without_main_duplicate_warning() {
        let normalizer = Normalizer::new();

        let mut filing_2023 = sample_numeric_value_for_year(SourceType::Html, 21483.0, 2023);
        filing_2023.label = Some("Senior notes".to_string());
        filing_2023.provenance.accession_number = "000001961724000225".to_string();
        filing_2023.provenance.form_type = FilingForm::Form10K;
        filing_2023.provenance.filing_label =
            Some("Long-term unsecured funding Year ended December 31,".to_string());
        filing_2023.provenance.source_location.section_name =
            Some("Long-term unsecured funding Year ended December 31,".to_string());
        filing_2023.provenance.source_location.table_name = Some("Senior notes".to_string());
        filing_2023.provenance.source_location.row_label = Some("Senior notes".to_string());

        let mut filing_2024_q1 = sample_numeric_value_for_year(SourceType::Html, 21483.0, 2023);
        filing_2024_q1.label = Some("Senior notes".to_string());
        filing_2024_q1.provenance.accession_number = "000001961724000301".to_string();
        filing_2024_q1.provenance.form_type = FilingForm::Form10Q;
        filing_2024_q1.provenance.filing_label =
            Some("Long-term unsecured funding Year ended December 31,".to_string());
        filing_2024_q1.provenance.source_location.section_name =
            Some("Long-term unsecured funding Year ended December 31,".to_string());
        filing_2024_q1.provenance.source_location.table_name = Some("Senior notes".to_string());
        filing_2024_q1.provenance.source_location.row_label = Some("Senior notes".to_string());

        let html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("debt_and_credit.notes_and_bonds"),
                    metric_name: "Notes and Bonds".to_string(),
                    domain: DomainName::DebtAndCredit,
                    subdomain: Some("funding".to_string()),
                    numeric_value: filing_2024_q1,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("debt_and_credit.notes_and_bonds"),
                    metric_name: "Notes and Bonds".to_string(),
                    domain: DomainName::DebtAndCredit,
                    subdomain: Some("funding".to_string()),
                    numeric_value: filing_2023,
                },
            ],
            narrative_sections: Vec::new(),
        };

        let normalized = normalizer.normalize(&[], &html_result);

        assert_eq!(normalized.numeric_metrics[0].source_values.len(), 2);
        assert!(normalized.issues.iter().all(|issue| issue.code != "duplicate_source_values"));
    }

    #[test]
    fn derivative_gain_loss_prefers_nearest_filing_window_and_drops_later_history_repeats() {
        let normalizer = Normalizer::new();

        let html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("derivatives_and_securities.derivative_gain_loss"),
                    metric_name: "Derivative Gain or Loss".to_string(),
                    domain: DomainName::DerivativesAndSecurities,
                    subdomain: Some("derivatives".to_string()),
                    numeric_value: sample_derivative_gain_loss_value(
                        173.0,
                        "0000066740-24-000016",
                        FilingForm::Form10K,
                        2022,
                        time::Month::January,
                        1,
                        2022,
                        time::Month::December,
                        31,
                    ),
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("derivatives_and_securities.derivative_gain_loss"),
                    metric_name: "Derivative Gain or Loss".to_string(),
                    domain: DomainName::DerivativesAndSecurities,
                    subdomain: Some("derivatives".to_string()),
                    numeric_value: sample_derivative_gain_loss_value(
                        197.0,
                        "0000066740-25-000010",
                        FilingForm::Form10K,
                        2022,
                        time::Month::January,
                        1,
                        2022,
                        time::Month::December,
                        31,
                    ),
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("derivatives_and_securities.derivative_gain_loss"),
                    metric_name: "Derivative Gain or Loss".to_string(),
                    domain: DomainName::DerivativesAndSecurities,
                    subdomain: Some("derivatives".to_string()),
                    numeric_value: sample_derivative_gain_loss_value(
                        221.0,
                        "0000066740-26-000014",
                        FilingForm::Form10K,
                        2022,
                        time::Month::January,
                        1,
                        2022,
                        time::Month::December,
                        31,
                    ),
                },
            ],
            narrative_sections: Vec::new(),
        };

        let normalized = normalizer.normalize(&[], &html_result);

        assert_eq!(normalized.numeric_metrics.len(), 1);
        assert_eq!(normalized.numeric_metrics[0].value.amount, 173.0);
        assert_eq!(normalized.numeric_metrics[0].source_values.len(), 1);
        assert!(
            normalized.issues.iter().all(|issue| issue.code != "duplicate_source_values"),
            "later comparative derivative history should not create duplicate warnings"
        );
    }

    #[test]
    fn derives_net_change_shares_outstanding_from_consecutive_share_counts() {
        let normalizer = Normalizer::new();
        let xbrl_metrics = vec![
            ExtractedMetricValue {
                metric_id: MetricId::new("shareholders_equity.shares_outstanding"),
                metric_name: "Shares Outstanding".to_string(),
                domain: DomainName::ShareholdersEquity,
                subdomain: Some("capital_accounts".to_string()),
                xbrl_tag: "EntityCommonStockSharesOutstanding".to_string(),
                numeric_value: sample_shares_outstanding_value(
                    1000.0,
                    2023,
                    "0000000000-24-000001",
                ),
            },
            ExtractedMetricValue {
                metric_id: MetricId::new("shareholders_equity.shares_outstanding"),
                metric_name: "Shares Outstanding".to_string(),
                domain: DomainName::ShareholdersEquity,
                subdomain: Some("capital_accounts".to_string()),
                xbrl_tag: "EntityCommonStockSharesOutstanding".to_string(),
                numeric_value: sample_shares_outstanding_value(950.0, 2024, "0000000000-25-000001"),
            },
        ];

        let normalized = normalizer.normalize(&xbrl_metrics, &HtmlExtractionResult::default());

        let derived = normalized
            .numeric_metrics
            .iter()
            .find(|metric| {
                metric.metric_id.as_str() == "equity_compensation.net_change_shares_outstanding"
            })
            .expect("derived net change shares outstanding should exist");

        assert_eq!(derived.value.amount, -50.0);
        assert_eq!(
            derived.value.provenance.xbrl_tag.as_deref(),
            Some("derived_from_shares_outstanding_delta")
        );
        assert_eq!(
            derived.source_values[0].review_note.as_deref(),
            Some("derived from consecutive shareholders_equity.shares_outstanding values")
        );
    }

    #[test]
    fn derives_gross_profit_from_revenue_minus_cost_of_goods_sold() {
        let normalizer = Normalizer::new();
        let xbrl_metrics = vec![
            ExtractedMetricValue {
                metric_id: MetricId::new("income_statement.revenue"),
                metric_name: "Revenue".to_string(),
                domain: DomainName::IncomeStatement,
                subdomain: Some("operating_results".to_string()),
                xbrl_tag: "Revenues".to_string(),
                numeric_value: sample_income_statement_value("Revenue", 1000.0, "Revenues"),
            },
            ExtractedMetricValue {
                metric_id: MetricId::new("income_statement.cost_of_goods_sold"),
                metric_name: "Cost of Goods Sold".to_string(),
                domain: DomainName::IncomeStatement,
                subdomain: Some("operating_results".to_string()),
                xbrl_tag: "CostOfSales".to_string(),
                numeric_value: sample_income_statement_value("Cost of sales", 600.0, "CostOfSales"),
            },
        ];

        let normalized = normalizer.normalize(&xbrl_metrics, &HtmlExtractionResult::default());

        let derived = normalized
            .numeric_metrics
            .iter()
            .find(|metric| metric.metric_id.as_str() == "income_statement.gross_profit")
            .expect("derived gross profit should exist");

        assert_eq!(derived.value.amount, 400.0);
        assert_eq!(
            derived.value.provenance.xbrl_tag.as_deref(),
            Some("derived_from_revenue_minus_cogs")
        );
        assert_eq!(
            derived.source_values[0].review_note.as_deref(),
            Some("derived from revenue minus cost_of_goods_sold")
        );
    }

    #[test]
    fn derives_gross_profit_when_revenue_and_cogs_share_only_period_end_date() {
        let normalizer = Normalizer::new();

        let mut revenue_value = sample_income_statement_value("Revenue", 1000.0, "Revenues");
        revenue_value.reporting_period = ReportingPeriod {
            context: PeriodContext::Duration {
                start: date!(2024 - 01 - 01),
                end: date!(2024 - 12 - 31),
            },
            fiscal_period: None,
            label: None,
        };
        revenue_value.provenance.reporting_period = revenue_value.reporting_period.clone();

        let mut cogs_value = sample_income_statement_value("Cost of sales", 600.0, "CostOfSales");
        cogs_value.reporting_period = ReportingPeriod {
            context: PeriodContext::Instant { as_of: date!(2024 - 12 - 31) },
            fiscal_period: Some(FiscalPeriod {
                fiscal_year: 2024,
                fiscal_quarter: Some(FiscalQuarter::Q4),
            }),
            label: Some("FY".to_string()),
        };
        cogs_value.provenance.reporting_period = cogs_value.reporting_period.clone();

        let xbrl_metrics = vec![
            ExtractedMetricValue {
                metric_id: MetricId::new("income_statement.revenue"),
                metric_name: "Revenue".to_string(),
                domain: DomainName::IncomeStatement,
                subdomain: Some("operating_results".to_string()),
                xbrl_tag: "Revenues".to_string(),
                numeric_value: revenue_value,
            },
            ExtractedMetricValue {
                metric_id: MetricId::new("income_statement.cost_of_goods_sold"),
                metric_name: "Cost of Goods Sold".to_string(),
                domain: DomainName::IncomeStatement,
                subdomain: Some("operating_results".to_string()),
                xbrl_tag: "CostOfSales".to_string(),
                numeric_value: cogs_value,
            },
        ];

        let normalized = normalizer.normalize(&xbrl_metrics, &HtmlExtractionResult::default());
        let derived = normalized
            .numeric_metrics
            .iter()
            .find(|metric| metric.metric_id.as_str() == "income_statement.gross_profit")
            .expect("derived gross profit should exist");

        assert_eq!(derived.value.amount, 400.0);
    }

    #[test]
    fn segment_metrics_with_different_segment_names_stay_separate() {
        let normalizer = Normalizer::new();

        let mut consumer_value = sample_numeric_value(SourceType::Xbrl, 600.0);
        consumer_value.provenance.source_location.segment_name =
            Some("Consumer Segment".to_string());

        let mut industrial_value = sample_numeric_value(SourceType::Xbrl, 400.0);
        industrial_value.provenance.source_location.segment_name =
            Some("Industrial Segment".to_string());

        let xbrl_metrics = vec![
            ExtractedMetricValue {
                metric_id: MetricId::new("segment_data.segment_revenue"),
                metric_name: "Segment Revenue".to_string(),
                domain: DomainName::SegmentData,
                subdomain: Some("segment_results".to_string()),
                xbrl_tag: "RevenueFromExternalCustomersByReportableSegment".to_string(),
                numeric_value: consumer_value,
            },
            ExtractedMetricValue {
                metric_id: MetricId::new("segment_data.segment_revenue"),
                metric_name: "Segment Revenue".to_string(),
                domain: DomainName::SegmentData,
                subdomain: Some("segment_results".to_string()),
                xbrl_tag: "RevenueFromExternalCustomersByReportableSegment".to_string(),
                numeric_value: industrial_value,
            },
        ];

        let normalized = normalizer.normalize(&xbrl_metrics, &HtmlExtractionResult::default());

        assert_eq!(normalized.numeric_metrics.len(), 2);
        let mut segment_names: Vec<_> = normalized
            .numeric_metrics
            .iter()
            .map(|metric| {
                metric
                    .value
                    .provenance
                    .source_location
                    .segment_name
                    .clone()
                    .expect("segment name should be preserved")
            })
            .collect();
        segment_names.sort();
        assert_eq!(segment_names, vec!["Consumer Segment", "Industrial Segment"]);
    }

    #[test]
    fn segment_label_variants_canonicalize_into_one_metric_row() {
        let normalizer = Normalizer::new();
        let xbrl_metrics = vec![
            ExtractedMetricValue {
                metric_id: MetricId::new("segment_data.segment_revenue"),
                metric_name: "Segment Revenue".to_string(),
                domain: DomainName::SegmentData,
                subdomain: Some("segment_results".to_string()),
                xbrl_tag: "Revenues".to_string(),
                numeric_value: sample_segment_value(
                    SourceType::Xbrl,
                    100.0,
                    "Healthcare Segment",
                    "2024 10-K",
                    "0000000000-24-000001",
                ),
            },
            ExtractedMetricValue {
                metric_id: MetricId::new("segment_data.segment_revenue"),
                metric_name: "Segment Revenue".to_string(),
                domain: DomainName::SegmentData,
                subdomain: Some("segment_results".to_string()),
                xbrl_tag: "Revenues".to_string(),
                numeric_value: sample_segment_value(
                    SourceType::Xbrl,
                    100.0,
                    "Healthc Care Segment",
                    "2025 10-K",
                    "0000000000-25-000001",
                ),
            },
        ];

        let normalized = normalizer.normalize(&xbrl_metrics, &HtmlExtractionResult::default());

        assert_eq!(normalized.numeric_metrics.len(), 1);
        let metric = &normalized.numeric_metrics[0];
        assert_eq!(
            metric.value.provenance.source_location.segment_name.as_deref(),
            Some("Healthcare Segment")
        );
        assert_eq!(metric.source_values.len(), 2);
        assert!(
            normalized.issues.iter().all(|issue| issue.code != "duplicate_source_values"),
            "identical segment history rows with normalized labels should not warn"
        );
    }

    #[test]
    fn identical_segment_history_from_later_filings_does_not_warn() {
        let normalizer = Normalizer::new();
        let xbrl_metrics = vec![
            ExtractedMetricValue {
                metric_id: MetricId::new("segment_data.segment_revenue"),
                metric_name: "Segment Revenue".to_string(),
                domain: DomainName::SegmentData,
                subdomain: Some("segment_results".to_string()),
                xbrl_tag: "Revenues".to_string(),
                numeric_value: sample_segment_value(
                    SourceType::Xbrl,
                    250.0,
                    "Aerospace Segment",
                    "2024 10-K",
                    "0000000000-24-000001",
                ),
            },
            ExtractedMetricValue {
                metric_id: MetricId::new("segment_data.segment_revenue"),
                metric_name: "Segment Revenue".to_string(),
                domain: DomainName::SegmentData,
                subdomain: Some("segment_results".to_string()),
                xbrl_tag: "Revenues".to_string(),
                numeric_value: sample_segment_value(
                    SourceType::Xbrl,
                    250.0,
                    "Aerospace Segment",
                    "2025 10-K",
                    "0000000000-25-000001",
                ),
            },
        ];

        let normalized = normalizer.normalize(&xbrl_metrics, &HtmlExtractionResult::default());

        assert_eq!(normalized.numeric_metrics.len(), 1);
        assert_eq!(normalized.numeric_metrics[0].source_values.len(), 2);
        assert!(
            normalized.issues.iter().all(|issue| issue.code != "duplicate_source_values"),
            "repeated identical segment history across later filings should stay in provenance without warning"
        );
    }

    #[test]
    fn ignores_segment_values_without_segment_name() {
        let normalizer = Normalizer::new();
        let named_segment = ExtractedHtmlMetricValue {
            metric_id: MetricId::new("segment_data.segment_revenue"),
            metric_name: "Segment Revenue".to_string(),
            domain: DomainName::SegmentData,
            subdomain: Some("segment_results".to_string()),
            numeric_value: sample_segment_value(
                SourceType::Html,
                250.0,
                "Consumer Segment",
                "2024 10-Q",
                "0000000000-24-000001",
            ),
        };
        let mut unnamed_value = sample_segment_value(
            SourceType::Html,
            999.0,
            "Consumer Segment",
            "2024 10-Q",
            "0000000000-24-000001",
        );
        unnamed_value.provenance.source_location.segment_name = None;
        unnamed_value.provenance.source_location.section_name = None;
        unnamed_value.provenance.source_location.table_name = None;
        unnamed_value.provenance.source_location.row_label = Some("Segment Revenue".to_string());

        let html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![
                named_segment,
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("segment_data.segment_revenue"),
                    metric_name: "Segment Revenue".to_string(),
                    domain: DomainName::SegmentData,
                    subdomain: Some("segment_results".to_string()),
                    numeric_value: unnamed_value,
                },
            ],
            narrative_sections: Vec::new(),
        };

        let normalized = normalizer.normalize(&[], &html_result);

        assert_eq!(normalized.numeric_metrics.len(), 1);
        assert_eq!(
            normalized.numeric_metrics[0].value.provenance.source_location.segment_name.as_deref(),
            Some("Consumer Segment")
        );
        assert_eq!(normalized.numeric_metrics[0].source_values.len(), 1);
    }

    #[test]
    fn segment_history_prefers_original_filing_window_over_later_comparatives() {
        let normalizer = Normalizer::new();
        let historical_period = sample_duration_reporting_period(2019, 2019);
        let mut filing_2021 = sample_segment_value(
            SourceType::Html,
            5151.0,
            "Consumer Segment",
            "2021 10-K",
            "0001558370-21-000737",
        );
        set_reporting_period(&mut filing_2021, historical_period.clone());
        let mut filing_2022 = sample_segment_value(
            SourceType::Html,
            5129.0,
            "Consumer Segment",
            "2022 10-K",
            "0000066740-22-000010",
        );
        set_reporting_period(&mut filing_2022, historical_period.clone());
        let mut filing_2020 = sample_segment_value(
            SourceType::Html,
            5089.0,
            "Consumer Segment",
            "2020 10-K",
            "0001558370-20-000581",
        );
        set_reporting_period(&mut filing_2020, historical_period);
        let html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("segment_data.segment_revenue"),
                    metric_name: "Segment Revenue".to_string(),
                    domain: DomainName::SegmentData,
                    subdomain: Some("segment_results".to_string()),
                    numeric_value: filing_2021,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("segment_data.segment_revenue"),
                    metric_name: "Segment Revenue".to_string(),
                    domain: DomainName::SegmentData,
                    subdomain: Some("segment_results".to_string()),
                    numeric_value: filing_2022,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("segment_data.segment_revenue"),
                    metric_name: "Segment Revenue".to_string(),
                    domain: DomainName::SegmentData,
                    subdomain: Some("segment_results".to_string()),
                    numeric_value: filing_2020,
                },
            ],
            narrative_sections: Vec::new(),
        };

        let normalized = normalizer.normalize(&[], &html_result);
        let metric = &normalized.numeric_metrics[0];

        assert_eq!(metric.value.amount, 5089.0);
        assert_eq!(metric.value.provenance.accession_number, "0001558370-20-000581");
    }

    #[test]
    fn later_segment_history_repeats_stay_in_review_without_main_duplicate_warning() {
        let normalizer = Normalizer::new();
        let historical_period = sample_duration_reporting_period(2019, 2019);
        let mut filing_2021 = sample_segment_value(
            SourceType::Html,
            5151.0,
            "Consumer Segment",
            "2021 10-K",
            "0001558370-21-000737",
        );
        set_reporting_period(&mut filing_2021, historical_period.clone());
        let mut filing_2022 = sample_segment_value(
            SourceType::Html,
            5129.0,
            "Consumer Segment",
            "2022 10-K",
            "0000066740-22-000010",
        );
        set_reporting_period(&mut filing_2022, historical_period.clone());
        let mut filing_2020 = sample_segment_value(
            SourceType::Html,
            5089.0,
            "Consumer Segment",
            "2020 10-K",
            "0001558370-20-000581",
        );
        set_reporting_period(&mut filing_2020, historical_period);
        let html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("segment_data.segment_revenue"),
                    metric_name: "Segment Revenue".to_string(),
                    domain: DomainName::SegmentData,
                    subdomain: Some("segment_results".to_string()),
                    numeric_value: filing_2021,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("segment_data.segment_revenue"),
                    metric_name: "Segment Revenue".to_string(),
                    domain: DomainName::SegmentData,
                    subdomain: Some("segment_results".to_string()),
                    numeric_value: filing_2022,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("segment_data.segment_revenue"),
                    metric_name: "Segment Revenue".to_string(),
                    domain: DomainName::SegmentData,
                    subdomain: Some("segment_results".to_string()),
                    numeric_value: filing_2020,
                },
            ],
            narrative_sections: Vec::new(),
        };

        let normalized = normalizer.normalize(&[], &html_result);

        assert_eq!(normalized.numeric_metrics.len(), 1);
        assert_eq!(normalized.numeric_metrics[0].source_values.len(), 3);
        assert!(
            normalized.issues.iter().all(|issue| issue.code != "duplicate_source_values"),
            "later comparative segment history should stay in provenance without cluttering the main review sheet"
        );
    }

    #[test]
    fn identical_segment_values_from_same_accession_are_collapsed_before_warning() {
        let normalizer = Normalizer::new();
        let html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("segment_data.segment_revenue"),
                    metric_name: "Segment Revenue".to_string(),
                    domain: DomainName::SegmentData,
                    subdomain: Some("segment_results".to_string()),
                    numeric_value: sample_segment_value(
                        SourceType::Html,
                        5089.0,
                        "Consumer Segment",
                        "2020 10-K",
                        "0001558370-20-000581",
                    ),
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("segment_data.segment_revenue"),
                    metric_name: "Segment Revenue".to_string(),
                    domain: DomainName::SegmentData,
                    subdomain: Some("segment_results".to_string()),
                    numeric_value: sample_segment_value(
                        SourceType::Html,
                        5089.0,
                        "Consumer Segment",
                        "2020 10-K",
                        "0001558370-20-000581",
                    ),
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("segment_data.segment_revenue"),
                    metric_name: "Segment Revenue".to_string(),
                    domain: DomainName::SegmentData,
                    subdomain: Some("segment_results".to_string()),
                    numeric_value: sample_segment_value(
                        SourceType::Html,
                        5089.0,
                        "Consumer Segment",
                        "2020 10-K",
                        "0001558370-20-000581",
                    ),
                },
            ],
            narrative_sections: Vec::new(),
        };

        let normalized = normalizer.normalize(&[], &html_result);

        assert_eq!(normalized.numeric_metrics.len(), 1);
        assert_eq!(normalized.numeric_metrics[0].source_values.len(), 1);
        assert!(
            normalized.issues.iter().all(|issue| issue.code != "duplicate_source_values"),
            "identical same-accession segment facts should collapse before review warnings"
        );
    }

    #[test]
    fn inline_xbrl_segment_aggregate_total_suppresses_component_warning_noise() {
        let normalizer = Normalizer::new();
        let period = sample_duration_reporting_period(2017, 2017);

        let mut total = sample_segment_value(
            SourceType::Html,
            9070.0,
            "Capital Segment",
            "2020 10-K",
            "0000040545-20-000009",
        );
        set_reporting_period(&mut total, period.clone());
        total.provenance.source_location.section_name = Some("inline_xbrl_segment".to_string());
        total.provenance.source_location.table_name = Some("inline_xbrl_segment".to_string());

        let mut component_one = sample_segment_value(
            SourceType::Html,
            1558.0,
            "Capital Segment",
            "2020 10-K",
            "0000040545-20-000009",
        );
        set_reporting_period(&mut component_one, period.clone());
        component_one.provenance.source_location.section_name =
            Some("inline_xbrl_segment".to_string());
        component_one.provenance.source_location.table_name =
            Some("inline_xbrl_segment".to_string());

        let mut component_two = sample_segment_value(
            SourceType::Html,
            7512.0,
            "Capital Segment",
            "2020 10-K",
            "0000040545-20-000009",
        );
        set_reporting_period(&mut component_two, period);
        component_two.provenance.source_location.section_name =
            Some("inline_xbrl_segment".to_string());
        component_two.provenance.source_location.table_name =
            Some("inline_xbrl_segment".to_string());

        let html_result = HtmlExtractionResult {
            numeric_fallbacks: vec![
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("segment_data.segment_revenue"),
                    metric_name: "Segment Revenue".to_string(),
                    domain: DomainName::SegmentData,
                    subdomain: Some("segment_results".to_string()),
                    numeric_value: component_one,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("segment_data.segment_revenue"),
                    metric_name: "Segment Revenue".to_string(),
                    domain: DomainName::SegmentData,
                    subdomain: Some("segment_results".to_string()),
                    numeric_value: component_two,
                },
                ExtractedHtmlMetricValue {
                    metric_id: MetricId::new("segment_data.segment_revenue"),
                    metric_name: "Segment Revenue".to_string(),
                    domain: DomainName::SegmentData,
                    subdomain: Some("segment_results".to_string()),
                    numeric_value: total,
                },
            ],
            narrative_sections: Vec::new(),
        };

        let normalized = normalizer.normalize(&[], &html_result);
        let metric = &normalized.numeric_metrics[0];

        assert_eq!(metric.value.amount, 9070.0);
        assert!(
            normalized.issues.iter().all(|issue| issue.code != "duplicate_source_values"),
            "aggregate total plus inline xbrl component rows should not create duplicate warning noise"
        );
    }

    #[test]
    fn keeps_narrative_sections_under_their_domain_only() {
        let normalizer = Normalizer::new();
        let html_result = HtmlExtractionResult {
            numeric_fallbacks: Vec::new(),
            narrative_sections: vec![
                ExtractedNarrativeSection {
                    metric_id: MetricId::new("footnotes.disclosure_text"),
                    domain: DomainName::Footnotes,
                    value: MetricValue::Text(TextBlock {
                        title: "Note 1".to_string(),
                        content: "Footnote text".to_string(),
                        form_type: FilingForm::Form10K,
                        filing_date: date!(2025 - 02 - 01),
                        source_type: SourceType::Html,
                        source_location: SourceLocator {
                            section_name: Some("Note 1".to_string()),
                            table_name: None,
                            row_label: None,
                            cell_reference: None,
                            segment_name: None,
                        },
                        associated_domain: Some("footnotes".to_string()),
                    }),
                },
                ExtractedNarrativeSection {
                    metric_id: MetricId::new("mda.management_discussion_text"),
                    domain: DomainName::Mda,
                    value: MetricValue::Text(TextBlock {
                        title: "MD&A".to_string(),
                        content: "MDA text".to_string(),
                        form_type: FilingForm::Form10K,
                        filing_date: date!(2025 - 02 - 01),
                        source_type: SourceType::Html,
                        source_location: SourceLocator {
                            section_name: Some("MD&A".to_string()),
                            table_name: None,
                            row_label: None,
                            cell_reference: None,
                            segment_name: None,
                        },
                        associated_domain: Some("mda".to_string()),
                    }),
                },
            ],
        };

        let normalized = normalizer.normalize(&[], &html_result);
        assert_eq!(normalized.narrative_metrics.len(), 2);
        assert_eq!(normalized.narrative_metrics[0].domain, DomainName::Footnotes);
        assert_eq!(normalized.narrative_metrics[1].domain, DomainName::Mda);
    }
}
