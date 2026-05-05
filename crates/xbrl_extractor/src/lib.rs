//! XBRL-first extraction from SEC company facts payloads.
//!
//! This extractor is intentionally registry-driven. It matches concepts through the canonical
//! metric registry so future mapping changes stay in one place.

use accounting_domains::{DomainMetric, MetricId, MetricRegistry};
use filing_models::{
    FilingMetadata, FilingSourceMethod, MeasurementUnit, NumericValue, PeriodContext, Provenance,
    ReportingPeriod, SignConvention, SourceLocator, SourceType, ValueScale,
};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use thiserror::Error;
use time::{Date, format_description::well_known::Iso8601};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractedMetricValue {
    pub metric_id: MetricId,
    pub metric_name: String,
    pub domain: accounting_domains::DomainName,
    pub subdomain: Option<String>,
    pub xbrl_tag: String,
    pub numeric_value: NumericValue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompanyFactsResponse {
    #[serde(deserialize_with = "deserialize_cik_str")]
    pub cik: String,
    #[serde(rename = "entityName")]
    pub entity_name: String,
    pub facts: HashMap<String, HashMap<String, CompanyFactConcept>>,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompanyFactConcept {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub units: HashMap<String, Vec<CompanyFactUnitValue>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompanyFactUnitValue {
    #[serde(default)]
    pub end: Option<String>,
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub val: Option<f64>,
    #[serde(default)]
    pub accn: Option<String>,
    #[serde(default)]
    pub fy: Option<i32>,
    #[serde(default)]
    pub fp: Option<String>,
    #[serde(default)]
    pub form: Option<String>,
    #[serde(default)]
    pub filed: Option<String>,
    #[serde(default)]
    pub frame: Option<String>,
}

#[derive(Debug, Error)]
pub enum XbrlExtractionError {
    #[error("failed to parse SEC company facts JSON: {0}")]
    InvalidPayload(String),
    #[error("invalid XBRL date value: {value}")]
    InvalidDate { value: String },
}

#[derive(Debug, Clone)]
pub struct XbrlExtractor {
    registry: MetricRegistry,
}

impl Default for XbrlExtractor {
    fn default() -> Self {
        Self::new(MetricRegistry::default())
    }
}

impl XbrlExtractor {
    pub fn new(registry: MetricRegistry) -> Self {
        Self { registry }
    }

    pub fn registry(&self) -> &MetricRegistry {
        &self.registry
    }

    pub fn parse_company_facts_json(
        &self,
        payload: &str,
    ) -> Result<CompanyFactsResponse, XbrlExtractionError> {
        serde_json::from_str(payload)
            .map_err(|error| XbrlExtractionError::InvalidPayload(error.to_string()))
    }

    pub fn extract_for_filings(
        &self,
        company_facts: &CompanyFactsResponse,
        filings: &[FilingMetadata],
    ) -> Result<Vec<ExtractedMetricValue>, XbrlExtractionError> {
        let mut extracted = Vec::new();

        for filing in filings {
            for metric in self.registry.all() {
                extracted.extend(self.extract_metric_for_filing(company_facts, filing, metric)?);
            }
        }

        extracted.sort_by(|left, right| {
            left.metric_id.as_str().cmp(right.metric_id.as_str()).then_with(|| {
                left.numeric_value
                    .provenance
                    .accession_number
                    .cmp(&right.numeric_value.provenance.accession_number)
            })
        });

        Ok(extracted)
    }

    fn extract_metric_for_filing(
        &self,
        company_facts: &CompanyFactsResponse,
        filing: &FilingMetadata,
        metric: &DomainMetric,
    ) -> Result<Vec<ExtractedMetricValue>, XbrlExtractionError> {
        for tag in metric
            .definition
            .preferred_xbrl_tags
            .iter()
            .chain(metric.definition.alternate_xbrl_tags.iter())
        {
            let matches = find_facts_for_tag(company_facts, tag, filing, metric);
            if !matches.is_empty() {
                let mut extracted = Vec::new();

                for (concept, unit_name, fact) in matches {
                    let reporting_period = reporting_period_from_fact(fact)?;
                    let unit = measurement_unit_from_company_facts_unit(unit_name);
                    let filing_label = concept.label.clone().unwrap_or_else(|| tag.clone());
                    let amount = match fact.val {
                        Some(amount) => amount,
                        None => continue,
                    };
                    let provenance = Provenance {
                        accession_number: filing.accession_number.clone(),
                        filing_url: filing.filing_urls.primary_document.clone(),
                        form_type: filing.form_type.clone(),
                        source_type: SourceType::Xbrl,
                        source_method: FilingSourceMethod::ApiXbrlFacts,
                        source_location: SourceLocator {
                            section_name: metric.definition.statement.map(statement_name_label),
                            table_name: Some(metric.definition.domain.sheet_name().to_string()),
                            row_label: Some(metric.definition.display_name.clone()),
                            // SEC companyfacts can retain segment/member context only in the
                            // frame string. Preserve it when available so downstream workbook rows
                            // can stay separated by segment instead of collapsing into one metric.
                            cell_reference: fact.frame.clone(),
                            segment_name: extract_segment_name_from_frame(fact.frame.as_deref()),
                        },
                        xbrl_tag: Some(tag.clone()),
                        filing_label: Some(filing_label.clone()),
                        reporting_period: reporting_period.clone(),
                        unit: unit.clone(),
                        scale: ValueScale::Raw,
                    };

                    extracted.push(ExtractedMetricValue {
                        metric_id: metric.definition.metric_id.clone(),
                        metric_name: metric.definition.display_name.clone(),
                        domain: metric.definition.domain,
                        subdomain: metric.subdomain.clone(),
                        xbrl_tag: tag.clone(),
                        numeric_value: NumericValue {
                            amount,
                            unit,
                            scale: ValueScale::Raw,
                            sign_convention: SignConvention::AsReported,
                            label: Some(filing_label),
                            reporting_period,
                            provenance,
                        },
                    });
                }

                return Ok(extracted);
            }
        }

        Ok(Vec::new())
    }
}

fn find_facts_for_tag<'a>(
    company_facts: &'a CompanyFactsResponse,
    tag: &str,
    filing: &FilingMetadata,
    metric: &DomainMetric,
) -> Vec<(&'a CompanyFactConcept, &'a str, &'a CompanyFactUnitValue)> {
    for taxonomy in company_facts.facts.values() {
        if let Some(concept) = taxonomy.get(tag) {
            let mut matches = Vec::new();
            for (unit_name, facts) in &concept.units {
                let selected =
                    if metric.definition.domain == accounting_domains::DomainName::SegmentData {
                        select_best_facts_by_segment(facts, filing)
                    } else {
                        select_best_fact(facts, filing).into_iter().collect()
                    };

                for fact in selected {
                    matches.push((concept, unit_name.as_str(), fact));
                }
            }

            if !matches.is_empty() {
                return matches;
            }
        }
    }

    Vec::new()
}

fn select_best_fact<'a>(
    facts: &'a [CompanyFactUnitValue],
    filing: &FilingMetadata,
) -> Option<&'a CompanyFactUnitValue> {
    matching_facts(facts, filing).into_iter().next()
}

