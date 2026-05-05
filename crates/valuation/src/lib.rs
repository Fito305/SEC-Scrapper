//! Placeholder valuation functions.
//!
//! These functions are intentionally written for readability first because they are expected to be
//! edited later. The current math is temporary. The important part right now is that:
//!
//! 1. valuation stays separate from SEC retrieval and parsing
//! 2. the program has a real place where valuation is called from later
//! 3. each placeholder formula returns structured output with provenance-friendly inputs
//!
//! When you replace the formulas later, start with:
//!
//! - `owners_earnings_placeholder`
//! - `adjusted_earnings_ratio_placeholder`
//! - `compute_placeholder_outputs`
//!
//! `compute_placeholder_outputs` is the top-level entry point intended to be called by later CLI,
//! export, and application orchestration code.

use accounting_domains::{DomainName, MetricId};
use filing_models::{
    FilingSourceMethod, MeasurementUnit, NumericValue, Provenance, ReportingPeriod, SignConvention,
    SourceLocator, SourceType, ValueScale,
};
use normalization::{NormalizationResult, NormalizedNumericMetric};
use std::collections::BTreeSet;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ValuationError {
    #[error("required normalized metric was missing: {metric_id}")]
    MissingMetric { metric_id: String },
    #[error("placeholder valuation inputs were invalid: {reason}")]
    InvalidInput { reason: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValuationInputBreakdown {
    pub metric_id: MetricId,
    pub metric_name: String,
    pub amount: f64,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValuationOutput {
    pub metric_id: MetricId,
    pub metric_name: String,
    pub domain: DomainName,
    pub value: NumericValue,
    pub inputs: Vec<ValuationInputBreakdown>,
    pub comment: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OwnersEarningsPlaceholderInputs {
    pub net_income: ValuationInputBreakdown,
    pub depreciation_and_amortization: ValuationInputBreakdown,
    pub capital_expenditures: ValuationInputBreakdown,
    pub additional_working_capital_needed: f64,
    pub treasury_yield: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AdjustedEarningsRatioPlaceholderInputs {
    pub net_income: ValuationInputBreakdown,
    pub stock_based_comp_expense: ValuationInputBreakdown,
    pub stock_comp_tax_effects: Option<ValuationInputBreakdown>,
    pub stock_repurchases: Option<ValuationInputBreakdown>,
    pub shares_repurchased: Option<ValuationInputBreakdown>,
    pub net_change_shares_outstanding: Option<ValuationInputBreakdown>,
}

#[derive(Debug, Default)]
pub struct ValuationEngine;

impl ValuationEngine {
    pub fn new() -> Self {
        Self
    }

    /// This is the main orchestration hook for later CLI/export integration.
    ///
    /// The current implementation intentionally produces the two placeholder metrics required by the
    /// spec. Replace the formula internals later, but keep this function as the point where
    /// normalized data becomes valuation outputs.
    ///
    /// Segment rows are intentionally excluded from these placeholder formulas for now. When you
    /// decide to build segment-aware valuation logic later, add a dedicated helper that filters
    /// `normalized.numeric_metrics` down to `DomainName::SegmentData` and the exact segment names
    /// you want to model. Keeping that work in a separate helper preserves the current
    /// company-level formulas while making the later segment-specific entry point obvious.
    pub fn compute_placeholder_outputs(
        &self,
        normalized: &NormalizationResult,
    ) -> Result<Vec<ValuationOutput>, ValuationError> {
        let period_keys: BTreeSet<String> =
            normalized.numeric_metrics.iter().map(|metric| metric.period_key.clone()).collect();

        if period_keys.is_empty() {
            return Err(ValuationError::InvalidInput {
                reason: "no normalized numeric periods are available for valuation".to_string(),
            });
        }

        let mut outputs = Vec::new();
        let mut first_missing_input = None;

        for period_key in period_keys {
            match build_owners_earnings_inputs(normalized, &period_key) {
                Ok(inputs) => outputs.push(owners_earnings_placeholder(inputs)?),
                Err(error) => {
                    first_missing_input.get_or_insert(error);
                }
            }

            match build_adjusted_earnings_inputs(normalized, &period_key) {
                Ok(inputs) => outputs.push(adjusted_earnings_ratio_placeholder(inputs)?),
                Err(error) => {
                    first_missing_input.get_or_insert(error);
                }
            }
        }

        match outputs.is_empty() {
            true => Err(first_missing_input.unwrap_or_else(|| ValuationError::InvalidInput {
                reason: "no placeholder valuation formulas could be computed".to_string(),
            })),
            false => Ok(outputs),
        }
    }
}

/// Placeholder owner's earnings formula.
///
/// Replace this function later with your real formula. The current calculation is intentionally
/// simple so the rest of the pipeline can be wired and exported now.
///
/// Current temporary formula:
/// `(net_income + depreciation_and_amortization - capex - working_capital_needed) / treasury_yield`
///
/// Notes for future edits:
///
/// - This function is expected to stay pure and testable.
/// - The caller is `ValuationEngine::compute_placeholder_outputs`.
/// - The output metric ID should stay stable unless you intentionally change the workbook schema.
pub fn owners_earnings_placeholder(
    inputs: OwnersEarningsPlaceholderInputs,
) -> Result<ValuationOutput, ValuationError> {
    match inputs.treasury_yield {
        value if value <= 0.0 => Err(ValuationError::InvalidInput {
            reason: "treasury_yield must be greater than zero".to_string(),
        }),
        treasury_yield => {
            let result = (inputs.net_income.amount + inputs.depreciation_and_amortization.amount
                - inputs.capital_expenditures.amount
                - inputs.additional_working_capital_needed)
                / treasury_yield;

            let reporting_period = inputs.net_income.provenance.reporting_period.clone();
            let output_provenance = valuation_output_provenance(
                "valuation.owners_earnings_placeholder",
                &reporting_period,
            );

            Ok(ValuationOutput {
                metric_id: MetricId::new("valuation.owners_earnings_placeholder"),
                metric_name: "Owner's Earnings Placeholder".to_string(),
                domain: DomainName::Valuation,
                value: NumericValue {
                    amount: result,
                    unit: MeasurementUnit::Currency("USD".to_string()),
                    scale: ValueScale::Raw,
                    sign_convention: SignConvention::AsReported,
                    label: Some("Owner's Earnings Placeholder".to_string()),
                    reporting_period: reporting_period.clone(),
                    provenance: output_provenance,
                },
                inputs: vec![
                    inputs.net_income,
                    inputs.depreciation_and_amortization,
                    inputs.capital_expenditures,
                ],
                comment: "Temporary formula. Replace in owners_earnings_placeholder when you finalize the rule.",
            })
        }
    }
}

/// Placeholder adjusted earnings ratio formula.
///
/// Replace this function later with your real formula. The current implementation uses `Result`
/// plus explicit `match` arms so the expected edge-case edit points are visible.
///
/// Current temporary formula:
/// `(net_income + stock_based_comp - tax_effects - buyback_adjustment) / net_income`
///
/// `buyback_adjustment` is also temporary and intentionally readable rather than clever.
pub fn adjusted_earnings_ratio_placeholder(
    inputs: AdjustedEarningsRatioPlaceholderInputs,
) -> Result<ValuationOutput, ValuationError> {
    let net_income = inputs.net_income.amount;

    match net_income {
        value if value == 0.0 => Err(ValuationError::InvalidInput {
            reason: "net income cannot be zero for the adjusted earnings ratio placeholder"
                .to_string(),
        }),
        _ => {
            let tax_effects = match &inputs.stock_comp_tax_effects {
                Some(metric) => metric.amount,
                None => 0.0,
            };

            let buyback_adjustment = match (
                &inputs.stock_repurchases,
                &inputs.shares_repurchased,
                &inputs.net_change_shares_outstanding,
            ) {
                (
                    Some(stock_repurchases),
                    Some(shares_repurchased),
                    Some(net_change_shares_outstanding),
                ) => match shares_repurchased.amount {
                    shares if shares == 0.0 => 0.0,
                    shares => {
                        stock_repurchases.amount
                            * ((shares + net_change_shares_outstanding.amount) / shares)
                    }
                },
                _ => 0.0,
            };

            let numerator = net_income + inputs.stock_based_comp_expense.amount
                - tax_effects
                - buyback_adjustment;
            let result = numerator / net_income;

            let reporting_period = inputs.net_income.provenance.reporting_period.clone();
            let output_provenance = valuation_output_provenance(
                "valuation.adjusted_earnings_ratio_placeholder",
                &reporting_period,
            );

            let mut output_inputs = vec![inputs.net_income, inputs.stock_based_comp_expense];
            if let Some(metric) = inputs.stock_comp_tax_effects {
                output_inputs.push(metric);
            }
            if let Some(metric) = inputs.stock_repurchases {
                output_inputs.push(metric);
            }
            if let Some(metric) = inputs.shares_repurchased {
                output_inputs.push(metric);
            }
            if let Some(metric) = inputs.net_change_shares_outstanding {
                output_inputs.push(metric);
            }

            Ok(ValuationOutput {
                metric_id: MetricId::new("valuation.adjusted_earnings_ratio_placeholder"),
                metric_name: "Adjusted Earnings Ratio Placeholder".to_string(),
                domain: DomainName::Valuation,
                value: NumericValue {
                    amount: result,
                    unit: MeasurementUnit::Ratio,
                    scale: ValueScale::Raw,
                    sign_convention: SignConvention::AsReported,
                    label: Some("Adjusted Earnings Ratio Placeholder".to_string()),
                    reporting_period: reporting_period.clone(),
                    provenance: output_provenance,
                },
                inputs: output_inputs,
                comment: "Temporary formula. Replace in adjusted_earnings_ratio_placeholder when you finalize the rule.",
            })
        }
    }
}

fn build_owners_earnings_inputs(
    normalized: &NormalizationResult,
    period_key: &str,
) -> Result<OwnersEarningsPlaceholderInputs, ValuationError> {
    Ok(OwnersEarningsPlaceholderInputs {
        net_income: find_required_metric(normalized, "income_statement.net_income", period_key)?,
        depreciation_and_amortization: find_required_metric(
            normalized,
            "cash_flow.depreciation_and_amortization",
            period_key,
        )?,
        capital_expenditures: find_required_metric(
            normalized,
            "cash_flow.capital_expenditures",
            period_key,
        )?,
        // This is intentionally a plain number for now so you can change it later without
        // unraveling the current type graph.
        additional_working_capital_needed: 0.0,
        // This is a temporary stub until the separate Treasury-yield provider is implemented.
        treasury_yield: 0.04,
    })
}

fn build_adjusted_earnings_inputs(
    normalized: &NormalizationResult,
    period_key: &str,
) -> Result<AdjustedEarningsRatioPlaceholderInputs, ValuationError> {
    Ok(AdjustedEarningsRatioPlaceholderInputs {
        net_income: find_required_metric(normalized, "income_statement.net_income", period_key)?,
        stock_based_comp_expense: find_required_metric(
            normalized,
            "equity_compensation.stock_based_comp_expense",
            period_key,
        )?,
        stock_comp_tax_effects: find_optional_metric(
            normalized,
            "equity_compensation.tax_effects",
            period_key,
        ),
        stock_repurchases: find_optional_metric(
            normalized,
            "cash_flow.stock_repurchases",
            period_key,
        ),
        shares_repurchased: find_optional_metric(
            normalized,
            "equity_compensation.shares_repurchased",
            period_key,
        ),
        net_change_shares_outstanding: find_optional_metric(
            normalized,
            "equity_compensation.net_change_shares_outstanding",
            period_key,
        ),
    })
}

fn find_required_metric(
    normalized: &NormalizationResult,
    metric_id: &str,
    period_key: &str,
) -> Result<ValuationInputBreakdown, ValuationError> {
    normalized
        .numeric_metrics
        .iter()
        .find(|metric| metric.metric_id.as_str() == metric_id && metric.period_key == period_key)
        .map(to_breakdown)
        .ok_or_else(|| ValuationError::MissingMetric {
            metric_id: format!("{metric_id} for period {period_key}"),
        })
}

fn find_optional_metric(
    normalized: &NormalizationResult,
    metric_id: &str,
    period_key: &str,
) -> Option<ValuationInputBreakdown> {
    normalized
        .numeric_metrics
        .iter()
        .find(|metric| metric.metric_id.as_str() == metric_id && metric.period_key == period_key)
        .map(to_breakdown)
}

fn to_breakdown(metric: &NormalizedNumericMetric) -> ValuationInputBreakdown {
    ValuationInputBreakdown {
        metric_id: metric.metric_id.clone(),
        metric_name: metric.metric_name.clone(),
        amount: metric.value.amount,
        provenance: metric.value.provenance.clone(),
    }
}

fn valuation_output_provenance(metric_id: &str, reporting_period: &ReportingPeriod) -> Provenance {
    Provenance {
        accession_number: format!("derived:{metric_id}"),
        filing_url: None,
        form_type: filing_models::FilingForm::Other("DERIVED".to_string()),
        source_type: SourceType::WorkbookImport,
        source_method: FilingSourceMethod::WorkbookImport,
        source_location: SourceLocator {
            section_name: Some("valuation".to_string()),
            table_name: Some("valuation".to_string()),
            row_label: Some(metric_id.to_string()),
            cell_reference: None,
            segment_name: None,
        },
        xbrl_tag: None,
        filing_label: Some(metric_id.to_string()),
        reporting_period: reporting_period.clone(),
        unit: MeasurementUnit::Other("derived".to_string()),
        scale: ValueScale::Raw,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filing_models::{FilingForm, FiscalPeriod, FiscalQuarter, PeriodContext, SourceType};
    use normalization::{
        NormalizationDecision, NormalizationSource, NormalizedNarrativeMetric,
        NormalizedNumericMetric, NormalizedSourceValue,
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

    fn sample_provenance(metric_id: &str, source_type: SourceType) -> Provenance {
        let source_method = match source_type {
            SourceType::Xbrl => FilingSourceMethod::ApiXbrlFacts,
            _ => FilingSourceMethod::FilingHtml,
        };

        Provenance {
            accession_number: "0000798354-25-000010".to_string(),
            filing_url: Some("https://example.test/10k.htm".to_string()),
            form_type: FilingForm::Form10K,
            source_type,
            source_method,
            source_location: SourceLocator {
                section_name: Some("valuation-test".to_string()),
                table_name: Some("valuation-test".to_string()),
                row_label: Some(metric_id.to_string()),
                cell_reference: None,
                segment_name: None,
            },
            xbrl_tag: None,
            filing_label: Some(metric_id.to_string()),
            reporting_period: sample_reporting_period(),
            unit: MeasurementUnit::Currency("USD".to_string()),
            scale: ValueScale::Raw,
        }
    }

    fn sample_normalized_metric(
        metric_id: &str,
        metric_name: &str,
        amount: f64,
    ) -> NormalizedNumericMetric {
        let value = NumericValue {
            amount,
            unit: MeasurementUnit::Currency("USD".to_string()),
            scale: ValueScale::Raw,
            sign_convention: SignConvention::AsReported,
            label: Some(metric_name.to_string()),
            reporting_period: sample_reporting_period(),
            provenance: sample_provenance(metric_id, SourceType::Xbrl),
        };

        NormalizedNumericMetric {
            metric_id: MetricId::new(metric_id),
            period_key: "2024-12-31".to_string(),
            domain: DomainName::IncomeStatement,
            metric_name: metric_name.to_string(),
            subdomain: None,
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

    fn sample_normalization_result() -> NormalizationResult {
        NormalizationResult {
            numeric_metrics: vec![
                sample_normalized_metric("income_statement.net_income", "Net Income", 100.0),
                sample_normalized_metric(
                    "cash_flow.depreciation_and_amortization",
                    "Depreciation and Amortization",
                    25.0,
                ),
                sample_normalized_metric(
                    "cash_flow.capital_expenditures",
                    "Capital Expenditures",
                    15.0,
                ),
                sample_normalized_metric(
                    "equity_compensation.stock_based_comp_expense",
                    "Stock-Based Compensation Expense",
                    8.0,
                ),
                sample_normalized_metric("equity_compensation.tax_effects", "Tax Effects", 2.0),
                sample_normalized_metric("cash_flow.stock_repurchases", "Stock Repurchases", 20.0),
                sample_normalized_metric(
                    "equity_compensation.shares_repurchased",
                    "Shares Repurchased",
                    4.0,
                ),
                sample_normalized_metric(
                    "equity_compensation.net_change_shares_outstanding",
                    "Net Change Shares Outstanding",
                    1.0,
                ),
            ],
            narrative_metrics: Vec::<NormalizedNarrativeMetric>::new(),
            issues: Vec::new(),
        }
    }

    #[test]
    fn computes_both_placeholder_outputs_from_normalized_data() {
        let engine = ValuationEngine::new();
        let outputs = engine
            .compute_placeholder_outputs(&sample_normalization_result())
            .expect("placeholder valuation should compute");

        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].metric_id.as_str(), "valuation.owners_earnings_placeholder");
        assert_eq!(outputs[1].metric_id.as_str(), "valuation.adjusted_earnings_ratio_placeholder");
    }

    #[test]
    fn owners_earnings_placeholder_uses_result_for_invalid_yield() {
        let error = owners_earnings_placeholder(OwnersEarningsPlaceholderInputs {
            net_income: ValuationInputBreakdown {
                metric_id: MetricId::new("income_statement.net_income"),
                metric_name: "Net Income".to_string(),
                amount: 100.0,
                provenance: sample_provenance("income_statement.net_income", SourceType::Xbrl),
            },
            depreciation_and_amortization: ValuationInputBreakdown {
                metric_id: MetricId::new("cash_flow.depreciation_and_amortization"),
                metric_name: "Depreciation".to_string(),
                amount: 10.0,
                provenance: sample_provenance(
                    "cash_flow.depreciation_and_amortization",
                    SourceType::Xbrl,
                ),
            },
            capital_expenditures: ValuationInputBreakdown {
                metric_id: MetricId::new("cash_flow.capital_expenditures"),
                metric_name: "Capex".to_string(),
                amount: 5.0,
                provenance: sample_provenance("cash_flow.capital_expenditures", SourceType::Xbrl),
            },
            additional_working_capital_needed: 0.0,
            treasury_yield: 0.0,
        })
        .expect_err("zero treasury yield should error");

        assert!(matches!(error, ValuationError::InvalidInput { .. }));
    }

    #[test]
    fn adjusted_earnings_ratio_placeholder_handles_zero_net_income() {
        let error = adjusted_earnings_ratio_placeholder(AdjustedEarningsRatioPlaceholderInputs {
            net_income: ValuationInputBreakdown {
                metric_id: MetricId::new("income_statement.net_income"),
                metric_name: "Net Income".to_string(),
                amount: 0.0,
                provenance: sample_provenance("income_statement.net_income", SourceType::Xbrl),
            },
            stock_based_comp_expense: ValuationInputBreakdown {
                metric_id: MetricId::new("equity_compensation.stock_based_comp_expense"),
                metric_name: "SBC".to_string(),
                amount: 10.0,
                provenance: sample_provenance(
                    "equity_compensation.stock_based_comp_expense",
                    SourceType::Xbrl,
                ),
            },
            stock_comp_tax_effects: None,
            stock_repurchases: None,
            shares_repurchased: None,
            net_change_shares_outstanding: None,
        })
        .expect_err("zero net income should error");

        assert!(matches!(error, ValuationError::InvalidInput { .. }));
    }
}