fn select_best_facts_by_segment<'a>(
    facts: &'a [CompanyFactUnitValue],
    filing: &FilingMetadata,
) -> Vec<&'a CompanyFactUnitValue> {
    let mut selected_by_segment: HashMap<String, &'a CompanyFactUnitValue> = HashMap::new();

    for fact in matching_facts(facts, filing) {
        let key = extract_segment_name_from_frame(fact.frame.as_deref())
            .or_else(|| fact.frame.clone())
            .unwrap_or_else(|| "__default__".to_string());
        selected_by_segment.entry(key).or_insert(fact);
    }

    let mut selected: Vec<_> = selected_by_segment.into_values().collect();
    selected.sort_by(|left, right| {
        extract_segment_name_from_frame(left.frame.as_deref())
            .cmp(&extract_segment_name_from_frame(right.frame.as_deref()))
    });
    selected
}

fn matching_facts<'a>(
    facts: &'a [CompanyFactUnitValue],
    filing: &FilingMetadata,
) -> Vec<&'a CompanyFactUnitValue> {
    let mut matching: Vec<&CompanyFactUnitValue> =
        facts.iter().filter(|fact| fact_match_score(fact, filing).is_some()).collect();

    matching.sort_by(|left, right| {
        fact_match_score(right, filing).cmp(&fact_match_score(left, filing)).then_with(|| {
            let left_filed = left.filed.as_deref().unwrap_or_default();
            let right_filed = right.filed.as_deref().unwrap_or_default();
            right_filed.cmp(left_filed).then_with(|| right.end.as_deref().cmp(&left.end.as_deref()))
        })
    });
    matching
}

fn fact_match_score(fact: &CompanyFactUnitValue, filing: &FilingMetadata) -> Option<u8> {
    let accession_matches =
        fact.accn.as_deref().map(|accn| accn == filing.accession_number).unwrap_or(false);

    let form_matches =
        fact.form.as_deref().map(|form| form == filing.form_type.as_str()).unwrap_or(false);

    let period_matches = filing
        .report_period_end
        .map(|report_end| {
            let report_end = report_end.format(&Iso8601::DATE).unwrap_or_default();
            fact.end.as_deref() == Some(report_end.as_str())
        })
        .unwrap_or(false);

    let is_instant_fact = fact.start.is_none();

    if !(fact.end.is_some() && fact.val.is_some()) {
        return None;
    }

    if accession_matches && period_matches {
        Some(0)
    } else if form_matches && period_matches {
        Some(1)
    } else if is_instant_fact && period_matches {
        // SEC companyfacts often points comparative balance-sheet values at a later filing rather
        // than the original accession. For point-in-time facts, matching the exact end date is a
        // conservative fallback because the value belongs to that balance-sheet date regardless of
        // which later filing repeated it.
        Some(2)
    } else if accession_matches && filing.report_period_end.is_none() {
        Some(3)
    } else {
        None
    }
}

fn reporting_period_from_fact(
    fact: &CompanyFactUnitValue,
) -> Result<ReportingPeriod, XbrlExtractionError> {
    let end = parse_date(fact.end.as_deref().ok_or_else(|| XbrlExtractionError::InvalidDate {
        value: "missing end date".to_string(),
    })?)?;
    let fiscal_period = match (fact.fy, fact.fp.as_deref()) {
        (Some(fiscal_year), Some("FY")) => Some(filing_models::FiscalPeriod {
            fiscal_year,
            fiscal_quarter: Some(filing_models::FiscalQuarter::Q4),
        }),
        (Some(fiscal_year), Some("Q1")) => Some(filing_models::FiscalPeriod {
            fiscal_year,
            fiscal_quarter: Some(filing_models::FiscalQuarter::Q1),
        }),
        (Some(fiscal_year), Some("Q2")) => Some(filing_models::FiscalPeriod {
            fiscal_year,
            fiscal_quarter: Some(filing_models::FiscalQuarter::Q2),
        }),
        (Some(fiscal_year), Some("Q3")) => Some(filing_models::FiscalPeriod {
            fiscal_year,
            fiscal_quarter: Some(filing_models::FiscalQuarter::Q3),
        }),
        (Some(fiscal_year), Some("Q4")) => Some(filing_models::FiscalPeriod {
            fiscal_year,
            fiscal_quarter: Some(filing_models::FiscalQuarter::Q4),
        }),
        _ => None,
    };

    let context = if let Some(start) = &fact.start {
        PeriodContext::Duration { start: parse_date(start)?, end }
    } else {
        PeriodContext::Instant { as_of: end }
    };

    Ok(ReportingPeriod { context, fiscal_period, label: fact.fp.clone() })
}

fn parse_date(value: &str) -> Result<Date, XbrlExtractionError> {
    Date::parse(value, &Iso8601::DATE)
        .map_err(|_| XbrlExtractionError::InvalidDate { value: value.to_string() })
}

fn measurement_unit_from_company_facts_unit(unit_name: &str) -> MeasurementUnit {
    match unit_name {
        "USD" => MeasurementUnit::Currency("USD".to_string()),
        "USD/shares" | "shares/USD" | "USD/share" | "usd/shares" => MeasurementUnit::Ratio,
        "shares" => MeasurementUnit::Shares,
        "pure" => MeasurementUnit::Ratio,
        "percent" | "percentage" => MeasurementUnit::Percentage,
        other if other.to_ascii_uppercase().starts_with("USD") => {
            MeasurementUnit::Currency(other.to_string())
        }
        other => MeasurementUnit::Other(other.to_string()),
    }
}

fn statement_name_label(statement: accounting_domains::StatementName) -> String {
    match statement {
        accounting_domains::StatementName::BalanceSheet => "balance_sheet",
        accounting_domains::StatementName::IncomeStatement => "income_statement",
        accounting_domains::StatementName::CashFlowStatement => "cash_flow",
        accounting_domains::StatementName::ShareholdersEquityStatement => "shareholders_equity",
        accounting_domains::StatementName::SegmentFootnote => "segment_footnote",
        accounting_domains::StatementName::DebtFootnote => "debt_footnote",
        accounting_domains::StatementName::DerivativeFootnote => "derivative_footnote",
        accounting_domains::StatementName::EquityCompFootnote => "equity_comp_footnote",
        accounting_domains::StatementName::Notes => "notes",
        accounting_domains::StatementName::Mda => "mda",
        accounting_domains::StatementName::Other => "other",
    }
    .to_string()
}

fn extract_segment_name_from_frame(frame: Option<&str>) -> Option<String> {
    let frame = frame?;
    let tokens: Vec<&str> = frame.split('_').collect();

    for index in 0..tokens.len() {
        let token = tokens[index];
        if !token.to_ascii_lowercase().contains("segment") || !token.ends_with("Axis") {
            continue;
        }

        for candidate in tokens.iter().skip(index + 1) {
            if candidate.ends_with("Member") {
                return Some(humanize_member_name(candidate));
            }
        }
    }

    None
}

fn humanize_member_name(member: &str) -> String {
    let without_suffix = member.strip_suffix("Member").unwrap_or(member);
    let mut humanized = String::with_capacity(without_suffix.len() + 8);

    for (index, ch) in without_suffix.chars().enumerate() {
        if index > 0 && ch.is_ascii_uppercase() {
            humanized.push(' ');
        }
        humanized.push(ch);
    }

    humanized
}

#[cfg(test)]
mod tests {
    use super::*;
    use filing_models::{FilingForm, FilingUrls, SourceType};
    use time::macros::date;

    fn sample_filing(accession: &str, form: FilingForm, report_end: Date) -> FilingMetadata {
        FilingMetadata {
            accession_number: accession.to_string(),
            form_type: form,
            filing_date: date!(2025 - 02 - 01),
            report_period_end: Some(report_end),
            fiscal_period: None,
            filing_urls: FilingUrls {
                filing_detail: None,
                primary_document: Some("https://example.test/filing.htm".to_string()),
                xbrl_instance: None,
                html_index: None,
            },
            source_types: vec![SourceType::Xbrl],
            is_amendment: false,
        }
    }

    fn sample_company_facts_json() -> &'static str {
        r#"
{
  "cik": "0000798354",
  "entityName": "Example Corp",
  "facts": {
    "us-gaap": {
      "CashAndCashEquivalentsAtCarryingValue": {
        "label": "Cash and cash equivalents",
        "description": "Cash",
        "units": {
          "USD": [
            {
              "end": "2024-12-31",
              "val": 1250000,
              "accn": "0000798354-25-000010",
              "fy": 2024,
              "fp": "FY",
              "form": "10-K",
              "filed": "2025-02-01"
            }
          ]
        }
      },
      "RevenueFromContractWithCustomerExcludingAssessedTax": {
        "label": "Revenue",
        "units": {
          "USD": [
            {
              "start": "2024-01-01",
              "end": "2024-12-31",
              "val": 9800000,
              "accn": "0000798354-25-000010",
              "fy": 2024,
              "fp": "FY",
              "form": "10-K",
              "filed": "2025-02-01"
            }
          ]
        }
      },
      "NetCashProvidedByUsedInOperatingActivities": {
        "label": "Net cash from operations",
        "units": {
          "USD": [
            {
              "start": "2024-01-01",
              "end": "2024-12-31",
              "val": 2200000,
              "accn": "0000798354-25-000010",
              "fy": 2024,
              "fp": "FY",
              "form": "10-K",
              "filed": "2025-02-01"
            }
          ]
        }
      },
      "LineOfCreditFacilityAmountOutstanding": {
        "label": "Line of credit outstanding",
        "units": {
          "USD": [
            {
              "end": "2024-12-31",
              "val": 450000,
              "accn": "0000798354-25-000010",
              "fy": 2024,
              "fp": "FY",
              "form": "10-K",
              "filed": "2025-02-01"
            }
          ]
        }
      },
      "ShareBasedCompensation": {
        "label": "Share-based compensation",
        "units": {
          "USD": [
            {
              "start": "2024-01-01",
              "end": "2024-12-31",
              "val": 310000,
              "accn": "0000798354-25-000010",
              "fy": 2024,
              "fp": "FY",
              "form": "10-K",
              "filed": "2025-02-01"
            }
          ]
        }
      },
      "TreasuryStockSharesAcquired": {
        "label": "Treasury stock shares acquired",
        "units": {
          "shares": [
            {
              "end": "2024-12-31",
              "val": 1250000,
              "accn": "0000798354-25-000010",
              "fy": 2024,
              "fp": "FY",
              "form": "10-K",
              "filed": "2025-02-01"
            }
          ]
        }
      }
    }
  }
}
        "#
    }

    #[test]
    fn parses_company_facts_payload() {
        let extractor = XbrlExtractor::default();
        let parsed = extractor
            .parse_company_facts_json(sample_company_facts_json())
            .expect("payload should parse");

        assert_eq!(parsed.cik, "0000798354");
        assert!(
            parsed
                .facts
                .get("us-gaap")
                .expect("taxonomy should exist")
                .contains_key("RevenueFromContractWithCustomerExcludingAssessedTax")
        );
    }

    #[test]
    fn extracts_core_statement_metrics_from_company_facts() {
        let extractor = XbrlExtractor::default();
        let facts = extractor
            .parse_company_facts_json(sample_company_facts_json())
            .expect("payload should parse");
        let extracted = extractor
            .extract_for_filings(
                &facts,
                &[sample_filing(
                    "0000798354-25-000010",
                    FilingForm::Form10K,
                    date!(2024 - 12 - 31),
                )],
            )
            .expect("facts should extract");

        assert!(
            extracted
                .iter()
                .any(|metric| metric.metric_id.as_str() == "balance_sheet.cash_and_equivalents")
        );
        assert!(
            extracted.iter().any(|metric| metric.metric_id.as_str() == "income_statement.revenue")
        );
        assert!(
            extracted
                .iter()
                .any(|metric| metric.metric_id.as_str() == "cash_flow.net_cash_from_operations")
        );
    }

    #[test]
    fn extracts_specialized_domain_metrics_from_company_facts() {
        let extractor = XbrlExtractor::default();
        let facts = extractor
            .parse_company_facts_json(sample_company_facts_json())
            .expect("payload should parse");
        let extracted = extractor
            .extract_for_filings(
                &facts,
                &[sample_filing(
                    "0000798354-25-000010",
                    FilingForm::Form10K,
                    date!(2024 - 12 - 31),
                )],
            )
            .expect("facts should extract");

        assert!(
            extracted
                .iter()
                .any(|metric| metric.metric_id.as_str() == "debt_and_credit.revolver_balance")
        );
        assert!(
            extracted.iter().any(|metric| metric.metric_id.as_str()
                == "equity_compensation.stock_based_comp_expense")
        );
        assert!(
            extracted
                .iter()
                .any(|metric| metric.metric_id.as_str() == "equity_compensation.shares_repurchased")
        );
    }

    #[test]
    fn fixture_companyfacts_for_selected_cik_extracts_expected_metrics() {
        let extractor = XbrlExtractor::default();
        let facts = extractor
            .parse_company_facts_json(include_str!(
                "../../../fixtures/0000798354/companyfacts_sample.json"
            ))
            .expect("fixture companyfacts should parse");

        let extracted = extractor
            .extract_for_filings(
                &facts,
                &[sample_filing(
                    "0000798354-25-000010",
                    FilingForm::Form10K,
                    date!(2024 - 12 - 31),
                )],
            )
            .expect("fixture companyfacts should extract");

        assert_eq!(facts.cik, "0000798354");
        assert!(
            extracted
                .iter()
                .any(|metric| metric.metric_id.as_str() == "income_statement.net_income")
        );
        assert!(
            extracted.iter().any(|metric| metric.metric_id.as_str()
                == "equity_compensation.stock_based_comp_expense")
        );
    }

    #[test]
    fn instant_fact_can_fall_back_to_same_period_end_when_accession_differs() {
        let extractor = XbrlExtractor::default();
        let filing =
            sample_filing("0001111111-20-000001", FilingForm::Form10K, date!(2019 - 12 - 31));
        let payload = r#"
{
  "cik": "0000798354",
  "entityName": "Example Corp",
  "facts": {
    "us-gaap": {
      "CashAndCashEquivalentsAtCarryingValue": {
        "label": "Cash and cash equivalents",
        "units": {
          "USD": [
            {
              "end": "2019-12-31",
              "val": 2353000000,
              "accn": "0001111111-20-000999",
              "fy": 2020,
              "fp": "Q1",
              "form": "10-Q",
              "filed": "2020-04-28"
            }
          ]
        }
      }
    }
  }
}
"#;

        let facts = extractor.parse_company_facts_json(payload).expect("payload should parse");
        let extracted = extractor
            .extract_for_filings(&facts, &[filing])
            .expect("same-period instant fact should extract");

        let cash = extracted
            .iter()
            .find(|metric| metric.metric_id.as_str() == "balance_sheet.cash_and_equivalents")
            .expect("cash metric should be extracted");
        assert_eq!(cash.numeric_value.amount, 2353000000.0);
        assert_eq!(
            cash.numeric_value.provenance.xbrl_tag.as_deref(),
            Some("CashAndCashEquivalentsAtCarryingValue")
        );
    }

    #[test]
    fn preferred_tag_wrong_comparative_fact_does_not_block_correct_alternate_tag() {
        let extractor = XbrlExtractor::default();
        let filing =
            sample_filing("0001111111-20-000001", FilingForm::Form10Q, date!(2020 - 03 - 31));
        let payload = r#"
{
  "cik": "0000798354",
  "entityName": "Example Corp",
  "facts": {
    "us-gaap": {
      "CashAndCashEquivalentsAtCarryingValue": {
        "label": "Cash and cash equivalents",
        "units": {
          "USD": [
            {
              "end": "2019-12-31",
              "val": 2353000000,
              "accn": "0001111111-20-000001",
              "fy": 2020,
              "fp": "Q1",
              "form": "10-Q",
              "filed": "2020-04-28"
            }
          ]
        }
      },
      "CashCashEquivalentsRestrictedCashAndRestrictedCashEquivalents": {
        "label": "Cash, Cash Equivalents, Restricted Cash and Restricted Cash Equivalents",
        "units": {
          "USD": [
            {
              "end": "2020-03-31",
              "val": 4253000000,
              "accn": "0001111111-20-000001",
              "fy": 2020,
              "fp": "Q1",
              "form": "10-Q",
              "filed": "2020-04-28"
            }
          ]
        }
      }
    }
  }
}
"#;

        let facts = extractor.parse_company_facts_json(payload).expect("payload should parse");
        let extracted =
            extractor.extract_for_filings(&facts, &[filing]).expect("alternate tag should extract");

        let cash = extracted
            .iter()
            .find(|metric| metric.metric_id.as_str() == "balance_sheet.cash_and_equivalents")
            .expect("cash metric should be extracted");
        assert_eq!(cash.numeric_value.amount, 4253000000.0);
        assert_eq!(
            cash.numeric_value.provenance.xbrl_tag.as_deref(),
            Some("CashCashEquivalentsRestrictedCashAndRestrictedCashEquivalents")
        );
    }

    #[test]
    fn extracts_multiple_segment_members_for_same_metric_and_filing() {
        let extractor = XbrlExtractor::default();
        let filing =
            sample_filing("0001111111-25-000001", FilingForm::Form10K, date!(2024 - 12 - 31));
        let payload = r#"
{
  "cik": "0001111111",
  "entityName": "Example Segment Co",
  "facts": {
    "us-gaap": {
      "RevenueFromExternalCustomersByReportableSegment": {
        "label": "Revenue from external customers by reportable segment",
        "units": {
          "USD": [
            {
              "start": "2024-01-01",
              "end": "2024-12-31",
              "val": 600.0,
              "accn": "0001111111-25-000001",
              "fy": 2024,
              "fp": "FY",
              "form": "10-K",
              "filed": "2025-02-20",
              "frame": "CY2024_us-gaap_StatementBusinessSegmentsAxis_us-gaap_ConsumerSegmentMember"
            },
            {
              "start": "2024-01-01",
              "end": "2024-12-31",
              "val": 400.0,
              "accn": "0001111111-25-000001",
              "fy": 2024,
              "fp": "FY",
              "form": "10-K",
              "filed": "2025-02-20",
              "frame": "CY2024_us-gaap_StatementBusinessSegmentsAxis_us-gaap_IndustrialSegmentMember"
            }
          ]
        }
      }
    }
  }
}
"#;

        let facts = extractor.parse_company_facts_json(payload).expect("payload should parse");
        let extracted =
            extractor.extract_for_filings(&facts, &[filing]).expect("segment facts should extract");

        let segment_revenue: Vec<_> = extracted
            .iter()
            .filter(|metric| metric.metric_id.as_str() == "segment_data.segment_revenue")
            .collect();
        assert_eq!(segment_revenue.len(), 2);

        let mut segment_names: Vec<_> = segment_revenue
            .iter()
            .map(|metric| {
                metric
                    .numeric_value
                    .provenance
                    .source_location
                    .segment_name
                    .clone()
                    .expect("segment facts should preserve segment name")
            })
            .collect();
        segment_names.sort();
        assert_eq!(segment_names, vec!["Consumer Segment", "Industrial Segment"]);
    }
}
