//! HTML and text fallback extraction.
//!
//! This extractor is intentionally conservative. It aims to recover values when XBRL is missing or
//! insufficient, and to capture narrative sections like footnotes and MD&A with clear source
//! metadata.

use accounting_domains::{DomainMetric, MetricId, MetricRegistry};
use filing_models::{
    FilingMetadata, FilingSourceMethod, MeasurementUnit, MetricValue, NumericValue, PeriodContext,
    Provenance, ReportingPeriod, SignConvention, SourceLocator, SourceType, TextBlock, ValueScale,
};
use scraper::{ElementRef, Html, Selector};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedHtmlMetricValue {
    pub metric_id: MetricId,
    pub metric_name: String,
    pub domain: accounting_domains::DomainName,
    pub subdomain: Option<String>,
    pub numeric_value: NumericValue,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedNarrativeSection {
    pub metric_id: MetricId,
    pub domain: accounting_domains::DomainName,
    pub value: MetricValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HtmlColumnPeriod {
    display_column_start: usize,
    display_column_end: usize,
    reporting_period: ReportingPeriod,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HtmlTableCell {
    text: String,
    display_column_start: usize,
    display_column_end: usize,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct HtmlExtractionResult {
    pub numeric_fallbacks: Vec<ExtractedHtmlMetricValue>,
    pub narrative_sections: Vec<ExtractedNarrativeSection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskFactorExtractionSkeleton {
    pub metric_id: MetricId,
    pub intended_domain: accounting_domains::DomainName,
    pub implementation_note: &'static str,
}

impl RiskFactorExtractionSkeleton {
    pub fn planned() -> Self {
        Self {
            metric_id: MetricId::new("risk_factors.placeholder"),
            intended_domain: accounting_domains::DomainName::RiskFactorsSkeleton,
            implementation_note: "Risk factor text extraction is intentionally deferred. Add section parsing here after numeric and MD&A workflows are stable.",
        }
    }
}

#[derive(Debug, Error)]
pub enum HtmlExtractionError {
    #[error("failed to parse numeric fallback value from row label {row_label}")]
    InvalidNumericValue { row_label: String },
}

#[derive(Debug, Clone)]
pub struct HtmlExtractor {
    registry: MetricRegistry,
    alias_index: HashMap<String, Vec<MetricId>>,
}

impl Default for HtmlExtractor {
    fn default() -> Self {
        Self::new(MetricRegistry::default())
    }
}

impl HtmlExtractor {
    pub fn new(registry: MetricRegistry) -> Self {
        let alias_index = build_alias_index(&registry);
        Self { registry, alias_index }
    }

    pub fn extract(
        &self,
        html: &str,
        filing: &FilingMetadata,
    ) -> Result<HtmlExtractionResult, HtmlExtractionError> {
        let document = Html::parse_document(html);

        Ok(HtmlExtractionResult {
            numeric_fallbacks: self
                .extract_numeric_fallbacks_with_inline_xbrl(html, &document, filing)?,
            narrative_sections: self.extract_narrative_sections_from_document(&document, filing),
        })
    }

    pub fn extract_numeric_fallbacks(
        &self,
        html: &str,
        filing: &FilingMetadata,
    ) -> Result<Vec<ExtractedHtmlMetricValue>, HtmlExtractionError> {
        let document = Html::parse_document(html);
        self.extract_numeric_fallbacks_with_inline_xbrl(html, &document, filing)
    }

    pub fn extract_narrative_sections(
        &self,
        html: &str,
        filing: &FilingMetadata,
    ) -> Vec<ExtractedNarrativeSection> {
        let document = Html::parse_document(html);
        self.extract_narrative_sections_from_document(&document, filing)
    }

    fn extract_numeric_fallbacks_from_document(
        &self,
        document: &Html,
        filing: &FilingMetadata,
    ) -> Result<Vec<ExtractedHtmlMetricValue>, HtmlExtractionError> {
        let table_selector = selector("table");
        let row_selector = selector("tr");
        let cell_selector = selector("th, td");
        let caption_selector = selector("caption");
        let mut extracted = Vec::new();

        for table in document.select(&table_selector) {
            let structured_rows =
                collect_structured_table_rows(&table, &row_selector, &cell_selector);
            let table_caption_context =
                table.select(&caption_selector).next().map(text_content).unwrap_or_default();
            let header_context = infer_table_context(&structured_rows);
            let bare_annual_context = if table_caption_context.trim().is_empty() {
                header_context.clone()
            } else {
                table_caption_context.clone()
            };
            let default_table_context = table_caption_context.clone();
            let table_scale = infer_scale(&default_table_context);
            let header_cells = structured_rows
                .first()
                .map(|row| row.iter().map(|cell| cell.text.clone()).collect::<Vec<_>>())
                .unwrap_or_default();
            let column_periods =
                table_column_periods(&structured_rows, filing, &bare_annual_context);
            let table_context =
                if !column_periods.is_empty() && table_caption_context.trim().is_empty() {
                    header_context.clone()
                } else {
                    default_table_context
                };
            let provenance_context = if table_context.trim().is_empty() {
                header_context.clone()
            } else {
                table_context.clone()
            };
            let skip_generic_extraction = should_skip_generic_table_extraction(&provenance_context);

            extracted.extend(self.extract_debt_note_table_metrics(
                &structured_rows,
                filing,
                &provenance_context,
                table_scale,
            ));
            extracted.extend(self.extract_funding_flow_table_metrics(
                &structured_rows,
                &header_cells,
                filing,
                &provenance_context,
                table_scale,
            ));
            extracted.extend(self.extract_funding_balance_table_metrics(
                &structured_rows,
                &header_cells,
                filing,
                &provenance_context,
                table_scale,
            ));

            if skip_generic_extraction {
                continue;
            }

            for structured_cells in &structured_rows {
                let cells: Vec<String> =
                    structured_cells.iter().map(|cell| cell.text.clone()).collect();
                if cells.len() < 2 {
                    continue;
                }

                let row_label = cells[0].trim().to_string();
                let Some(row_label_quality) = RowLabelQuality::from_label(&row_label) else {
                    continue;
                };
                let matched_metric = match self
                    .match_row_label_to_metric(&row_label_quality.normalized, &table_context)
                {
                    Some(metric) => metric,
                    None => continue,
                };

                let unit = measurement_unit_from_hint(
                    matched_metric.definition.expected_unit_hint.as_deref(),
                );
                let allows_percentage_values = metric_allows_percentage_values(matched_metric);
                let explicit_period_values = if column_periods.is_empty() {
                    Vec::new()
                } else {
                    column_periods
                        .iter()
                        .filter_map(|column_period| {
                            let numeric_text = select_numeric_cell_in_period_span(
                                &structured_cells,
                                column_period,
                            )?;
                            if !allows_percentage_values && cell_looks_like_percentage(numeric_text)
                            {
                                return None;
                            }
                            let (amount, sign_convention) = parse_numeric_cell(numeric_text)?;
                            if !amount_is_plausible_for_metric(
                                matched_metric.definition.metric_id.as_str(),
                                amount,
                            ) {
                                return None;
                            }
                            Some((
                                column_period.reporting_period.clone(),
                                amount,
                                sign_convention,
                                numeric_text.to_string(),
                            ))
                        })
                        .collect::<Vec<_>>()
                };

                if !explicit_period_values.is_empty() {
                    for (reporting_period, amount, sign_convention, numeric_text) in
                        explicit_period_values
                    {
                        let provenance = Provenance {
                            accession_number: filing.accession_number.clone(),
                            filing_url: filing.filing_urls.primary_document.clone(),
                            form_type: filing.form_type.clone(),
                            source_type: SourceType::Html,
                            source_method: FilingSourceMethod::FilingHtml,
                            source_location: SourceLocator {
                                section_name: if provenance_context.is_empty() {
                                    None
                                } else {
                                    Some(provenance_context.clone())
                                },
                                table_name: if provenance_context.is_empty() {
                                    None
                                } else {
                                    Some(provenance_context.clone())
                                },
                                row_label: Some(row_label.clone()),
                                cell_reference: None,
                                segment_name: None,
                            },
                            xbrl_tag: None,
                            filing_label: Some(row_label.clone()),
                            reporting_period: reporting_period.clone(),
                            unit: unit.clone(),
                            scale: table_scale,
                        };

                        extracted.push(ExtractedHtmlMetricValue {
                            metric_id: matched_metric.definition.metric_id.clone(),
                            metric_name: matched_metric.definition.display_name.clone(),
                            domain: matched_metric.definition.domain,
                            subdomain: matched_metric.subdomain.clone(),
                            numeric_value: NumericValue {
                                amount,
                                unit: unit.clone(),
                                scale: table_scale,
                                sign_convention,
                                label: Some(numeric_text),
                                reporting_period,
                                provenance,
                            },
                        });
                    }

                    continue;
                }

                let numeric_text = select_numeric_cell_for_filing(&cells, &header_cells, filing);

                let Some(numeric_text) = numeric_text else {
                    continue;
                };

                if !allows_percentage_values && cell_looks_like_percentage(&numeric_text) {
                    continue;
                }

                let Some((amount, sign_convention)) = parse_numeric_cell(&numeric_text) else {
                    return Err(HtmlExtractionError::InvalidNumericValue { row_label });
                };
                if !amount_is_plausible_for_metric(
                    matched_metric.definition.metric_id.as_str(),
                    amount,
                ) {
                    continue;
                }

                let reporting_period = reporting_period_from_filing(filing);
                let provenance = Provenance {
                    accession_number: filing.accession_number.clone(),
                    filing_url: filing.filing_urls.primary_document.clone(),
                    form_type: filing.form_type.clone(),
                    source_type: SourceType::Html,
                    source_method: FilingSourceMethod::FilingHtml,
                    source_location: SourceLocator {
                        section_name: if provenance_context.is_empty() {
                            None
                        } else {
                            Some(provenance_context.clone())
                        },
                        table_name: if provenance_context.is_empty() {
                            None
                        } else {
                            Some(provenance_context.clone())
                        },
                        row_label: Some(row_label.clone()),
                        cell_reference: None,
                        segment_name: None,
                    },
                    xbrl_tag: None,
                    filing_label: Some(row_label.clone()),
                    reporting_period: reporting_period.clone(),
                    unit: unit.clone(),
                    scale: table_scale,
                };

                extracted.push(ExtractedHtmlMetricValue {
                    metric_id: matched_metric.definition.metric_id.clone(),
                    metric_name: matched_metric.definition.display_name.clone(),
                    domain: matched_metric.definition.domain,
                    subdomain: matched_metric.subdomain.clone(),
                    numeric_value: NumericValue {
                        amount,
                        unit,
                        scale: table_scale,
                        sign_convention,
                        label: Some(row_label),
                        reporting_period,
                        provenance,
                    },
                });
            }
        }

        extracted.sort_by(|left, right| {
            left.metric_id
                .as_str()
                .cmp(right.metric_id.as_str())
                .then_with(|| left.metric_name.cmp(&right.metric_name))
        });

        Ok(extracted)
    }

    fn extract_numeric_fallbacks_with_inline_xbrl(
        &self,
        html: &str,
        document: &Html,
        filing: &FilingMetadata,
    ) -> Result<Vec<ExtractedHtmlMetricValue>, HtmlExtractionError> {
        let mut extracted = self.extract_numeric_fallbacks_from_document(document, filing)?;
        extracted.extend(self.extract_core_inline_xbrl_metrics(html, filing));
        extracted.extend(self.extract_segment_inline_xbrl_metrics(html, filing));
        extracted.extend(self.extract_derivative_inline_xbrl_metrics(html, filing));
        extracted.extend(self.extract_equity_comp_inline_xbrl_metrics(html, filing));
        if extracted
            .iter()
            .all(|metric| metric.metric_id.as_str() != "debt_and_credit.revolver_balance")
        {
            if let Some(revolver_zero) = self.extract_undrawn_revolver_from_text(document, filing) {
                extracted.push(revolver_zero);
            }
        }
        extracted.sort_by(|left, right| {
            left.metric_id
                .as_str()
                .cmp(right.metric_id.as_str())
                .then_with(|| left.metric_name.cmp(&right.metric_name))
        });
        Ok(extracted)
    }

    fn extract_undrawn_revolver_from_text(
        &self,
        document: &Html,
        filing: &FilingMetadata,
    ) -> Option<ExtractedHtmlMetricValue> {
        let metric = self.registry.by_id("debt_and_credit.revolver_balance")?;
        let report_end = filing.report_period_end?;
        let report_end_text =
            report_end.format(&time::format_description::well_known::Iso8601::DATE).ok()?;
        let report_end_human =
            format!("{} {}, {}", report_end.month(), report_end.day(), report_end.year())
                .to_ascii_lowercase();

        let document_text = document.root_element().text().collect::<Vec<_>>().join(" ");
        let normalized = normalize_label(&document_text);

        let has_credit_facility = normalized.contains("revolving credit facility")
            || normalized.contains("credit facility");
        let has_undrawn = normalized.contains("undrawn");
        let has_report_date = normalized.contains(&normalize_label(&report_end_human))
            || normalized.contains(&normalize_label(&report_end_text));

        if !(has_credit_facility && has_undrawn && has_report_date) {
            return None;
        }

        let reporting_period = reporting_period_from_filing(filing);
        let unit = MeasurementUnit::Currency("USD".to_string());
        Some(ExtractedHtmlMetricValue {
            metric_id: metric.definition.metric_id.clone(),
            metric_name: metric.definition.display_name.clone(),
            domain: metric.definition.domain,
            subdomain: metric.subdomain.clone(),
            numeric_value: NumericValue {
                amount: 0.0,
                unit: unit.clone(),
                scale: ValueScale::Raw,
                sign_convention: SignConvention::AsReported,
                label: Some("Undrawn revolving credit facility".to_string()),
                reporting_period: reporting_period.clone(),
                provenance: Provenance {
                    accession_number: filing.accession_number.clone(),
                    filing_url: filing.filing_urls.primary_document.clone(),
                    form_type: filing.form_type.clone(),
                    source_type: SourceType::Html,
                    source_method: FilingSourceMethod::FilingText,
                    source_location: SourceLocator {
                        section_name: Some("credit_facility_text_disclosure".to_string()),
                        table_name: None,
                        row_label: Some("Undrawn revolving credit facility".to_string()),
                        cell_reference: None,
                        segment_name: None,
                    },
                    xbrl_tag: None,
                    filing_label: Some("Undrawn revolving credit facility".to_string()),
                    reporting_period,
                    unit,
                    scale: ValueScale::Raw,
                },
            },
        })
    }

    fn extract_core_inline_xbrl_metrics(
        &self,
        html: &str,
        filing: &FilingMetadata,
    ) -> Vec<ExtractedHtmlMetricValue> {
        let core_contexts = inline_xbrl_core_contexts(html);
        if core_contexts.is_empty() {
            return Vec::new();
        }

        let revenue_tags = [
            "RevenueFromContractWithCustomerExcludingAssessedTax",
            "Revenues",
            "SalesRevenueNet",
            "SalesRevenueServicesNet",
        ];
        let Some(metric) = self.registry.by_id("income_statement.revenue") else {
            return Vec::new();
        };

        let mut extracted = Vec::new();
        for fact in inline_xbrl_nonfraction_facts(html) {
            let Some(context) = core_contexts.get(&fact.context_ref) else {
                continue;
            };
            let local_name = fact.name.rsplit(':').next().unwrap_or(&fact.name);
            if !revenue_tags.contains(&local_name) {
                continue;
            }
            let Some((amount, sign_convention)) = parse_numeric_cell(&fact.value) else {
                continue;
            };
            let reporting_period = context
                .clone()
                .or_else(|| reporting_period_from_inline_context_id(&fact.context_ref))
                .unwrap_or_else(|| reporting_period_from_filing(filing));
            if !core_inline_xbrl_matches_current_filing_period(&reporting_period, filing) {
                continue;
            }
            let unit = measurement_unit_from_inline_unit_ref(fact.unit_ref.as_deref());
            let scale = inline_xbrl_scale_from_attr(fact.scale.as_deref());

            extracted.push(ExtractedHtmlMetricValue {
                metric_id: metric.definition.metric_id.clone(),
                metric_name: metric.definition.display_name.clone(),
                domain: metric.definition.domain,
                subdomain: metric.subdomain.clone(),
                numeric_value: NumericValue {
                    amount,
                    unit: unit.clone(),
                    scale,
                    sign_convention,
                    label: Some(fact.value.clone()),
                    reporting_period: reporting_period.clone(),
                    provenance: Provenance {
                        accession_number: filing.accession_number.clone(),
                        filing_url: filing.filing_urls.primary_document.clone(),
                        form_type: filing.form_type.clone(),
                        // This value comes from embedded inline XBRL inside the filing HTML, not
                        // from row-label fallback matching. Keep it XBRL-typed so review logic
                        // can distinguish it from conservative HTML fallback values.
                        source_type: SourceType::Xbrl,
                        source_method: FilingSourceMethod::FilingHtml,
                        source_location: SourceLocator {
                            section_name: Some("inline_xbrl_core".to_string()),
                            table_name: Some("inline_xbrl_core".to_string()),
                            row_label: Some(metric.definition.display_name.clone()),
                            cell_reference: Some(fact.context_ref.clone()),
                            segment_name: None,
                        },
                        xbrl_tag: Some(fact.name.clone()),
                        filing_label: Some(metric.definition.display_name.clone()),
                        reporting_period,
                        unit,
                        scale,
                    },
                },
            });
        }

        extracted
    }

    fn extract_segment_inline_xbrl_metrics(
        &self,
        html: &str,
        filing: &FilingMetadata,
    ) -> Vec<ExtractedHtmlMetricValue> {
        let segment_contexts = inline_xbrl_segment_contexts(html);
        if segment_contexts.is_empty() {
            return Vec::new();
        }

        let mut extracted = Vec::new();
        for fact in inline_xbrl_nonfraction_facts(html) {
            let Some(context) = segment_contexts.get(&fact.context_ref) else {
                continue;
            };
            let Some(metric_id) = inline_xbrl_segment_metric_id(&fact.name) else {
                continue;
            };
            let Some(metric) = self.registry.by_id(metric_id) else {
                continue;
            };
            let Some((amount, sign_convention)) = parse_numeric_cell(&fact.value) else {
                continue;
            };
            let reporting_period = context
                .reporting_period
                .clone()
                .or_else(|| reporting_period_from_inline_context_id(&fact.context_ref))
                .unwrap_or_else(|| reporting_period_from_filing(filing));
            let unit = measurement_unit_from_inline_unit_ref(fact.unit_ref.as_deref());
            let scale = inline_xbrl_scale_from_attr(fact.scale.as_deref());

            extracted.push(ExtractedHtmlMetricValue {
                metric_id: metric.definition.metric_id.clone(),
                metric_name: metric.definition.display_name.clone(),
                domain: metric.definition.domain,
                subdomain: metric.subdomain.clone(),
                numeric_value: NumericValue {
                    amount,
                    unit: unit.clone(),
                    scale,
                    sign_convention,
                    label: Some(fact.value.clone()),
                    reporting_period: reporting_period.clone(),
                    provenance: Provenance {
                        accession_number: filing.accession_number.clone(),
                        filing_url: filing.filing_urls.primary_document.clone(),
                        form_type: filing.form_type.clone(),
                        source_type: SourceType::Html,
                        // This is still read from filing HTML, but the value comes from the
                        // embedded inline XBRL fact rather than inferred table text.
                        source_method: FilingSourceMethod::FilingHtml,
                        source_location: SourceLocator {
                            section_name: Some("inline_xbrl_segment".to_string()),
                            table_name: Some("inline_xbrl_segment".to_string()),
                            row_label: Some(metric.definition.display_name.clone()),
                            cell_reference: Some(fact.context_ref.clone()),
                            segment_name: Some(context.segment_name.clone()),
                        },
                        xbrl_tag: Some(fact.name.clone()),
                        filing_label: Some(metric.definition.display_name.clone()),
                        reporting_period,
                        unit,
                        scale,
                    },
                },
            });
        }

        extracted
    }

    fn extract_derivative_inline_xbrl_metrics(
        &self,
        html: &str,
        filing: &FilingMetadata,
    ) -> Vec<ExtractedHtmlMetricValue> {
        let derivative_contexts = inline_xbrl_derivative_contexts(html);
        if derivative_contexts.is_empty() {
            return Vec::new();
        }

        let Some(metric) = self.registry.by_id("derivatives_and_securities.derivative_gain_loss")
        else {
            return Vec::new();
        };

        let mut aggregated: std::collections::BTreeMap<String, (ReportingPeriod, f64, ValueScale)> =
            std::collections::BTreeMap::new();

        for fact in inline_xbrl_nonfraction_facts(html) {
            let Some(context) = derivative_contexts.get(&fact.context_ref) else {
                continue;
            };
            if !matches!(
                inline_xbrl_derivative_metric_id(&fact.name),
                Some("derivatives_and_securities.derivative_gain_loss")
            ) {
                continue;
            }
            let Some((amount, _)) = parse_numeric_cell(&fact.value) else {
                continue;
            };
            let reporting_period = context
                .reporting_period
                .clone()
                .or_else(|| reporting_period_from_inline_context_id(&fact.context_ref))
                .unwrap_or_else(|| reporting_period_from_filing(filing));
            let scale = inline_xbrl_scale_from_attr(fact.scale.as_deref());
            let period_key = match reporting_period.context {
                PeriodContext::Instant { as_of } => format!("I:{as_of}"),
                PeriodContext::Duration { start, end } => format!("D:{start}:{end}"),
            };
            let entry = aggregated.entry(period_key).or_insert((reporting_period, 0.0, scale));
            entry.1 += amount;
        }

        aggregated
            .into_values()
            .map(|(reporting_period, amount, scale)| {
                let unit = MeasurementUnit::Currency("USD".to_string());
                ExtractedHtmlMetricValue {
                    metric_id: metric.definition.metric_id.clone(),
                    metric_name: metric.definition.display_name.clone(),
                    domain: metric.definition.domain,
                    subdomain: metric.subdomain.clone(),
                    numeric_value: NumericValue {
                        amount,
                        unit: unit.clone(),
                        scale,
                        sign_convention: SignConvention::AsReported,
                        label: Some("aggregated inline xbrl derivative gain or loss".to_string()),
                        reporting_period: reporting_period.clone(),
                        provenance: Provenance {
                            accession_number: filing.accession_number.clone(),
                            filing_url: filing.filing_urls.primary_document.clone(),
                            form_type: filing.form_type.clone(),
                            source_type: SourceType::Html,
                            // SEC filings often expose derivative gain/loss only as multiple
                            // inline XBRL hedging facts by location/relationship. We aggregate
                            // those allowed contexts into one period value here so the analyst
                            // workbook has a usable domain-level placeholder total. If you later
                            // want hedge-type/location rows separately, split this function into
                            // distinct outputs before normalization.
                            source_method: FilingSourceMethod::FilingHtml,
                            source_location: SourceLocator {
                                section_name: Some("inline_xbrl_derivative".to_string()),
                                table_name: Some("inline_xbrl_derivative".to_string()),
                                row_label: Some(metric.definition.display_name.clone()),
                                cell_reference: Some("aggregated_allowed_contexts".to_string()),
                                segment_name: None,
                            },
                            xbrl_tag: Some("inline_xbrl_derivative_aggregate".to_string()),
                            filing_label: Some(metric.definition.display_name.clone()),
                            reporting_period,
                            unit,
                            scale,
                        },
                    },
                }
            })
            .collect()
    }

    fn extract_equity_comp_inline_xbrl_metrics(
        &self,
        html: &str,
        filing: &FilingMetadata,
    ) -> Vec<ExtractedHtmlMetricValue> {
        let equity_contexts = inline_xbrl_equity_comp_contexts(html);
        if equity_contexts.is_empty() {
            return Vec::new();
        }

        let Some(metric) = self.registry.by_id("cash_flow.share_issuance_proceeds") else {
            return Vec::new();
        };

        let mut extracted = Vec::new();
        for fact in inline_xbrl_nonfraction_facts(html) {
            let Some(context) = equity_contexts.get(&fact.context_ref) else {
                continue;
            };
            if !matches!(
                inline_xbrl_equity_comp_metric_id(&fact.name),
                Some("cash_flow.share_issuance_proceeds")
            ) {
                continue;
            }
            let Some((amount, sign_convention)) = parse_numeric_cell(&fact.value) else {
                continue;
            };
            let reporting_period = context
                .reporting_period
                .clone()
                .or_else(|| reporting_period_from_inline_context_id(&fact.context_ref))
                .unwrap_or_else(|| reporting_period_from_filing(filing));
            let unit = measurement_unit_from_inline_unit_ref(fact.unit_ref.as_deref());
            let scale = inline_xbrl_scale_from_attr(fact.scale.as_deref());

            extracted.push(ExtractedHtmlMetricValue {
                metric_id: metric.definition.metric_id.clone(),
                metric_name: metric.definition.display_name.clone(),
                domain: metric.definition.domain,
                subdomain: metric.subdomain.clone(),
                numeric_value: NumericValue {
                    amount,
                    unit: unit.clone(),
                    scale,
                    sign_convention,
                    label: Some(fact.value.clone()),
                    reporting_period: reporting_period.clone(),
                    provenance: Provenance {
                        accession_number: filing.accession_number.clone(),
                        filing_url: filing.filing_urls.primary_document.clone(),
                        form_type: filing.form_type.clone(),
                        source_type: SourceType::Html,
                        // This remains an HTML-file source, but the value is taken from a direct
                        // inline XBRL fact. If you later want separate stock-option vs broader
                        // employee-benefit issuance buckets, extend this function with additional
                        // explicit tag mapping instead of loosening generic HTML row matching.
                        source_method: FilingSourceMethod::FilingHtml,
                        source_location: SourceLocator {
                            section_name: Some("inline_xbrl_equity_comp".to_string()),
                            table_name: Some("inline_xbrl_equity_comp".to_string()),
                            row_label: Some(metric.definition.display_name.clone()),
                            cell_reference: Some(fact.context_ref.clone()),
                            segment_name: None,
                        },
                        xbrl_tag: Some(fact.name.clone()),
                        filing_label: Some(metric.definition.display_name.clone()),
                        reporting_period,
                        unit,
                        scale,
                    },
                },
            });
        }

        extracted
    }

    fn extract_debt_note_table_metrics(
        &self,
        rows: &[Vec<HtmlTableCell>],
        filing: &FilingMetadata,
        provenance_context: &str,
        table_scale: ValueScale,
    ) -> Vec<ExtractedHtmlMetricValue> {
        let rows = rows.iter().filter(|row| row.len() >= 2).cloned().collect::<Vec<_>>();
        let Some((rate_column, carrying_value_column)) = debt_metric_columns(&rows) else {
            return Vec::new();
        };
        if !looks_like_debt_note_table(provenance_context, rate_column, carrying_value_column) {
            return Vec::new();
        }

        let financial_funding_summary =
            looks_like_financial_funding_summary_table(provenance_context);
        let mut notes_and_bonds_total = 0.0_f64;
        let mut notes_and_bonds_found = false;
        let mut revolver_value = None;
        let mut detail_funding_rows = Vec::new();
        let mut interest_rate_candidates = Vec::new();

        for row in rows.iter().skip(1) {
            let row_label = row[0].text.trim().to_string();
            let normalized_row = normalize_label(&row_label);
            if normalized_row.is_empty() {
                continue;
            }

            let carrying_value = carrying_value_column
                .and_then(|column| row_cell_in_display_column(row, column))
                .and_then(|cell| parse_numeric_cell(&cell.text).map(|(amount, _)| amount));
            let interest_rate = rate_column
                .and_then(|column| row_cell_in_display_column(row, column))
                .and_then(|cell| parse_numeric_cell(&cell.text).map(|(amount, _)| amount));

            if let Some(amount) = carrying_value {
                if is_notes_and_bonds_row(&normalized_row) {
                    notes_and_bonds_total += amount;
                    notes_and_bonds_found = true;
                }

                if let Some(detail_metric_id) = debt_detail_metric_id(&normalized_row) {
                    detail_funding_rows.push((detail_metric_id, row_label.clone(), amount));
                }

                if is_revolver_row(&normalized_row) {
                    revolver_value = Some((row_label.clone(), amount));
                }
            }

            if let Some(rate) = interest_rate {
                if is_interest_rate_row(&normalized_row)
                    || is_revolver_row(&normalized_row)
                    || (financial_funding_summary && is_financial_funding_row(&normalized_row))
                {
                    interest_rate_candidates.push((
                        interest_rate_priority(&normalized_row),
                        row_label.clone(),
                        rate,
                    ));
                }
            }
        }

        let mut extracted = Vec::new();

        if notes_and_bonds_found {
            if let Some(metric) = self.registry.by_id("debt_and_credit.notes_and_bonds") {
                extracted.push(build_debt_note_metric(
                    metric,
                    notes_and_bonds_total,
                    "aggregated debt note instruments".to_string(),
                    filing,
                    provenance_context,
                    table_scale,
                ));
            }
        }

        for (metric_id, row_label, amount) in detail_funding_rows {
            if let Some(metric) = self.registry.by_id(metric_id) {
                extracted.push(build_debt_note_metric(
                    metric,
                    amount,
                    row_label,
                    filing,
                    provenance_context,
                    table_scale,
                ));
            }
        }

        if let Some((row_label, amount)) = revolver_value {
            if let Some(metric) = self.registry.by_id("debt_and_credit.revolver_balance") {
                extracted.push(build_debt_note_metric(
                    metric,
                    amount,
                    row_label,
                    filing,
                    provenance_context,
                    table_scale,
                ));
            }
        }

        interest_rate_candidates.sort_by(|left, right| left.0.cmp(&right.0));
        if let Some((_, row_label, amount)) = interest_rate_candidates.into_iter().next() {
            if let Some(metric) = self.registry.by_id("debt_and_credit.interest_rate") {
                extracted.push(build_debt_note_metric(
                    metric,
                    amount,
                    row_label,
                    filing,
                    provenance_context,
                    table_scale,
                ));
            }
        }

        extracted
    }

    fn extract_funding_flow_table_metrics(
        &self,
        rows: &[Vec<HtmlTableCell>],
        header_cells: &[String],
        filing: &FilingMetadata,
        provenance_context: &str,
        table_scale: ValueScale,
    ) -> Vec<ExtractedHtmlMetricValue> {
        let normalized_context = normalize_label(provenance_context);
        let is_unsecured_funding = normalized_context.contains("long term unsecured funding");
        let is_secured_funding = normalized_context.contains("long term secured funding");
        let is_supported_funding_flow = is_unsecured_funding || is_secured_funding;
        if !is_supported_funding_flow {
            return Vec::new();
        }

        if is_secured_funding {
            return self.extract_secured_funding_flow_table_metrics(
                rows,
                filing,
                provenance_context,
                table_scale,
            );
        }

        let mut extracted = Vec::new();
        let mut current_flow: Option<&'static str> = None;

        for row in rows {
            let cells: Vec<String> = row.iter().map(|cell| cell.text.clone()).collect();
            if cells.len() < 2 {
                continue;
            }

            let row_label = cells[0].trim().to_string();
            let normalized_row = normalize_label(&row_label);
            if normalized_row.is_empty() {
                continue;
            }

            if normalized_row == "issuance" {
                current_flow = Some("issuance");
                continue;
            }
            if normalized_row.contains("maturities") || normalized_row.contains("redemptions") {
                current_flow = Some("maturities");
                continue;
            }

            let Some(base_metric_id) = debt_detail_base_metric_id(&normalized_row) else {
                continue;
            };
            let Some(flow) = current_flow else {
                continue;
            };
            let metric_id = debt_detail_flow_metric_id(base_metric_id, flow);
            let Some(metric_id) = metric_id else {
                continue;
            };
            let Some(numeric_text) = select_numeric_cell_for_filing(&cells, header_cells, filing)
            else {
                continue;
            };
            let Some((amount, _)) = parse_numeric_cell(&numeric_text) else {
                continue;
            };
            let Some(metric) = self.registry.by_id(metric_id) else {
                continue;
            };
            extracted.push(build_debt_note_metric(
                metric,
                amount,
                row_label,
                filing,
                provenance_context,
                table_scale,
            ));
        }

        extracted
    }

    fn extract_secured_funding_flow_table_metrics(
        &self,
        rows: &[Vec<HtmlTableCell>],
        filing: &FilingMetadata,
        provenance_context: &str,
        table_scale: ValueScale,
    ) -> Vec<ExtractedHtmlMetricValue> {
        let Some((issuance_column, maturities_column)) = funding_flow_columns(rows) else {
            return Vec::new();
        };

        let mut extracted = Vec::new();
        for row in rows.iter().skip(1) {
            if row.len() < 2 {
                continue;
            }

            let row_label = row[0].text.trim().to_string();
            let normalized_row = normalize_label(&row_label);
            if normalized_row.is_empty() {
                continue;
            }

            let Some(base_metric_id) = debt_detail_base_metric_id(&normalized_row) else {
                continue;
            };
            if base_metric_id != "debt_and_credit.detail_secured_borrowings" {
                continue;
            }

            let issuance_amount = issuance_column
                .and_then(|column| row_cell_in_display_column(row, column))
                .and_then(|cell| parse_numeric_cell(&cell.text).map(|(amount, _)| amount));
            let maturities_amount = maturities_column
                .and_then(|column| row_cell_in_display_column(row, column))
                .and_then(|cell| parse_numeric_cell(&cell.text).map(|(amount, _)| amount));

            if let Some(amount) = issuance_amount {
                if let Some(metric) =
                    self.registry.by_id("debt_and_credit.detail_secured_borrowings_issuance")
                {
                    extracted.push(build_debt_note_metric(
                        metric,
                        amount,
                        row_label.clone(),
                        filing,
                        provenance_context,
                        table_scale,
                    ));
                }
            }

            if let Some(amount) = maturities_amount {
                if let Some(metric) =
                    self.registry.by_id("debt_and_credit.detail_secured_borrowings_maturities")
                {
                    extracted.push(build_debt_note_metric(
                        metric,
                        amount,
                        row_label.clone(),
                        filing,
                        provenance_context,
                        table_scale,
                    ));
                }
            }
        }

        extracted
    }

    fn extract_funding_balance_table_metrics(
        &self,
        rows: &[Vec<HtmlTableCell>],
        header_cells: &[String],
        filing: &FilingMetadata,
        provenance_context: &str,
        table_scale: ValueScale,
    ) -> Vec<ExtractedHtmlMetricValue> {
        let normalized_context = normalize_label(provenance_context);
        if !normalized_context.contains("short term unsecured funding") {
            return Vec::new();
        }

        let mut extracted = Vec::new();
        for row in rows {
            let cells: Vec<String> = row.iter().map(|cell| cell.text.clone()).collect();
            if cells.len() < 2 {
                continue;
            }

            let row_label = cells[0].trim().to_string();
            let normalized_row = normalize_label(&row_label);
            if normalized_row.is_empty() {
                continue;
            }

            let Some(metric_id) = debt_detail_base_metric_id(&normalized_row) else {
                continue;
            };
            if metric_id != "debt_and_credit.detail_other_borrowed_funds" {
                continue;
            }

            let Some(numeric_text) = select_numeric_cell_for_filing(&cells, header_cells, filing)
            else {
                continue;
            };
            let Some((amount, _)) = parse_numeric_cell(&numeric_text) else {
                continue;
            };
            let Some(metric) = self.registry.by_id(metric_id) else {
                continue;
            };
            extracted.push(build_debt_note_metric(
                metric,
                amount,
                row_label,
                filing,
                provenance_context,
                table_scale,
            ));
        }

        extracted
    }

    fn extract_narrative_sections_from_document(
        &self,
        document: &Html,
        filing: &FilingMetadata,
    ) -> Vec<ExtractedNarrativeSection> {
        let mut sections = Vec::new();

        for section in collect_heading_sections(document) {
            let normalized_title = normalize_label(&section.title);

            if is_mda_heading(&normalized_title) {
                sections.push(ExtractedNarrativeSection {
                    metric_id: MetricId::new("mda.management_discussion_text"),
                    domain: accounting_domains::DomainName::Mda,
                    value: MetricValue::Text(TextBlock {
                        title: section.title.clone(),
                        content: section.content.clone(),
                        form_type: filing.form_type.clone(),
                        filing_date: filing.filing_date,
                        source_type: SourceType::Html,
                        source_location: SourceLocator {
                            section_name: Some(section.title.clone()),
                            table_name: None,
                            row_label: None,
                            cell_reference: None,
                            segment_name: None,
                        },
                        associated_domain: Some("mda".to_string()),
                    }),
                });
            } else if is_footnote_heading(&normalized_title) {
                sections.push(ExtractedNarrativeSection {
                    metric_id: MetricId::new("footnotes.disclosure_text"),
                    domain: accounting_domains::DomainName::Footnotes,
                    value: MetricValue::Text(TextBlock {
                        title: section.title.clone(),
                        content: section.content.clone(),
                        form_type: filing.form_type.clone(),
                        filing_date: filing.filing_date,
                        source_type: SourceType::Html,
                        source_location: SourceLocator {
                            section_name: Some(section.title.clone()),
                            table_name: None,
                            row_label: None,
                            cell_reference: None,
                            segment_name: None,
                        },
                        associated_domain: Some("footnotes".to_string()),
                    }),
                });
            }
        }

        sections
    }

    fn match_row_label_to_metric<'a>(
        &'a self,
        normalized_row: &str,
        table_context: &str,
    ) -> Option<&'a DomainMetric> {
        self.alias_index
            .get(normalized_row)
            .into_iter()
            .flat_map(|metric_ids| metric_ids.iter())
            .filter_map(|metric_id| self.registry.by_id(metric_id.as_str()))
            .find(|metric| table_context_supports_metric(table_context, metric))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HeadingSection {
    title: String,
    content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RowLabelQuality {
    normalized: String,
}

impl RowLabelQuality {
    fn from_label(row_label: &str) -> Option<Self> {
        let normalized = normalize_label(row_label);

        if normalized.len() < 4 || normalized.len() > 140 {
            return None;
        }

        if normalized.chars().all(|character| character.is_ascii_digit()) {
            return None;
        }

        if is_known_non_metric_label(&normalized) {
            return None;
        }

        Some(Self { normalized })
    }
}

fn build_alias_index(registry: &MetricRegistry) -> HashMap<String, Vec<MetricId>> {
    let mut alias_index = HashMap::new();

    for metric in registry.all().iter().filter(|metric| {
        !matches!(
            metric.definition.domain,
            accounting_domains::DomainName::Footnotes
                | accounting_domains::DomainName::Mda
                | accounting_domains::DomainName::Valuation
                | accounting_domains::DomainName::RiskFactorsSkeleton
                | accounting_domains::DomainName::CompanyOverview
                | accounting_domains::DomainName::FilingIndex
                | accounting_domains::DomainName::Schema
                | accounting_domains::DomainName::Provenance
        )
    }) {
        for alias in html_metric_aliases(metric.definition.metric_id.as_str()) {
            alias_index
                .entry((*alias).to_string())
                .or_insert_with(Vec::new)
                .push(metric.definition.metric_id.clone());
        }
    }

    alias_index
}

fn should_skip_generic_table_extraction(table_context: &str) -> bool {
    let normalized = normalize_label(table_context);
    normalized.contains("reference table")
        || normalized.contains("reference")
        || normalized.contains("assets and liabilities measured at fair value on a recurring basis")
        || normalized.contains("derivative netting adjustments")
        || normalized.contains("fair value hierarchy")
        || normalized.contains("free standing derivative receivables and payables")
}

fn looks_like_debt_note_table(
    table_context: &str,
    rate_column: Option<usize>,
    carrying_value_column: Option<usize>,
) -> bool {
    let normalized = normalize_label(table_context);
    normalized.contains("debt")
        || normalized.contains("borrowings")
        || normalized.contains("notes")
        || normalized.contains("credit")
        || rate_column.is_some()
        || carrying_value_column.is_some()
}

fn debt_metric_columns(rows: &[Vec<HtmlTableCell>]) -> Option<(Option<usize>, Option<usize>)> {
    let mut rate_column = None;
    let mut carrying_value_column = None;

    for row in rows.iter().take(3) {
        for cell in row {
            let normalized = normalize_label(&cell.text);
            if rate_column.is_none()
                && (normalized.contains("effective interest rate") || normalized == "interest rate")
            {
                rate_column = Some(cell.display_column_start);
            }
            if carrying_value_column.is_none() && normalized.contains("carrying value") {
                carrying_value_column = Some(cell.display_column_start);
            }
        }
    }

    if rate_column.is_none() && carrying_value_column.is_none() {
        None
    } else {
        Some((rate_column, carrying_value_column))
    }
}

fn funding_flow_columns(rows: &[Vec<HtmlTableCell>]) -> Option<(Option<usize>, Option<usize>)> {
    let mut issuance_column = None;
    let mut maturities_column = None;

    for row in rows.iter().take(3) {
        for cell in row {
            let normalized = normalize_label(&cell.text);
            if issuance_column.is_none() && normalized.contains("issuance") {
                issuance_column = Some(cell.display_column_start);
            }
            if maturities_column.is_none()
                && (normalized.contains("maturities") || normalized.contains("redemptions"))
            {
                maturities_column = Some(cell.display_column_start);
            }
        }
    }

    if issuance_column.is_none() && maturities_column.is_none() {
        None
    } else {
        Some((issuance_column, maturities_column))
    }
}

fn row_cell_in_display_column<'a>(
    row: &'a [HtmlTableCell],
    display_column: usize,
) -> Option<&'a HtmlTableCell> {
    row.iter().find(|cell| {
        cell.display_column_start <= display_column && cell.display_column_end >= display_column
    })
}

fn is_notes_and_bonds_row(normalized_row: &str) -> bool {
    normalized_row.contains("note")
        || normalized_row.contains("bond")
        || normalized_row.contains("debenture")
}

fn debt_detail_base_metric_id(normalized_row: &str) -> Option<&'static str> {
    if normalized_row.contains("structured note") {
        Some("debt_and_credit.detail_structured_notes")
    } else if normalized_row.contains("subordinated debt")
        || normalized_row.contains("subordinated note")
        || normalized_row.contains("junior subordinated")
        || normalized_row == "subordinated"
    {
        Some("debt_and_credit.detail_subordinated_debt")
    } else if normalized_row.contains("secured borrowing")
        || normalized_row.contains("secured financing")
        || normalized_row.contains("asset backed")
        || normalized_row.contains("asset-backed")
        || normalized_row.contains("federal home loan bank")
        || normalized_row.contains("fhlb")
        || normalized_row.contains("credit card securitization")
    {
        Some("debt_and_credit.detail_secured_borrowings")
    } else if normalized_row.contains("other borrowed funds")
        || normalized_row.contains("other borrowings")
        || normalized_row.contains("borrowed funds")
    {
        Some("debt_and_credit.detail_other_borrowed_funds")
    } else if normalized_row.contains("senior note")
        || normalized_row.contains("senior unsecured note")
        || normalized_row.contains("medium term note")
        || normalized_row.contains("medium-term note")
    {
        Some("debt_and_credit.detail_senior_notes")
    } else {
        None
    }
}

fn debt_detail_flow_metric_id(base: &str, flow: &str) -> Option<&'static str> {
    match (base, flow) {
        ("debt_and_credit.detail_senior_notes", "issuance") => {
            Some("debt_and_credit.detail_senior_notes_issuance")
        }
        ("debt_and_credit.detail_senior_notes", "maturities") => {
            Some("debt_and_credit.detail_senior_notes_maturities")
        }
        ("debt_and_credit.detail_subordinated_debt", "issuance") => {
            Some("debt_and_credit.detail_subordinated_debt_issuance")
        }
        ("debt_and_credit.detail_subordinated_debt", "maturities") => {
            Some("debt_and_credit.detail_subordinated_debt_maturities")
        }
        ("debt_and_credit.detail_other_borrowed_funds", "issuance") => {
            Some("debt_and_credit.detail_other_borrowed_funds_issuance")
        }
        ("debt_and_credit.detail_other_borrowed_funds", "maturities") => {
            Some("debt_and_credit.detail_other_borrowed_funds_maturities")
        }
        ("debt_and_credit.detail_secured_borrowings", "issuance") => {
            Some("debt_and_credit.detail_secured_borrowings_issuance")
        }
        ("debt_and_credit.detail_secured_borrowings", "maturities") => {
            Some("debt_and_credit.detail_secured_borrowings_maturities")
        }
        ("debt_and_credit.detail_structured_notes", "issuance") => {
            Some("debt_and_credit.detail_structured_notes_issuance")
        }
        ("debt_and_credit.detail_structured_notes", "maturities") => {
            Some("debt_and_credit.detail_structured_notes_maturities")
        }
        _ => None,
    }
}

fn debt_detail_metric_id(normalized_row: &str) -> Option<&'static str> {
    let flow_suffix = if normalized_row.contains("issuance")
        || normalized_row.contains("issued")
        || normalized_row.contains("originated")
        || normalized_row.contains("new borrowings")
    {
        Some("issuance")
    } else if normalized_row.contains("maturit")
        || normalized_row.contains("redemption")
        || normalized_row.contains("redeemed")
        || normalized_row.contains("repayment")
        || normalized_row.contains("repayments")
    {
        Some("maturities")
    } else {
        None
    };

    let base = debt_detail_base_metric_id(normalized_row)?;

    match (base, flow_suffix) {
        (base, Some("issuance")) => debt_detail_flow_metric_id(base, "issuance"),
        (base, Some("maturities")) => debt_detail_flow_metric_id(base, "maturities"),
        (base, None) => Some(base),
        _ => None,
    }
}

fn is_revolver_row(normalized_row: &str) -> bool {
    normalized_row.contains("revolving credit") || normalized_row.contains("revolver")
}

fn is_interest_rate_row(normalized_row: &str) -> bool {
    normalized_row.contains("weighted average")
        || normalized_row.contains("fixed rate debt")
        || normalized_row.contains("floating rate debt")
        || normalized_row.contains("current portion of long term debt")
        || normalized_row.contains("short term borrowings")
        || is_notes_and_bonds_row(normalized_row)
        || is_revolver_row(normalized_row)
}

fn is_financial_funding_row(normalized_row: &str) -> bool {
    normalized_row == "long term debt"
        || normalized_row == "short term borrowings"
        || normalized_row == "deposits"
        || normalized_row == "total"
}

fn interest_rate_priority(normalized_row: &str) -> u8 {
    if normalized_row.contains("weighted average") {
        0
    } else if normalized_row == "long term debt" {
        1
    } else if normalized_row.contains("fixed rate debt")
        || normalized_row.contains("floating rate debt")
    {
        2
    } else if is_revolver_row(normalized_row) {
        3
    } else {
        4
    }
}

fn amount_is_plausible_for_metric(metric_id: &str, amount: f64) -> bool {
    if metric_id == "debt_and_credit.interest_rate" {
        // Debt-rate outputs should remain true percentage-like values. If this row is producing a
        // five-digit amount, the extractor likely read a carrying value or notional amount from
        // the wrong column/context. Keep the filter narrow here instead of loosening downstream
        // normalization so future debt-detail work starts from clean rate candidates.
        return amount.abs() <= 100.0;
    }

    true
}

fn looks_like_financial_funding_summary_table(table_context: &str) -> bool {
    let normalized = normalize_label(table_context);
    normalized.contains("long term debt")
        && normalized.contains("short term borrowings")
        && normalized.contains("deposits")
        && normalized.contains("total")
}

fn build_debt_note_metric(
    metric: &DomainMetric,
    amount: f64,
    row_label: String,
    filing: &FilingMetadata,
    provenance_context: &str,
    table_scale: ValueScale,
) -> ExtractedHtmlMetricValue {
    let unit = measurement_unit_from_hint(metric.definition.expected_unit_hint.as_deref());
    let reporting_period = reporting_period_from_filing(filing);
    let provenance = Provenance {
        accession_number: filing.accession_number.clone(),
        filing_url: filing.filing_urls.primary_document.clone(),
        form_type: filing.form_type.clone(),
        source_type: SourceType::Html,
        source_method: FilingSourceMethod::FilingHtml,
        source_location: SourceLocator {
            section_name: if provenance_context.is_empty() {
                None
            } else {
                Some(provenance_context.to_string())
            },
            table_name: if provenance_context.is_empty() {
                None
            } else {
                Some(provenance_context.to_string())
            },
            row_label: Some(row_label.clone()),
            cell_reference: None,
            segment_name: None,
        },
        xbrl_tag: None,
        filing_label: Some(row_label.clone()),
        reporting_period: reporting_period.clone(),
        unit: unit.clone(),
        scale: table_scale,
    };

    ExtractedHtmlMetricValue {
        metric_id: metric.definition.metric_id.clone(),
        metric_name: metric.definition.display_name.clone(),
        domain: metric.definition.domain,
        subdomain: metric.subdomain.clone(),
        numeric_value: NumericValue {
            amount,
            unit,
            scale: table_scale,
            sign_convention: SignConvention::AsReported,
            label: Some(row_label),
            reporting_period,
            provenance,
        },
    }
}

fn select_numeric_cell_for_filing(
    cells: &[String],
    header_cells: &[String],
    filing: &FilingMetadata,
) -> Option<String> {
    if let Some(report_period_end) = filing.report_period_end {
        let report_year = report_period_end.year().to_string();
        let report_date = report_period_end.to_string();

        for (index, header) in header_cells.iter().enumerate().skip(1) {
            let normalized_header = normalize_label(header);
            let header_matches_report_period = normalized_header.contains(&report_date)
                || normalized_header.contains(&report_year);

            if header_matches_report_period {
                if let Some(cell) = cells.get(index) {
                    if parse_numeric_cell(cell).is_some() {
                        return Some(cell.clone());
                    }
                }
            }
        }
    }

    cells.iter().skip(1).find(|cell| parse_numeric_cell(cell).is_some()).cloned()
}

fn table_column_periods(
    structured_rows: &[Vec<HtmlTableCell>],
    filing: &FilingMetadata,
    table_context: &str,
) -> Vec<HtmlColumnPeriod> {
    let mut best_match = Vec::new();
    let allow_bare_annual_headers = table_context_supports_bare_annual_headers(table_context);

    for row in structured_rows.iter().take(4) {
        let periods =
            row.iter()
                .cloned()
                .filter_map(|cell| {
                    if is_reference_header(&cell.text) {
                        return None;
                    }
                    parse_header_reporting_period(&cell.text, filing, allow_bare_annual_headers)
                        .map(|reporting_period| HtmlColumnPeriod {
                            display_column_start: cell.display_column_start,
                            display_column_end: cell.display_column_end,
                            reporting_period,
                        })
                })
                .collect::<Vec<_>>();

        if periods.len() > best_match.len() {
            best_match = periods;
        }
    }

    best_match
}

fn structured_row_cells(row: &ElementRef<'_>, cell_selector: &Selector) -> Vec<HtmlTableCell> {
    let mut display_column_start = 0usize;

    row.select(cell_selector)
        .map(|cell| {
            let colspan = cell
                .value()
                .attr("colspan")
                .and_then(|value| value.parse::<usize>().ok())
                .filter(|value| *value > 0)
                .unwrap_or(1);
            let structured = HtmlTableCell {
                text: text_content(cell),
                display_column_start,
                display_column_end: display_column_start + colspan - 1,
            };
            display_column_start += colspan;
            structured
        })
        .collect()
}

fn collect_structured_table_rows(
    table: &ElementRef<'_>,
    row_selector: &Selector,
    cell_selector: &Selector,
) -> Vec<Vec<HtmlTableCell>> {
    table.select(row_selector).map(|row| structured_row_cells(&row, cell_selector)).collect()
}

fn select_numeric_cell_in_period_span<'a>(
    structured_cells: &'a [HtmlTableCell],
    column_period: &HtmlColumnPeriod,
) -> Option<&'a str> {
    structured_cells
        .iter()
        .skip(1)
        .find(|cell| {
            cell.display_column_start <= column_period.display_column_end
                && cell.display_column_end >= column_period.display_column_start
                && parse_numeric_cell(&cell.text).is_some()
        })
        .map(|cell| cell.text.as_str())
}

fn parse_header_reporting_period(
    header_text: &str,
    filing: &FilingMetadata,
    allow_bare_annual_headers: bool,
) -> Option<ReportingPeriod> {
    let normalized = normalize_label(header_text);
    let end = parse_header_end_date(header_text).or_else(|| {
        parse_bare_annual_year_header(&normalized, filing, allow_bare_annual_headers)
    })?;

    if let Some(months) = explicit_duration_months(&normalized) {
        if let Some(start) = duration_start_from_header_end(end, months) {
            return Some(ReportingPeriod {
                context: PeriodContext::Duration { start, end },
                fiscal_period: None,
                label: Some(header_text.trim().to_string()),
            });
        }

        return None;
    }

    Some(ReportingPeriod {
        context: PeriodContext::Instant { as_of: end },
        fiscal_period: None,
        label: Some(header_text.trim().to_string()),
    })
}

fn is_reference_header(header_text: &str) -> bool {
    normalize_label(header_text).contains("reference")
}

fn parse_header_end_date(header_text: &str) -> Option<time::Date> {
    let normalized = normalize_label(header_text);
    let tokens = normalized.split_whitespace().collect::<Vec<_>>();

    for window in tokens.windows(3) {
        if let Some(month) = month_from_token(window[0]) {
            let day = window[1].parse::<u8>().ok()?;
            let year = window[2].parse::<i32>().ok()?;
            if let Ok(date) = time::Date::from_calendar_date(year, month, day) {
                return Some(date);
            }
        }
    }

    None
}

fn parse_bare_annual_year_header(
    normalized_header: &str,
    filing: &FilingMetadata,
    allow_bare_annual_headers: bool,
) -> Option<time::Date> {
    // Some annual SEC statement tables label comparative columns with bare years such as
    // "2017", "2016", and "2015". We only trust that compact format for annual filings, and we
    // anchor the parsed year to the filing's known fiscal month/day rather than assuming a
    // calendar year-end.
    if !allow_bare_annual_headers {
        return None;
    }

    if !matches!(filing.form_type.as_str(), "10-K" | "20-F" | "40-F") {
        return None;
    }

    if normalized_header.len() != 4
        || !normalized_header.chars().all(|character| character.is_ascii_digit())
    {
        return None;
    }

    let anchor = filing.report_period_end?;
    let year = normalized_header.parse::<i32>().ok()?;

    time::Date::from_calendar_date(year, anchor.month(), anchor.day()).ok()
}

fn table_context_supports_bare_annual_headers(table_context: &str) -> bool {
    let normalized = normalize_label(table_context);

    normalized.contains("statement of income")
        || normalized.contains("statements of income")
        || normalized.contains("statement of operations")
        || normalized.contains("statements of operations")
        || normalized.contains("statement of earnings")
        || normalized.contains("statements of earnings")
        || normalized.contains("balance sheet")
        || normalized.contains("balance sheets")
        || normalized.contains("statement of financial position")
        || normalized.contains("statements of financial position")
        || normalized.contains("statement of cash flows")
        || normalized.contains("statements of cash flows")
        || normalized.contains("statement of shareholders equity")
        || normalized.contains("statements of shareholders equity")
        || normalized.contains("statement of stockholders equity")
        || normalized.contains("statements of stockholders equity")
}

fn infer_table_context(structured_rows: &[Vec<HtmlTableCell>]) -> String {
    structured_rows
        .iter()
        .take(3)
        .flat_map(|row| row.iter().cloned())
        .map(|cell| cell.text)
        .filter(|text| {
            let normalized = normalize_label(text);
            !normalized.is_empty()
                && !normalized.chars().all(|character| character.is_ascii_digit())
                && !normalized.contains("in millions")
                && !normalized.contains("in thousands")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn metric_allows_percentage_values(metric: &DomainMetric) -> bool {
    matches!(metric.definition.expected_unit_hint.as_deref(), Some("percentage") | Some("ratio"))
}

fn cell_looks_like_percentage(value: &str) -> bool {
    value.contains('%')
}

fn explicit_duration_months(normalized_header: &str) -> Option<u8> {
    if normalized_header.contains("year ended") {
        Some(12)
    } else if normalized_header.contains("three months ended") {
        Some(3)
    } else if normalized_header.contains("six months ended") {
        Some(6)
    } else if normalized_header.contains("nine months ended") {
        Some(9)
    } else if normalized_header.contains("twelve months ended") {
        Some(12)
    } else {
        None
    }
}

fn duration_start_from_header_end(end: time::Date, months: u8) -> Option<time::Date> {
    if !is_month_end(end) {
        return None;
    }

    let end_month = month_number(end.month()) as i32;
    let total_month_index = end.year() * 12 + end_month - i32::from(months) + 1;
    let start_year = (total_month_index - 1).div_euclid(12);
    let start_month_number = (total_month_index - 1).rem_euclid(12) + 1;
    let start_month = month_from_number(start_month_number as u8)?;

    time::Date::from_calendar_date(start_year, start_month, 1).ok()
}

fn is_month_end(date: time::Date) -> bool {
    let next_day = date.day().saturating_add(1);
    time::Date::from_calendar_date(date.year(), date.month(), next_day).is_err()
}

fn month_number(month: time::Month) -> u8 {
    match month {
        time::Month::January => 1,
        time::Month::February => 2,
        time::Month::March => 3,
        time::Month::April => 4,
        time::Month::May => 5,
        time::Month::June => 6,
        time::Month::July => 7,
        time::Month::August => 8,
        time::Month::September => 9,
        time::Month::October => 10,
        time::Month::November => 11,
        time::Month::December => 12,
    }
}

fn month_from_number(value: u8) -> Option<time::Month> {
    match value {
        1 => Some(time::Month::January),
        2 => Some(time::Month::February),
        3 => Some(time::Month::March),
        4 => Some(time::Month::April),
        5 => Some(time::Month::May),
        6 => Some(time::Month::June),
        7 => Some(time::Month::July),
        8 => Some(time::Month::August),
        9 => Some(time::Month::September),
        10 => Some(time::Month::October),
        11 => Some(time::Month::November),
        12 => Some(time::Month::December),
        _ => None,
    }
}

fn month_from_token(value: &str) -> Option<time::Month> {
    match value {
        "january" | "jan" => Some(time::Month::January),
        "february" | "feb" => Some(time::Month::February),
        "march" | "mar" => Some(time::Month::March),
        "april" | "apr" => Some(time::Month::April),
        "may" => Some(time::Month::May),
        "june" | "jun" => Some(time::Month::June),
        "july" | "jul" => Some(time::Month::July),
        "august" | "aug" => Some(time::Month::August),
        "september" | "sep" | "sept" => Some(time::Month::September),
        "october" | "oct" => Some(time::Month::October),
        "november" | "nov" => Some(time::Month::November),
        "december" | "dec" => Some(time::Month::December),
        _ => None,
    }
}

fn is_known_non_metric_label(normalized_row: &str) -> bool {
    normalized_row.starts_with("year ended")
        || normalized_row.starts_with("three months ended")
        || normalized_row.starts_with("six months ended")
        || normalized_row.starts_with("nine months ended")
        || normalized_row.starts_with("twelve months ended")
        || normalized_row.contains("in millions")
        || normalized_row.contains("in thousands")
        || normalized_row.contains("except per share")
        || normalized_row.contains("unaudited")
}

fn table_context_supports_metric(table_context: &str, metric: &DomainMetric) -> bool {
    let context = normalize_label(table_context);
    if context.is_empty() {
        return true;
    }

    if context.contains("percent of net sales") && !metric_allows_percentage_values(metric) {
        return false;
    }

    match metric.definition.domain {
        accounting_domains::DomainName::BalanceSheet => {
            contains_any(&context, &["balance sheet", "financial position"])
        }
        accounting_domains::DomainName::IncomeStatement => contains_any(
            &context,
            &[
                "statement of income",
                "statements of income",
                "statement of operations",
                "statements of operations",
                "results of operations",
                "earnings",
            ],
        ),
        accounting_domains::DomainName::CashFlow => {
            contains_any(&context, &["cash flow", "cash flows", "statements of cash"])
        }
        accounting_domains::DomainName::ShareholdersEquity => {
            contains_any(&context, &["equity", "stockholders", "shareholders"])
        }
        accounting_domains::DomainName::DebtAndCredit => {
            contains_any(&context, &["debt", "credit", "borrowings", "notes"])
        }
        accounting_domains::DomainName::DerivativesAndSecurities => {
            contains_any(&context, &["derivative", "securities", "fair value", "hedging"])
        }
        accounting_domains::DomainName::EquityCompensation => contains_any(
            &context,
            &["share based", "stock based", "equity compensation", "stock compensation"],
        ),
        _ => true,
    }
}

fn contains_any(value: &str, candidates: &[&str]) -> bool {
    candidates.iter().any(|candidate| value.contains(candidate))
}

fn html_metric_aliases(metric_id: &str) -> &'static [&'static str] {
    match metric_id {
        "balance_sheet.cash_and_equivalents" => {
            &["cash and cash equivalents", "cash cash equivalents", "cash and equivalents"]
        }
        "balance_sheet.accounts_receivable" => &[
            "accounts receivable",
            "accounts receivable net",
            "trade accounts receivable",
            "receivables net",
        ],
        "balance_sheet.inventory" => &["inventory", "inventories"],
        "balance_sheet.property_plant_equipment" => &[
            "property and equipment net",
            "property plant and equipment net",
            "property plant and equipment",
        ],
        "balance_sheet.goodwill_and_intangibles" => &[
            "goodwill and intangible assets",
            "goodwill and other intangible assets",
            "intangible assets net",
            "goodwill",
        ],
        "balance_sheet.total_assets" => &["total assets"],
        "balance_sheet.current_debt" => &[
            "current maturities of long term debt",
            "current portion of long term debt",
            "short term borrowings",
        ],
        "balance_sheet.long_term_debt" => &[
            "long term debt",
            "long term debt less current maturities",
            "long term debt excluding current maturities",
        ],
        "balance_sheet.total_liabilities" => &["total liabilities"],
        "balance_sheet.total_equity" => {
            &["total equity", "total shareholders equity", "total stockholders equity"]
        }
        "income_statement.revenue" => &["revenue", "total revenue", "revenues", "total revenues"],
        "income_statement.cost_of_goods_sold" => {
            &["cost of goods sold", "cost of sales", "cost of revenues", "cost of revenue"]
        }
        "income_statement.gross_profit" => &["gross profit"],
        "income_statement.operating_expenses" => {
            &["operating expenses", "total operating expenses"]
        }
        "income_statement.operating_income" => {
            &["operating income", "operating income loss", "income from operations"]
        }
        "income_statement.interest_expense" => &["interest expense"],
        "income_statement.tax_expense" => {
            &["income tax expense", "provision for income taxes", "income tax provision"]
        }
        "income_statement.net_income" => &["net income", "net earnings", "net income loss"],
        "income_statement.diluted_eps" => &[
            "diluted earnings per share",
            "earnings per share diluted",
            "diluted net income per share",
        ],
        "cash_flow.net_cash_from_operations" => &[
            "net cash provided by operating activities",
            "net cash provided by used in operating activities",
            "net cash from operating activities",
        ],
        "cash_flow.depreciation_and_amortization" => {
            &["depreciation and amortization", "depreciation depletion and amortization"]
        }
        "cash_flow.capital_expenditures" => &[
            "capital expenditures",
            "capital expenditures including capitalization of software costs",
            "payments to acquire property and equipment",
        ],
        "cash_flow.net_cash_from_investing" => &[
            "net cash used in investing activities",
            "net cash provided by used in investing activities",
            "net cash from investing activities",
        ],
        "cash_flow.net_cash_from_financing" => &[
            "net cash provided by financing activities",
            "net cash used in financing activities",
            "net cash provided by used in financing activities",
            "net cash from financing activities",
        ],
        "cash_flow.stock_repurchases" => {
            &["repurchases of common stock", "common stock repurchased", "treasury stock purchases"]
        }
        "cash_flow.share_issuance_proceeds" => {
            &["proceeds from stock options exercised", "proceeds from issuance of common stock"]
        }
        "shareholders_equity.common_stock" => &["common stock"],
        "shareholders_equity.additional_paid_in_capital" => &["additional paid in capital"],
        "shareholders_equity.retained_earnings" => {
            &["retained earnings", "retained earnings accumulated deficit"]
        }
        "shareholders_equity.treasury_stock" => &["treasury stock"],
        "shareholders_equity.accumulated_oci" => {
            &["accumulated other comprehensive income", "accumulated other comprehensive loss"]
        }
        "shareholders_equity.shares_outstanding" => &["shares outstanding"],
        "debt_and_credit.revolver_balance" => &[
            "revolver balance",
            "revolving credit facility",
            "revolving credit borrowings",
            "borrowings under the revolving credit facility",
            "revolving credit agreement borrowings",
        ],
        "debt_and_credit.term_loan_balance" => &["term loan", "term loan balance"],
        "debt_and_credit.notes_and_bonds" => &[
            "notes and bonds",
            "senior notes",
            "senior unsecured notes",
            "medium term notes",
            "notes payable",
            "bonds payable",
        ],
        "debt_and_credit.debt_maturities" => {
            &["current maturities of long term debt", "long term debt maturities"]
        }
        "debt_and_credit.interest_rate" => &[
            "interest rate",
            "weighted average interest rate",
            "weighted average debt interest rate",
            "weighted average rate",
        ],
        "derivatives_and_securities.derivative_fair_value" => {
            &["derivative assets", "derivative liabilities", "fair value of derivatives"]
        }
        "derivatives_and_securities.derivative_gain_loss" => &[
            "derivative gain loss",
            "gain loss on derivatives",
            "gain loss recognized in income on derivatives",
            "gain loss recognized in income on derivative instruments",
        ],
        "derivatives_and_securities.debt_securities_value" => &[
            "available for sale debt securities",
            "debt securities available for sale",
            "available for sale debt securities at fair value",
            "debt securities available for sale at fair value",
        ],
        "equity_compensation.stock_based_comp_expense" => &[
            "stock based compensation expense",
            "share based compensation expense",
            "stock compensation expense",
        ],
        "equity_compensation.rsu_activity" => &["restricted stock units", "rsu activity"],
        "equity_compensation.option_activity" => &["stock options", "option activity"],
        "equity_compensation.tax_effects" => &["tax benefit realized", "excess tax benefits"],
        "equity_compensation.shares_repurchased" => &["shares repurchased"],
        "equity_compensation.net_change_shares_outstanding" => {
            &["net change in shares outstanding", "increase decrease in common shares outstanding"]
        }
        _ => &[],
    }
}

fn normalize_label(value: &str) -> String {
    value
        .chars()
        .map(
            |character| {
                if character.is_ascii_alphanumeric() { character.to_ascii_lowercase() } else { ' ' }
            },
        )
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_numeric_cell(value: &str) -> Option<(f64, SignConvention)> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    let is_negative = trimmed.starts_with('(') && trimmed.ends_with(')');
    let cleaned = trimmed
        .trim_start_matches('(')
        .trim_end_matches(')')
        .replace(',', "")
        .replace('$', "")
        .replace('%', "")
        .replace('\u{a0}', " ")
        .trim()
        .to_string();

    if cleaned.is_empty() {
        return None;
    }

    let parsed = cleaned.parse::<f64>().ok()?;
    Some((if is_negative { -parsed } else { parsed }, SignConvention::AsReported))
}

fn infer_scale(table_context: &str) -> ValueScale {
    let normalized = normalize_label(table_context);
    if normalized.contains("in billions") {
        ValueScale::Billions
    } else if normalized.contains("in millions") {
        ValueScale::Millions
    } else if normalized.contains("in thousands") {
        ValueScale::Thousands
    } else {
        ValueScale::Raw
    }
}

fn measurement_unit_from_hint(unit_hint: Option<&str>) -> MeasurementUnit {
    match unit_hint {
        Some("USD") => MeasurementUnit::Currency("USD".to_string()),
        Some("shares") => MeasurementUnit::Shares,
        Some("ratio") => MeasurementUnit::Ratio,
        Some("percentage") => MeasurementUnit::Percentage,
        Some("text") => MeasurementUnit::Text,
        Some(other) => MeasurementUnit::Other(other.to_string()),
        None => MeasurementUnit::Currency("USD".to_string()),
    }
}

fn reporting_period_from_filing(filing: &FilingMetadata) -> ReportingPeriod {
    let as_of = filing.report_period_end.unwrap_or(filing.filing_date);
    ReportingPeriod {
        context: PeriodContext::Instant { as_of },
        fiscal_period: filing.fiscal_period.clone(),
        label: Some(filing.form_type.as_str().to_string()),
    }
}

fn collect_heading_sections(document: &Html) -> Vec<HeadingSection> {
    let selector = selector("h1, h2, h3, h4, h5, h6, p, li, div, span");
    let mut sections: Vec<HeadingSection> = Vec::new();
    let mut current_title: Option<String> = None;
    let mut current_content: Vec<String> = Vec::new();

    for element in document.select(&selector) {
        let tag_name = element.value().name();
        let text = text_content(element);
        if text.is_empty() {
            continue;
        }

        if is_heading_element(tag_name) || looks_like_section_heading(&text) {
            if let Some(title) = current_title.take() {
                if !current_content.is_empty() {
                    sections.push(HeadingSection { title, content: current_content.join("\n\n") });
                }
            }

            current_title = Some(text);
            current_content = Vec::new();
        } else if current_title.is_some() {
            current_content.push(text);
        }
    }

    if let Some(title) = current_title {
        if !current_content.is_empty() {
            sections.push(HeadingSection { title, content: current_content.join("\n\n") });
        }
    }

    sections
}

fn is_footnote_heading(normalized_title: &str) -> bool {
    normalized_title.starts_with("note ")
        || normalized_title.contains("footnote")
        || normalized_title.contains("notes to consolidated financial statements")
        || normalized_title.contains("notes to financial statements")
}

fn is_mda_heading(normalized_title: &str) -> bool {
    normalized_title.contains("management s discussion")
        || normalized_title.contains("management discussion")
        || normalized_title.contains("management s discussion and analysis")
        || normalized_title.contains("item 7 management")
        || normalized_title.contains("item 2 management")
        || normalized_title.contains("discussion and analysis of financial condition")
}

fn is_heading_element(tag_name: &str) -> bool {
    matches!(tag_name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
}

fn looks_like_section_heading(text: &str) -> bool {
    let normalized = normalize_label(text);

    if normalized.len() > 180 {
        return false;
    }

    is_mda_heading(&normalized)
        || is_footnote_heading(&normalized)
        || normalized.starts_with("item 7 ")
        || normalized.starts_with("item 8 ")
        || normalized.starts_with("item 2 ")
}

fn text_content(element: ElementRef<'_>) -> String {
    element.text().collect::<Vec<_>>().join(" ").split_whitespace().collect::<Vec<_>>().join(" ")
}

fn selector(query: &str) -> Selector {
    Selector::parse(query).expect("valid selector")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InlineXbrlNonFractionFact {
    name: String,
    context_ref: String,
    unit_ref: Option<String>,
    scale: Option<String>,
    value: String,
}

#[derive(Debug, Clone)]
struct InlineXbrlSegmentContext {
    segment_name: String,
    reporting_period: Option<ReportingPeriod>,
}

#[derive(Debug, Clone)]
struct InlineXbrlDerivativeContext {
    reporting_period: Option<ReportingPeriod>,
}

#[derive(Debug, Clone)]
struct InlineXbrlEquityCompContext {
    reporting_period: Option<ReportingPeriod>,
}

fn inline_xbrl_segment_contexts(
    html: &str,
) -> std::collections::BTreeMap<String, InlineXbrlSegmentContext> {
    let mut contexts = std::collections::BTreeMap::new();
    let mut search_start = 0;

    while let Some(context_index) = html[search_start..].find("<xbrli:context id=\"") {
        let context_index = search_start + context_index;
        let id_start = context_index + "<xbrli:context id=\"".len();
        let Some(id_end_rel) = html[id_start..].find('"') else {
            break;
        };
        let id_end = id_start + id_end_rel;
        let context_id = &html[id_start..id_end];
        let Some(close_rel) = html[id_end..].find("</xbrli:context>") else {
            break;
        };
        let context_end = id_end + close_rel;
        let body = &html[id_end..context_end];
        if !body.contains("StatementBusinessSegmentsAxis") {
            search_start = context_end + "</xbrli:context>".len();
            continue;
        }
        if body.contains("ProductOrServiceAxis") {
            search_start = context_end + "</xbrli:context>".len();
            continue;
        }

        let explicit_members = extract_explicit_members(body);
        let Some(member) = explicit_members
            .iter()
            .find(|(dimension, _)| dimension.contains("StatementBusinessSegmentsAxis"))
            .map(|(_, member)| member.clone())
        else {
            search_start = context_end + "</xbrli:context>".len();
            continue;
        };

        if !member.contains("SegmentMember") {
            search_start = context_end + "</xbrli:context>".len();
            continue;
        }

        if explicit_members.iter().any(|(dimension, _)| {
            !dimension.contains("StatementBusinessSegmentsAxis")
                && !dimension.contains("ConsolidationItemsAxis")
        }) {
            search_start = context_end + "</xbrli:context>".len();
            continue;
        }

        contexts.insert(
            context_id.to_string(),
            InlineXbrlSegmentContext {
                segment_name: humanize_inline_member_name(&member),
                reporting_period: reporting_period_from_inline_context_body(body),
            },
        );
        search_start = context_end + "</xbrli:context>".len();
    }

    contexts
}

fn inline_xbrl_core_contexts(
    html: &str,
) -> std::collections::BTreeMap<String, Option<ReportingPeriod>> {
    let mut contexts = std::collections::BTreeMap::new();
    let mut search_start = 0;

    while let Some(context_index) = html[search_start..].find("<xbrli:context id=\"") {
        let context_index = search_start + context_index;
        let id_start = context_index + "<xbrli:context id=\"".len();
        let Some(id_end_rel) = html[id_start..].find('"') else {
            break;
        };
        let id_end = id_start + id_end_rel;
        let context_id = &html[id_start..id_end];
        let Some(close_rel) = html[id_end..].find("</xbrli:context>") else {
            break;
        };
        let context_end = id_end + close_rel;
        let body = &html[id_end..context_end];

        if !extract_explicit_members(body).is_empty() {
            search_start = context_end + "</xbrli:context>".len();
            continue;
        }

        contexts.insert(context_id.to_string(), reporting_period_from_inline_context_body(body));
        search_start = context_end + "</xbrli:context>".len();
    }

    contexts
}

fn core_inline_xbrl_matches_current_filing_period(
    reporting_period: &ReportingPeriod,
    filing: &FilingMetadata,
) -> bool {
    let Some(report_end) = filing.report_period_end else {
        return true;
    };

    match (&reporting_period.context, &filing.form_type) {
        (PeriodContext::Duration { start, end }, filing_models::FilingForm::Form10Q) => {
            *end == report_end && (*end - *start).whole_days() <= 100
        }
        (PeriodContext::Duration { start, end }, filing_models::FilingForm::Form10K) => {
            *end == report_end && (*end - *start).whole_days() >= 300
        }
        (PeriodContext::Instant { as_of }, _) => *as_of == report_end,
        (PeriodContext::Duration { end, .. }, _) => *end == report_end,
    }
}

fn inline_xbrl_derivative_contexts(
    html: &str,
) -> std::collections::BTreeMap<String, InlineXbrlDerivativeContext> {
    let mut contexts = std::collections::BTreeMap::new();
    let mut search_start = 0;

    while let Some(context_index) = html[search_start..].find("<xbrli:context id=\"") {
        let context_index = search_start + context_index;
        let id_start = context_index + "<xbrli:context id=\"".len();
        let Some(id_end_rel) = html[id_start..].find('"') else {
            break;
        };
        let id_end = id_start + id_end_rel;
        let context_id = &html[id_start..id_end];
        let Some(close_rel) = html[id_end..].find("</xbrli:context>") else {
            break;
        };
        let context_end = id_end + close_rel;
        let body = &html[id_end..context_end];
        let explicit_members = extract_explicit_members(body);
        if explicit_members.is_empty() {
            search_start = context_end + "</xbrli:context>".len();
            continue;
        }

        let has_derivative_axis = explicit_members.iter().any(|(dimension, _)| {
            dimension.contains("DerivativeInstrumentRiskAxis")
                || dimension.contains("DerivativeInstrumentsGainLossByHedgingRelationshipAxis")
                || dimension.contains("HedgingDesignationAxis")
        });
        if !has_derivative_axis {
            search_start = context_end + "</xbrli:context>".len();
            continue;
        }

        if explicit_members.iter().any(|(dimension, _)| {
            !dimension.contains("DerivativeInstrumentRiskAxis")
                && !dimension.contains("DerivativeInstrumentsGainLossByHedgingRelationshipAxis")
                && !dimension.contains("HedgingDesignationAxis")
                && !dimension.contains("IncomeStatementLocationAxis")
        }) {
            search_start = context_end + "</xbrli:context>".len();
            continue;
        }

        contexts.insert(
            context_id.to_string(),
            InlineXbrlDerivativeContext {
                reporting_period: reporting_period_from_inline_context_body(body),
            },
        );
        search_start = context_end + "</xbrli:context>".len();
    }

    contexts
}

fn inline_xbrl_equity_comp_contexts(
    html: &str,
) -> std::collections::BTreeMap<String, InlineXbrlEquityCompContext> {
    let mut contexts = std::collections::BTreeMap::new();
    let mut search_start = 0;

    while let Some(context_index) = html[search_start..].find("<xbrli:context id=\"") {
        let context_index = search_start + context_index;
        let id_start = context_index + "<xbrli:context id=\"".len();
        let Some(id_end_rel) = html[id_start..].find('"') else {
            break;
        };
        let id_end = id_start + id_end_rel;
        let context_id = &html[id_start..id_end];
        let Some(close_rel) = html[id_end..].find("</xbrli:context>") else {
            break;
        };
        let context_end = id_end + close_rel;
        let body = &html[id_end..context_end];
        let explicit_members = extract_explicit_members(body);
        if explicit_members.is_empty() {
            search_start = context_end + "</xbrli:context>".len();
            continue;
        }

        let has_award_axis =
            explicit_members.iter().any(|(dimension, _)| dimension.contains("AwardTypeAxis"));
        if !has_award_axis {
            search_start = context_end + "</xbrli:context>".len();
            continue;
        }

        if explicit_members.iter().any(|(dimension, _)| !dimension.contains("AwardTypeAxis")) {
            search_start = context_end + "</xbrli:context>".len();
            continue;
        }

        contexts.insert(
            context_id.to_string(),
            InlineXbrlEquityCompContext {
                reporting_period: reporting_period_from_inline_context_body(body),
            },
        );
        search_start = context_end + "</xbrli:context>".len();
    }

    contexts
}

fn extract_explicit_members(body: &str) -> Vec<(String, String)> {
    let mut members = Vec::new();
    let mut search_start = 0;
    while let Some(member_index) = body[search_start..].find("<xbrldi:explicitMember ") {
        let member_index = search_start + member_index;
        let Some(tag_end_rel) = body[member_index..].find('>') else {
            break;
        };
        let tag_end = member_index + tag_end_rel;
        let tag = &body[member_index..=tag_end];
        let Some(dimension) = extract_attr(tag, "dimension") else {
            search_start = tag_end + 1;
            continue;
        };
        let value_start = tag_end + 1;
        let Some(value_end_rel) = body[value_start..].find("</xbrldi:explicitMember>") else {
            break;
        };
        let value_end = value_start + value_end_rel;
        members.push((dimension, body[value_start..value_end].trim().to_string()));
        search_start = value_end + "</xbrldi:explicitMember>".len();
    }
    members
}

fn humanize_inline_member_name(member: &str) -> String {
    let member = member.rsplit(':').next().unwrap_or(member);
    let member = member.strip_suffix("Member").unwrap_or(member);
    humanize_camel_case(member)
}

fn humanize_camel_case(value: &str) -> String {
    let mut humanized = String::with_capacity(value.len() + 8);
    for (index, ch) in value.chars().enumerate() {
        if index > 0 && ch.is_ascii_uppercase() {
            humanized.push(' ');
        }
        humanized.push(ch);
    }
    humanized
}

fn inline_xbrl_nonfraction_facts(html: &str) -> Vec<InlineXbrlNonFractionFact> {
    let mut facts = Vec::new();
    let mut search_start = 0;

    while let Some(tag_index) = html[search_start..].find("<ix:nonFraction ") {
        let tag_index = search_start + tag_index;
        let Some(tag_end_rel) = html[tag_index..].find('>') else {
            break;
        };
        let tag_end = tag_index + tag_end_rel;
        let attrs = &html[tag_index..=tag_end];
        let content_start = tag_end + 1;
        let Some(close_rel) = html[content_start..].find("</ix:nonFraction>") else {
            break;
        };
        let content_end = content_start + close_rel;

        let name = extract_attr(attrs, "name");
        let context_ref = extract_attr(attrs, "contextRef");
        if let (Some(name), Some(context_ref)) = (name, context_ref) {
            let value = html[content_start..content_end].replace("&nbsp;", " ");
            facts.push(InlineXbrlNonFractionFact {
                name,
                context_ref,
                unit_ref: extract_attr(attrs, "unitRef"),
                scale: extract_attr(attrs, "scale"),
                value: strip_html_tags(&value).trim().to_string(),
            });
        }

        search_start = content_end + "</ix:nonFraction>".len();
    }

    facts
}

fn extract_attr(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let end = start + tag[start..].find('"')?;
    Some(tag[start..end].to_string())
}

fn strip_html_tags(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut in_tag = false;
    for ch in value.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => output.push(ch),
            _ => {}
        }
    }
    output
}

fn inline_xbrl_segment_metric_id(name: &str) -> Option<&'static str> {
    let local = name.rsplit(':').next().unwrap_or(name);
    match local {
        "RevenueFromContractWithCustomerExcludingAssessedTax" | "Revenues" => {
            Some("segment_data.segment_revenue")
        }
        "OperatingIncomeLoss" => Some("segment_data.segment_profit_or_loss"),
        "Assets" => Some("segment_data.segment_assets"),
        _ => None,
    }
}

fn inline_xbrl_derivative_metric_id(name: &str) -> Option<&'static str> {
    let local = name.rsplit(':').next().unwrap_or(name);
    match local {
        "DerivativeGainLossOnDerivativeNet"
        | "DerivativeInstrumentsNotDesignatedAsHedgingInstrumentsGainLossNet" => {
            Some("derivatives_and_securities.derivative_gain_loss")
        }
        _ => None,
    }
}

fn inline_xbrl_equity_comp_metric_id(name: &str) -> Option<&'static str> {
    let local = name.rsplit(':').next().unwrap_or(name);
    match local {
        "ProceedsFromStockOptionsExercised" => Some("cash_flow.share_issuance_proceeds"),
        _ => None,
    }
}

fn reporting_period_from_inline_context_body(body: &str) -> Option<ReportingPeriod> {
    let instant = extract_tag_text(body, "xbrli:instant");
    let start = extract_tag_text(body, "xbrli:startDate");
    let end = extract_tag_text(body, "xbrli:endDate");

    if let Some(instant) = instant {
        let as_of =
            time::Date::parse(&instant, &time::format_description::well_known::Iso8601::DATE)
                .ok()?;
        return Some(ReportingPeriod {
            context: PeriodContext::Instant { as_of },
            fiscal_period: None,
            label: None,
        });
    }

    let (Some(start), Some(end)) = (start, end) else {
        return None;
    };
    let start =
        time::Date::parse(&start, &time::format_description::well_known::Iso8601::DATE).ok()?;
    let end = time::Date::parse(&end, &time::format_description::well_known::Iso8601::DATE).ok()?;
    Some(ReportingPeriod {
        context: PeriodContext::Duration { start, end },
        fiscal_period: None,
        label: None,
    })
}

fn reporting_period_from_inline_context_id(context_ref: &str) -> Option<ReportingPeriod> {
    let suffix = context_ref.rsplit('_').next()?;
    if let Some(instant) = suffix.strip_prefix('I') {
        let as_of =
            time::Date::parse(instant, &time::format_description::well_known::Iso8601::DATE)
                .ok()?;
        return Some(ReportingPeriod {
            context: PeriodContext::Instant { as_of },
            fiscal_period: None,
            label: None,
        });
    }

    let duration = suffix.strip_prefix('D')?;
    let (start, end) = duration.split_once('-')?;
    let start =
        time::Date::parse(start, &time::format_description::well_known::Iso8601::DATE).ok()?;
    let end = time::Date::parse(end, &time::format_description::well_known::Iso8601::DATE).ok()?;
    Some(ReportingPeriod {
        context: PeriodContext::Duration { start, end },
        fiscal_period: None,
        label: None,
    })
}

fn extract_tag_text(body: &str, tag_name: &str) -> Option<String> {
    let open = format!("<{tag_name}>");
    let close = format!("</{tag_name}>");
    let start = body.find(&open)? + open.len();
    let end = start + body[start..].find(&close)?;
    Some(body[start..end].trim().to_string())
}

fn measurement_unit_from_inline_unit_ref(unit_ref: Option<&str>) -> MeasurementUnit {
    match unit_ref {
        Some("usd") | Some("USD") => MeasurementUnit::Currency("USD".to_string()),
        Some("shares") => MeasurementUnit::Shares,
        Some("pure") => MeasurementUnit::Ratio,
        Some(other) if other.to_ascii_uppercase().contains("USD") => {
            MeasurementUnit::Currency("USD".to_string())
        }
        Some(other) => MeasurementUnit::Other(other.to_string()),
        None => MeasurementUnit::Currency("USD".to_string()),
    }
}

fn inline_xbrl_scale_from_attr(scale: Option<&str>) -> ValueScale {
    match scale {
        Some("9") => ValueScale::Billions,
        Some("6") => ValueScale::Millions,
        Some("3") => ValueScale::Thousands,
        _ => ValueScale::Raw,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filing_models::{FilingForm, FilingUrls, SourceType};
    use time::macros::date;

    fn sample_filing() -> FilingMetadata {
        FilingMetadata {
            accession_number: "0000798354-25-000010".to_string(),
            form_type: FilingForm::Form10K,
            filing_date: date!(2025 - 02 - 01),
            report_period_end: Some(date!(2024 - 12 - 31)),
            fiscal_period: None,
            filing_urls: FilingUrls {
                filing_detail: None,
                primary_document: Some("https://example.test/10k.htm".to_string()),
                xbrl_instance: None,
                html_index: None,
            },
            source_types: vec![SourceType::Html],
            is_amendment: false,
        }
    }

    fn sample_quarterly_filing() -> FilingMetadata {
        FilingMetadata {
            accession_number: "0000320193-25-000050".to_string(),
            form_type: FilingForm::Form10Q,
            filing_date: date!(2025 - 04 - 25),
            report_period_end: Some(date!(2025 - 03 - 31)),
            fiscal_period: Some(filing_models::FiscalPeriod {
                fiscal_year: 2025,
                fiscal_quarter: Some(filing_models::FiscalQuarter::Q1),
            }),
            filing_urls: FilingUrls {
                filing_detail: None,
                primary_document: Some("https://example.test/10q.htm".to_string()),
                xbrl_instance: None,
                html_index: None,
            },
            source_types: vec![SourceType::Html],
            is_amendment: false,
        }
    }

    #[test]
    fn collect_structured_table_rows_preserves_row_structure_and_colspans() {
        let document = Html::parse_document(
            r#"
            <html>
              <body>
                <table>
                  <tr>
                    <th>Label</th>
                    <th colspan="2">December 31, 2024</th>
                  </tr>
                  <tr>
                    <th>Cash and cash equivalents</th>
                    <td>$</td>
                    <td>415</td>
                  </tr>
                </table>
              </body>
            </html>
            "#,
        );
        let table_selector = selector("table");
        let row_selector = selector("tr");
        let cell_selector = selector("th, td");
        let table = document.select(&table_selector).next().expect("table should exist");

        let rows = collect_structured_table_rows(&table, &row_selector, &cell_selector);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][1].text, "December 31, 2024");
        assert_eq!(rows[0][1].display_column_start, 1);
        assert_eq!(rows[0][1].display_column_end, 2);
        assert_eq!(rows[1][2].text, "415");
    }

    #[test]
    fn extracts_multiple_periods_from_explicit_balance_sheet_headers() {
        let extractor = HtmlExtractor::default();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Consolidated Balance Sheets (in millions)</caption>
                  <tr>
                    <th></th>
                    <th>December 31, 2024</th>
                    <th>December 31, 2023</th>
                  </tr>
                  <tr>
                    <th>Cash and cash equivalents</th>
                    <td>415</td>
                    <td>325</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &sample_filing())
            .expect("html fallback extraction should succeed");

        let cash_values = extracted
            .iter()
            .filter(|metric| metric.metric_id.as_str() == "balance_sheet.cash_and_equivalents")
            .collect::<Vec<_>>();

        assert_eq!(cash_values.len(), 2);
        assert!(cash_values.iter().any(|metric| {
            metric.numeric_value.amount == 415.0
                && matches!(
                    metric.numeric_value.reporting_period.context,
                    PeriodContext::Instant { as_of } if as_of == date!(2024 - 12 - 31)
                )
        }));
        assert!(cash_values.iter().any(|metric| {
            metric.numeric_value.amount == 325.0
                && matches!(
                    metric.numeric_value.reporting_period.context,
                    PeriodContext::Instant { as_of } if as_of == date!(2023 - 12 - 31)
                )
        }));
    }

    #[test]
    fn extracts_segment_metrics_from_inline_xbrl_contexts() {
        let extractor = HtmlExtractor::default();
        let filing = sample_quarterly_filing();
        let html = r#"
            <html>
              <body>
                <div style="display:none">
                  <ix:resources>
                    <xbrli:context id="seg_consumer_D2025-01-01-2025-03-31">
                      <xbrli:entity>
                        <xbrli:segment>
                          <xbrldi:explicitMember dimension="us-gaap:StatementBusinessSegmentsAxis">mmm:ConsumerSegmentMember</xbrldi:explicitMember>
                        </xbrli:segment>
                      </xbrli:entity>
                    </xbrli:context>
                    <xbrli:context id="seg_industrial_D2025-01-01-2025-03-31">
                      <xbrli:entity>
                        <xbrli:segment>
                          <xbrldi:explicitMember dimension="us-gaap:StatementBusinessSegmentsAxis">mmm:SafetyAndIndustrialSegmentMember</xbrldi:explicitMember>
                          <xbrldi:explicitMember dimension="srt:ConsolidationItemsAxis">us-gaap:OperatingSegmentsMember</xbrldi:explicitMember>
                        </xbrli:segment>
                      </xbrli:entity>
                    </xbrli:context>
                    <xbrli:context id="seg_consumer_I2025-03-31">
                      <xbrli:entity>
                        <xbrli:segment>
                          <xbrldi:explicitMember dimension="us-gaap:StatementBusinessSegmentsAxis">mmm:ConsumerSegmentMember</xbrldi:explicitMember>
                        </xbrli:segment>
                      </xbrli:entity>
                    </xbrli:context>
                    <xbrli:context id="seg_ignored_D2025-01-01-2025-03-31">
                      <xbrli:entity>
                        <xbrli:segment>
                          <xbrldi:explicitMember dimension="us-gaap:StatementBusinessSegmentsAxis">mmm:TwoGroupsMember</xbrldi:explicitMember>
                        </xbrli:segment>
                      </xbrli:entity>
                    </xbrli:context>
                  </ix:resources>
                </div>
                <ix:nonFraction unitRef="Unit_Standard_USD_custom" contextRef="seg_consumer_D2025-01-01-2025-03-31" decimals="-6" name="us-gaap:Revenues" scale="6">1,192</ix:nonFraction>
                <ix:nonFraction unitRef="usd" contextRef="seg_industrial_D2025-01-01-2025-03-31" decimals="-6" name="us-gaap:OperatingIncomeLoss" scale="6">601</ix:nonFraction>
                <ix:nonFraction unitRef="usd" contextRef="seg_consumer_I2025-03-31" decimals="-6" name="us-gaap:Assets" scale="6">2,500</ix:nonFraction>
                <ix:nonFraction unitRef="usd" contextRef="seg_ignored_D2025-01-01-2025-03-31" decimals="-6" name="us-gaap:Revenues" scale="6">999</ix:nonFraction>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("inline xbrl segment extraction should succeed");

        let consumer_revenue = extracted
            .iter()
            .find(|metric| {
                metric.metric_id.as_str() == "segment_data.segment_revenue"
                    && metric.numeric_value.provenance.source_location.segment_name.as_deref()
                        == Some("Consumer Segment")
            })
            .expect("consumer segment revenue should be extracted");
        assert_eq!(consumer_revenue.numeric_value.amount, 1192.0);
        assert_eq!(consumer_revenue.numeric_value.scale, ValueScale::Millions);
        assert_eq!(
            consumer_revenue.numeric_value.unit,
            MeasurementUnit::Currency("USD".to_string())
        );

        let industrial_profit = extracted
            .iter()
            .find(|metric| {
                metric.metric_id.as_str() == "segment_data.segment_profit_or_loss"
                    && metric.numeric_value.provenance.source_location.segment_name.as_deref()
                        == Some("Safety And Industrial Segment")
            })
            .expect("industrial segment operating income should be extracted");
        assert_eq!(industrial_profit.numeric_value.amount, 601.0);

        let consumer_assets = extracted
            .iter()
            .find(|metric| metric.metric_id.as_str() == "segment_data.segment_assets")
            .expect("segment assets should be extracted");
        assert!(matches!(
            consumer_assets.numeric_value.reporting_period.context,
            PeriodContext::Instant { as_of } if as_of == date!(2025 - 03 - 31)
        ));
        assert!(extracted.iter().all(|metric| {
            metric.numeric_value.provenance.source_location.segment_name.as_deref()
                != Some("Two Groups")
        }));
    }

    #[test]
    fn inline_xbrl_segment_context_dates_override_nonstandard_context_ids() {
        let extractor = HtmlExtractor::default();
        let filing = sample_quarterly_filing();
        let html = r#"
            <html>
              <body>
                <div style="display:none">
                  <ix:resources>
                    <xbrli:context id="consumer_current">
                      <xbrli:period>
                        <xbrli:startDate>2025-01-01</xbrli:startDate>
                        <xbrli:endDate>2025-03-31</xbrli:endDate>
                      </xbrli:period>
                      <xbrli:entity>
                        <xbrli:segment>
                          <xbrldi:explicitMember dimension="us-gaap:StatementBusinessSegmentsAxis">mmm:ConsumerSegmentMember</xbrldi:explicitMember>
                        </xbrli:segment>
                      </xbrli:entity>
                    </xbrli:context>
                    <xbrli:context id="consumer_ytd">
                      <xbrli:period>
                        <xbrli:startDate>2025-01-01</xbrli:startDate>
                        <xbrli:endDate>2025-06-30</xbrli:endDate>
                      </xbrli:period>
                      <xbrli:entity>
                        <xbrli:segment>
                          <xbrldi:explicitMember dimension="us-gaap:StatementBusinessSegmentsAxis">mmm:ConsumerSegmentMember</xbrldi:explicitMember>
                        </xbrli:segment>
                      </xbrli:entity>
                    </xbrli:context>
                  </ix:resources>
                </div>
                <ix:nonFraction unitRef="usd" contextRef="consumer_current" name="us-gaap:Revenues" scale="6">1,192</ix:nonFraction>
                <ix:nonFraction unitRef="usd" contextRef="consumer_ytd" name="us-gaap:Revenues" scale="6">2,485</ix:nonFraction>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("inline xbrl segment extraction should succeed");

        let mut periods = extracted
            .iter()
            .filter(|metric| metric.metric_id.as_str() == "segment_data.segment_revenue")
            .map(|metric| metric.numeric_value.reporting_period.clone())
            .collect::<Vec<_>>();
        periods.sort_by_key(|period| match period.context {
            PeriodContext::Instant { as_of } => as_of.to_string(),
            PeriodContext::Duration { start, end } => format!("{start}-{end}"),
        });

        assert_eq!(periods.len(), 2);
        assert!(periods.iter().any(|period| matches!(
            period.context,
            PeriodContext::Duration { start, end }
                if start == date!(2025 - 01 - 01) && end == date!(2025 - 03 - 31)
        )));
        assert!(periods.iter().any(|period| matches!(
            period.context,
            PeriodContext::Duration { start, end }
                if start == date!(2025 - 01 - 01) && end == date!(2025 - 06 - 30)
        )));
    }

    #[test]
    fn inline_xbrl_segment_excludes_extra_breakdown_axes_and_keeps_segment_total() {
        let extractor = HtmlExtractor::default();
        let filing = sample_quarterly_filing();
        let html = r#"
            <html>
              <body>
                <div style="display:none">
                  <ix:resources>
                    <xbrli:context id="seg_total">
                      <xbrli:period>
                        <xbrli:startDate>2025-01-01</xbrli:startDate>
                        <xbrli:endDate>2025-03-31</xbrli:endDate>
                      </xbrli:period>
                      <xbrli:entity>
                        <xbrli:segment>
                          <xbrldi:explicitMember dimension="srt:ConsolidationItemsAxis">us-gaap:OperatingSegmentsMember</xbrldi:explicitMember>
                          <xbrldi:explicitMember dimension="us-gaap:StatementBusinessSegmentsAxis">mmm:ConsumerSegmentMember</xbrldi:explicitMember>
                        </xbrli:segment>
                      </xbrli:entity>
                    </xbrli:context>
                    <xbrli:context id="seg_breakdown">
                      <xbrli:period>
                        <xbrli:startDate>2025-01-01</xbrli:startDate>
                        <xbrli:endDate>2025-03-31</xbrli:endDate>
                      </xbrli:period>
                      <xbrli:entity>
                        <xbrli:segment>
                          <xbrldi:explicitMember dimension="srt:ConsolidationItemsAxis">us-gaap:OperatingSegmentsMember</xbrldi:explicitMember>
                          <xbrldi:explicitMember dimension="mmm:DivisionAxis">mmm:HomeAndAutoCareMember</xbrldi:explicitMember>
                          <xbrldi:explicitMember dimension="us-gaap:StatementBusinessSegmentsAxis">mmm:ConsumerSegmentMember</xbrldi:explicitMember>
                        </xbrli:segment>
                      </xbrli:entity>
                    </xbrli:context>
                  </ix:resources>
                </div>
                <ix:nonFraction unitRef="usd" contextRef="seg_total" name="us-gaap:Revenues" scale="6">4,931</ix:nonFraction>
                <ix:nonFraction unitRef="usd" contextRef="seg_breakdown" name="us-gaap:RevenueFromContractWithCustomerExcludingAssessedTax" scale="6">1,080</ix:nonFraction>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("inline xbrl segment extraction should succeed");

        let segment_revenues = extracted
            .iter()
            .filter(|metric| metric.metric_id.as_str() == "segment_data.segment_revenue")
            .collect::<Vec<_>>();

        assert_eq!(segment_revenues.len(), 1);
        assert_eq!(segment_revenues[0].numeric_value.amount, 4931.0);
        assert_eq!(
            segment_revenues[0].numeric_value.provenance.source_location.segment_name.as_deref(),
            Some("Consumer Segment")
        );
    }

    #[test]
    fn extracts_core_revenue_from_inline_xbrl_without_segment_axes() {
        let extractor = HtmlExtractor::default();
        let filing = sample_quarterly_filing();
        let html = r#"
            <html>
              <body>
                <div style="display:none">
                  <ix:resources>
                    <xbrli:context id="core_revenue_context">
                      <xbrli:period>
                        <xbrli:startDate>2025-01-01</xbrli:startDate>
                        <xbrli:endDate>2025-03-31</xbrli:endDate>
                      </xbrli:period>
                      <xbrli:entity>
                        <xbrli:identifier scheme="http://www.sec.gov/CIK">0000000000</xbrli:identifier>
                      </xbrli:entity>
                    </xbrli:context>
                    <xbrli:context id="seg_context">
                      <xbrli:period>
                        <xbrli:startDate>2025-01-01</xbrli:startDate>
                        <xbrli:endDate>2025-03-31</xbrli:endDate>
                      </xbrli:period>
                      <xbrli:entity>
                        <xbrli:segment>
                          <xbrldi:explicitMember dimension="us-gaap:StatementBusinessSegmentsAxis">mmm:ConsumerSegmentMember</xbrldi:explicitMember>
                        </xbrli:segment>
                      </xbrli:entity>
                    </xbrli:context>
                  </ix:resources>
                </div>
                <ix:nonFraction unitRef="usd" contextRef="core_revenue_context" name="us-gaap:Revenues" scale="6">8,080</ix:nonFraction>
                <ix:nonFraction unitRef="usd" contextRef="seg_context" name="us-gaap:Revenues" scale="6">1,192</ix:nonFraction>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("inline xbrl core extraction should succeed");

        let core_revenue = extracted
            .iter()
            .find(|metric| {
                metric.metric_id.as_str() == "income_statement.revenue"
                    && metric.numeric_value.provenance.source_location.section_name.as_deref()
                        == Some("inline_xbrl_core")
            })
            .expect("core revenue should be extracted");

        assert_eq!(core_revenue.numeric_value.amount, 8080.0);
        assert_eq!(core_revenue.numeric_value.provenance.source_type, SourceType::Xbrl);
        assert!(matches!(
            core_revenue.numeric_value.reporting_period.context,
            PeriodContext::Duration { start, end }
                if start == date!(2025 - 01 - 01) && end == date!(2025 - 03 - 31)
        ));
    }

    #[test]
    fn core_inline_xbrl_revenue_skips_ytd_comparative_contexts_for_quarterly_filing() {
        let extractor = HtmlExtractor::default();
        let filing = sample_quarterly_filing();
        let html = r#"
            <html>
              <body>
                <div style="display:none">
                  <ix:resources>
                    <xbrli:context id="quarter_context">
                      <xbrli:period>
                        <xbrli:startDate>2025-01-01</xbrli:startDate>
                        <xbrli:endDate>2025-03-31</xbrli:endDate>
                      </xbrli:period>
                    </xbrli:context>
                    <xbrli:context id="ytd_context">
                      <xbrli:period>
                        <xbrli:startDate>2024-10-01</xbrli:startDate>
                        <xbrli:endDate>2025-03-31</xbrli:endDate>
                      </xbrli:period>
                    </xbrli:context>
                  </ix:resources>
                </div>
                <ix:nonFraction unitRef="usd" contextRef="quarter_context" name="us-gaap:Revenues" scale="6">8,080</ix:nonFraction>
                <ix:nonFraction unitRef="usd" contextRef="ytd_context" name="us-gaap:Revenues" scale="6">15,900</ix:nonFraction>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("inline xbrl core extraction should succeed");

        let revenues = extracted
            .iter()
            .filter(|metric| {
                metric.metric_id.as_str() == "income_statement.revenue"
                    && metric.numeric_value.provenance.source_location.section_name.as_deref()
                        == Some("inline_xbrl_core")
            })
            .collect::<Vec<_>>();

        assert_eq!(revenues.len(), 1);
        assert_eq!(revenues[0].numeric_value.amount, 8080.0);
    }

    #[test]
    fn extracts_multiple_periods_from_explicit_duration_headers() {
        let extractor = HtmlExtractor::default();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Consolidated Statements of Operations (in millions)</caption>
                  <tr>
                    <th></th>
                    <th>Three months ended March 31, 2025</th>
                    <th>Three months ended March 31, 2024</th>
                  </tr>
                  <tr>
                    <th>Total revenue</th>
                    <td>4,883</td>
                    <td>4,547</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &sample_quarterly_filing())
            .expect("html fallback extraction should succeed");

        let revenue_values = extracted
            .iter()
            .filter(|metric| metric.metric_id.as_str() == "income_statement.revenue")
            .collect::<Vec<_>>();

        assert_eq!(revenue_values.len(), 2);
        assert!(revenue_values.iter().any(|metric| {
            metric.numeric_value.amount == 4_883.0
                && matches!(
                    metric.numeric_value.reporting_period.context,
                    PeriodContext::Duration { start, end }
                        if start == date!(2025 - 01 - 01) && end == date!(2025 - 03 - 31)
                )
        }));
        assert!(revenue_values.iter().any(|metric| {
            metric.numeric_value.amount == 4_547.0
                && matches!(
                    metric.numeric_value.reporting_period.context,
                    PeriodContext::Duration { start, end }
                        if start == date!(2024 - 01 - 01) && end == date!(2024 - 03 - 31)
                )
        }));
    }

    #[test]
    fn ignores_reference_columns_when_multi_period_headers_exist() {
        let extractor = HtmlExtractor::default();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Consolidated Statements of Operations (in millions)</caption>
                  <tr>
                    <th></th>
                    <th>Three months ended March 31, 2025</th>
                    <th>Three months ended March 31, 2024</th>
                    <th>March 31, 2024 Reference</th>
                  </tr>
                  <tr>
                    <th>Total revenue</th>
                    <td>4,883</td>
                    <td>4,547</td>
                    <td>7</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &sample_quarterly_filing())
            .expect("html fallback extraction should succeed");

        let revenue_values = extracted
            .iter()
            .filter(|metric| metric.metric_id.as_str() == "income_statement.revenue")
            .collect::<Vec<_>>();

        assert_eq!(revenue_values.len(), 2);
        assert!(revenue_values.iter().all(|metric| metric.numeric_value.amount != 7.0));
    }

    #[test]
    fn extracts_six_month_duration_from_explicit_header() {
        let extractor = HtmlExtractor::default();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Consolidated Statements of Cash Flows (in millions)</caption>
                  <tr>
                    <th></th>
                    <th>Six months ended September 30, 2024</th>
                  </tr>
                  <tr>
                    <th>Net cash provided by operating activities</th>
                    <td>1,234</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &sample_quarterly_filing())
            .expect("html fallback extraction should succeed");

        let cash_flow = extracted
            .iter()
            .find(|metric| metric.metric_id.as_str() == "cash_flow.net_cash_from_operations")
            .expect("cash flow metric should be extracted");

        assert_eq!(cash_flow.numeric_value.amount, 1_234.0);
        assert!(matches!(
            cash_flow.numeric_value.reporting_period.context,
            PeriodContext::Duration { start, end }
                if start == date!(2024 - 04 - 01) && end == date!(2024 - 09 - 30)
        ));
        assert_eq!(cash_flow.numeric_value.reporting_period.fiscal_period, None);
    }

    #[test]
    fn explicit_comparative_column_uses_its_own_fiscal_period_metadata() {
        let extractor = HtmlExtractor::default();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Consolidated Balance Sheets (in millions)</caption>
                  <tr>
                    <th></th>
                    <th>December 31, 2025</th>
                    <th>December 31, 2024</th>
                  </tr>
                  <tr>
                    <th>Cash and cash equivalents</th>
                    <td>415</td>
                    <td>325</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &sample_quarterly_filing())
            .expect("html fallback extraction should succeed");

        let historical = extracted
            .iter()
            .find(|metric| metric.numeric_value.amount == 325.0)
            .expect("historical comparative value should be extracted");

        assert_eq!(historical.numeric_value.reporting_period.fiscal_period, None);
    }

    #[test]
    fn extracts_year_ended_duration_from_explicit_header() {
        let extractor = HtmlExtractor::default();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Consolidated Statements of Operations (in millions)</caption>
                  <tr>
                    <th></th>
                    <th>Year ended December 31, 2024</th>
                  </tr>
                  <tr>
                    <th>Total revenue</th>
                    <td>18,500</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &sample_filing())
            .expect("html fallback extraction should succeed");

        let revenue = extracted
            .iter()
            .find(|metric| metric.metric_id.as_str() == "income_statement.revenue")
            .expect("revenue should be extracted");

        assert!(matches!(
            revenue.numeric_value.reporting_period.context,
            PeriodContext::Duration { start, end }
                if start == date!(2024 - 01 - 01) && end == date!(2024 - 12 - 31)
        ));
        assert_eq!(revenue.numeric_value.reporting_period.fiscal_period, None);
    }

    #[test]
    fn extracts_for_year_ended_duration_from_explicit_header() {
        let extractor = HtmlExtractor::default();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Consolidated Statements of Operations (in millions)</caption>
                  <tr>
                    <th></th>
                    <th>For the year ended December 31, 2024</th>
                  </tr>
                  <tr>
                    <th>Total revenue</th>
                    <td>18,500</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &sample_filing())
            .expect("html fallback extraction should succeed");

        let revenue = extracted
            .iter()
            .find(|metric| metric.metric_id.as_str() == "income_statement.revenue")
            .expect("revenue should be extracted");

        assert!(matches!(
            revenue.numeric_value.reporting_period.context,
            PeriodContext::Duration { start, end }
                if start == date!(2024 - 01 - 01) && end == date!(2024 - 12 - 31)
        ));
        assert_eq!(revenue.numeric_value.reporting_period.fiscal_period, None);
    }

    #[test]
    fn extracts_multiple_periods_from_bare_annual_year_headers() {
        let extractor = HtmlExtractor::default();
        let filing = FilingMetadata {
            form_type: FilingForm::Form10K,
            report_period_end: Some(date!(2017 - 12 - 31)),
            ..sample_filing()
        };
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Consolidated Statements of Income (Millions)</caption>
                  <tr>
                    <th>(Millions)</th>
                    <th colspan="2">2017</th>
                    <th colspan="2">2016</th>
                    <th colspan="2">2015</th>
                  </tr>
                  <tr>
                    <th>Total operating expenses</th>
                    <td>$</td>
                    <td>23,837</td>
                    <td>$</td>
                    <td>22,886</td>
                    <td>$</td>
                    <td>23,328</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("html fallback extraction should succeed");

        let expenses = extracted
            .iter()
            .filter(|metric| metric.metric_id.as_str() == "income_statement.operating_expenses")
            .collect::<Vec<_>>();

        assert_eq!(expenses.len(), 3);
        assert_eq!(expenses[0].numeric_value.amount, 23837.0);
        assert!(matches!(
            expenses[0].numeric_value.reporting_period.context,
            PeriodContext::Instant { as_of } if as_of == date!(2017 - 12 - 31)
        ));
        assert_eq!(expenses[1].numeric_value.amount, 22886.0);
        assert!(matches!(
            expenses[1].numeric_value.reporting_period.context,
            PeriodContext::Instant { as_of } if as_of == date!(2016 - 12 - 31)
        ));
        assert_eq!(expenses[2].numeric_value.amount, 23328.0);
        assert!(matches!(
            expenses[2].numeric_value.reporting_period.context,
            PeriodContext::Instant { as_of } if as_of == date!(2015 - 12 - 31)
        ));
    }

    #[test]
    fn extracts_bare_annual_year_headers_when_units_row_comes_first() {
        let extractor = HtmlExtractor::default();
        let filing = FilingMetadata {
            form_type: FilingForm::Form10K,
            report_period_end: Some(date!(2017 - 12 - 31)),
            ..sample_filing()
        };
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Consolidated Statements of Income</caption>
                  <tr>
                    <th>(Millions, except per share amounts)</th>
                    <th colspan="2">&nbsp;</th>
                    <th colspan="2">&nbsp;</th>
                    <th colspan="2">&nbsp;</th>
                  </tr>
                  <tr>
                    <th></th>
                    <th colspan="2">2017</th>
                    <th colspan="2">2016</th>
                    <th colspan="2">2015</th>
                  </tr>
                  <tr>
                    <th>Total operating expenses</th>
                    <td>$</td>
                    <td>23,837</td>
                    <td>$</td>
                    <td>22,886</td>
                    <td>$</td>
                    <td>23,328</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("html fallback extraction should succeed");

        let expenses = extracted
            .iter()
            .filter(|metric| metric.metric_id.as_str() == "income_statement.operating_expenses")
            .collect::<Vec<_>>();

        assert_eq!(expenses.len(), 3);
        assert!(matches!(
            expenses[0].numeric_value.reporting_period.context,
            PeriodContext::Instant { as_of } if as_of == date!(2017 - 12 - 31)
        ));
        assert!(matches!(
            expenses[1].numeric_value.reporting_period.context,
            PeriodContext::Instant { as_of } if as_of == date!(2016 - 12 - 31)
        ));
        assert!(matches!(
            expenses[2].numeric_value.reporting_period.context,
            PeriodContext::Instant { as_of } if as_of == date!(2015 - 12 - 31)
        ));
    }

    #[test]
    fn does_not_treat_bare_annual_year_headers_in_note_tables_as_statement_periods() {
        let extractor = HtmlExtractor::default();
        let filing = FilingMetadata {
            form_type: FilingForm::Form10K,
            report_period_end: Some(date!(2017 - 12 - 31)),
            ..sample_filing()
        };
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Available-for-sale securities</caption>
                  <tr>
                    <th></th>
                    <th colspan="2">2017</th>
                    <th colspan="2">2016</th>
                  </tr>
                  <tr>
                    <th>Cash and cash equivalents</th>
                    <td>$</td>
                    <td>123</td>
                    <td>$</td>
                    <td>456</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("html fallback extraction should succeed");

        let cash = extracted
            .iter()
            .filter(|metric| metric.metric_id.as_str() == "balance_sheet.cash_and_equivalents")
            .collect::<Vec<_>>();

        assert!(cash.is_empty());
    }

    #[test]
    fn rejects_percentage_cells_for_currency_metrics_but_keeps_percentage_metrics() {
        let extractor = HtmlExtractor::default();
        let filing = sample_filing();
        let income_statement_html = r#"
            <html>
              <body>
                <table>
                  <caption>Consolidated Statements of Income</caption>
                  <tr>
                    <th></th>
                    <th>Year ended December 31, 2024</th>
                  </tr>
                  <tr>
                    <th>Cost of sales</th>
                    <td>57.4%</td>
                  </tr>
                </table>
                <table>
                  <caption>Debt and credit facilities</caption>
                  <tr>
                    <th></th>
                    <th>Year ended December 31, 2024</th>
                  </tr>
                  <tr>
                    <th>Interest rate</th>
                    <td>4.8%</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(income_statement_html, &filing)
            .expect("html fallback extraction should succeed");

        assert!(
            extracted
                .iter()
                .all(|metric| metric.metric_id.as_str() != "income_statement.cost_of_goods_sold")
        );
        assert!(
            extracted
                .iter()
                .any(|metric| metric.metric_id.as_str() == "debt_and_credit.interest_rate")
        );
    }

    #[test]
    fn rejects_percent_of_net_sales_tables_for_currency_metrics() {
        let extractor = HtmlExtractor::default();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Three months ended March 31, (Percent of net sales) Change</caption>
                  <tr>
                    <th></th>
                    <th>2024</th>
                  </tr>
                  <tr>
                    <th>Cost of sales</th>
                    <td>57.4</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &sample_quarterly_filing())
            .expect("html fallback extraction should succeed");

        assert!(
            extracted
                .iter()
                .all(|metric| metric.metric_id.as_str() != "income_statement.cost_of_goods_sold")
        );
    }

    #[test]
    fn no_caption_multi_period_tables_use_inferred_statement_context() {
        let extractor = HtmlExtractor::default();
        let filing = FilingMetadata {
            form_type: FilingForm::Form10K,
            report_period_end: Some(date!(2017 - 12 - 31)),
            ..sample_filing()
        };
        let html = r#"
            <html>
              <body>
                <table>
                  <tr>
                    <th colspan="5">Consolidated Statements of Income</th>
                  </tr>
                  <tr>
                    <th>(Millions)</th>
                    <th colspan="2">2017</th>
                    <th colspan="2">2016</th>
                  </tr>
                  <tr>
                    <th>Cost of sales</th>
                    <td>$</td>
                    <td>16,001</td>
                    <td>$</td>
                    <td>15,500</td>
                  </tr>
                </table>
                <table>
                  <tr>
                    <th colspan="5">Available-for-sale securities</th>
                  </tr>
                  <tr>
                    <th>(Millions)</th>
                    <th colspan="2">2017</th>
                    <th colspan="2">2016</th>
                  </tr>
                  <tr>
                    <th>Cash and cash equivalents</th>
                    <td>$</td>
                    <td>123</td>
                    <td>$</td>
                    <td>456</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("html fallback extraction should succeed");

        let cost = extracted
            .iter()
            .filter(|metric| metric.metric_id.as_str() == "income_statement.cost_of_goods_sold")
            .collect::<Vec<_>>();
        let cash = extracted
            .iter()
            .filter(|metric| metric.metric_id.as_str() == "balance_sheet.cash_and_equivalents")
            .collect::<Vec<_>>();

        assert_eq!(cost.len(), 2);
        assert_eq!(cash.len(), 1);
        assert!(matches!(
            cash[0].numeric_value.reporting_period.context,
            PeriodContext::Instant { as_of } if as_of == date!(2017 - 12 - 31)
        ));
    }

    #[test]
    fn extracts_numeric_fallbacks_from_statement_tables() {
        let extractor = HtmlExtractor::default();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Consolidated Balance Sheets (in millions)</caption>
                  <tr><th>Cash and Cash Equivalents</th><td>125</td></tr>
                  <tr><th>Long-Term Debt</th><td>(40)</td></tr>
                </table>
                <table>
                  <caption>Consolidated Statements of Operations</caption>
                  <tr><th>Revenue</th><td>980</td></tr>
                </table>
                <table>
                  <caption>Debt Footnote Table</caption>
                  <tr><th>Revolver Balance</th><td>45</td></tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &sample_filing())
            .expect("html fallback extraction should succeed");

        assert!(
            extracted
                .iter()
                .any(|metric| metric.metric_id.as_str() == "balance_sheet.cash_and_equivalents")
        );
        assert!(
            extracted
                .iter()
                .any(|metric| metric.metric_id.as_str() == "balance_sheet.long_term_debt"
                    && metric.numeric_value.amount == -40.0
                    && metric.numeric_value.scale == ValueScale::Millions)
        );
        assert!(
            extracted.iter().any(|metric| metric.metric_id.as_str() == "income_statement.revenue")
        );
        assert!(
            extracted
                .iter()
                .any(|metric| metric.metric_id.as_str() == "balance_sheet.long_term_debt")
        );
    }

    #[test]
    fn extracts_debt_note_labels_from_narrow_html_aliases() {
        let extractor = HtmlExtractor::default();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Debt and credit facilities</caption>
                  <tr><th>Senior unsecured notes</th><td>1250</td></tr>
                  <tr><th>Weighted average debt interest rate</th><td>4.8%</td></tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &sample_filing())
            .expect("html fallback extraction should succeed");

        assert!(
            extracted
                .iter()
                .any(|metric| metric.metric_id.as_str() == "debt_and_credit.notes_and_bonds")
        );
        assert!(
            extracted
                .iter()
                .any(|metric| metric.metric_id.as_str() == "debt_and_credit.interest_rate")
        );
    }

    #[test]
    fn extracts_debt_metrics_from_note_tables_with_metric_columns() {
        let extractor = HtmlExtractor::default();
        let filing = sample_filing();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Long-Term Debt and Short-Term Borrowings</caption>
                  <tr>
                    <th>Description</th>
                    <th>Effective Interest Rate / 2024</th>
                    <th>Carrying Value / 2024</th>
                  </tr>
                  <tr>
                    <th>Registered note ($750 million)</th>
                    <td>2.02%</td>
                    <td>750</td>
                  </tr>
                  <tr>
                    <th>30-year bond ($220 million)</th>
                    <td>6.38%</td>
                    <td>220</td>
                  </tr>
                  <tr>
                    <th>Revolving credit facility</th>
                    <td>5.10%</td>
                    <td>125</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("html fallback extraction should succeed");

        let notes_and_bonds = extracted
            .iter()
            .find(|metric| metric.metric_id.as_str() == "debt_and_credit.notes_and_bonds");
        let interest_rate = extracted
            .iter()
            .find(|metric| metric.metric_id.as_str() == "debt_and_credit.interest_rate");
        let revolver = extracted
            .iter()
            .find(|metric| metric.metric_id.as_str() == "debt_and_credit.revolver_balance");

        assert_eq!(notes_and_bonds.map(|metric| metric.numeric_value.amount), Some(970.0));
        assert_eq!(interest_rate.map(|metric| metric.numeric_value.amount), Some(5.10));
        assert_eq!(revolver.map(|metric| metric.numeric_value.amount), Some(125.0));
    }

    #[test]
    fn extracts_debt_detail_metrics_from_clearly_labeled_note_rows() {
        let extractor = HtmlExtractor::default();
        let filing = sample_filing();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Long-Term Debt and Other Borrowed Funds</caption>
                  <tr>
                    <th>Description</th>
                    <th>Effective Interest Rate / 2024</th>
                    <th>Carrying Value / 2024</th>
                  </tr>
                  <tr>
                    <th>Senior unsecured notes</th>
                    <td>4.20%</td>
                    <td>800</td>
                  </tr>
                  <tr>
                    <th>Junior subordinated notes</th>
                    <td>5.10%</td>
                    <td>250</td>
                  </tr>
                  <tr>
                    <th>Other borrowed funds</th>
                    <td>3.75%</td>
                    <td>175</td>
                  </tr>
                  <tr>
                    <th>Structured notes</th>
                    <td>6.00%</td>
                    <td>90</td>
                  </tr>
                  <tr>
                    <th>Asset-backed secured borrowings</th>
                    <td>4.90%</td>
                    <td>60</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("html fallback extraction should succeed");

        let senior_notes = extracted
            .iter()
            .find(|metric| metric.metric_id.as_str() == "debt_and_credit.detail_senior_notes");
        let subordinated_debt = extracted
            .iter()
            .find(|metric| metric.metric_id.as_str() == "debt_and_credit.detail_subordinated_debt");
        let other_borrowed_funds = extracted.iter().find(|metric| {
            metric.metric_id.as_str() == "debt_and_credit.detail_other_borrowed_funds"
        });
        let structured_notes = extracted
            .iter()
            .find(|metric| metric.metric_id.as_str() == "debt_and_credit.detail_structured_notes");
        let secured_borrowings = extracted.iter().find(|metric| {
            metric.metric_id.as_str() == "debt_and_credit.detail_secured_borrowings"
        });
        let aggregate_notes = extracted
            .iter()
            .find(|metric| metric.metric_id.as_str() == "debt_and_credit.notes_and_bonds");

        assert_eq!(senior_notes.map(|metric| metric.numeric_value.amount), Some(800.0));
        assert_eq!(subordinated_debt.map(|metric| metric.numeric_value.amount), Some(250.0));
        assert_eq!(other_borrowed_funds.map(|metric| metric.numeric_value.amount), Some(175.0));
        assert_eq!(structured_notes.map(|metric| metric.numeric_value.amount), Some(90.0));
        assert_eq!(secured_borrowings.map(|metric| metric.numeric_value.amount), Some(60.0));
        assert_eq!(aggregate_notes.map(|metric| metric.numeric_value.amount), Some(1_140.0));
    }

    #[test]
    fn extracts_debt_detail_flow_metrics_from_issuance_and_maturity_rows() {
        let extractor = HtmlExtractor::default();
        let filing = sample_filing();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Long-Term Unsecured Funding</caption>
                  <tr>
                    <th>Description</th>
                    <th>Carrying Value / 2024</th>
                  </tr>
                  <tr>
                    <th>Senior notes issuance</th>
                    <td>325</td>
                  </tr>
                  <tr>
                    <th>Senior notes maturities and redemptions</th>
                    <td>140</td>
                  </tr>
                  <tr>
                    <th>Other borrowed funds issuance</th>
                    <td>80</td>
                  </tr>
                  <tr>
                    <th>Other borrowed funds repayments</th>
                    <td>35</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("html fallback extraction should succeed");

        let senior_issuance = extracted.iter().find(|metric| {
            metric.metric_id.as_str() == "debt_and_credit.detail_senior_notes_issuance"
        });
        let senior_maturities = extracted.iter().find(|metric| {
            metric.metric_id.as_str() == "debt_and_credit.detail_senior_notes_maturities"
        });
        let other_issuance = extracted.iter().find(|metric| {
            metric.metric_id.as_str() == "debt_and_credit.detail_other_borrowed_funds_issuance"
        });
        let other_maturities = extracted.iter().find(|metric| {
            metric.metric_id.as_str() == "debt_and_credit.detail_other_borrowed_funds_maturities"
        });

        assert_eq!(senior_issuance.map(|metric| metric.numeric_value.amount), Some(325.0));
        assert_eq!(senior_maturities.map(|metric| metric.numeric_value.amount), Some(140.0));
        assert_eq!(other_issuance.map(|metric| metric.numeric_value.amount), Some(80.0));
        assert_eq!(other_maturities.map(|metric| metric.numeric_value.amount), Some(35.0));
    }

    #[test]
    fn extracts_unsecured_funding_flow_metrics_from_section_style_table() {
        let extractor = HtmlExtractor::default();
        let filing = sample_filing();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Long-term unsecured funding</caption>
                  <tr>
                    <th>Year ended December 31,</th>
                    <th>2024</th>
                    <th>2023</th>
                  </tr>
                  <tr>
                    <th>Issuance</th>
                    <td></td>
                    <td></td>
                  </tr>
                  <tr>
                    <th>Senior notes</th>
                    <td>900</td>
                    <td>700</td>
                  </tr>
                  <tr>
                    <th>Subordinated</th>
                    <td>150</td>
                    <td>125</td>
                  </tr>
                  <tr>
                    <th>Maturities/redemptions</th>
                    <td></td>
                    <td></td>
                  </tr>
                  <tr>
                    <th>Senior notes</th>
                    <td>420</td>
                    <td>390</td>
                  </tr>
                  <tr>
                    <th>Subordinated</th>
                    <td>80</td>
                    <td>60</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("html fallback extraction should succeed");

        let senior_issuance = extracted.iter().find(|metric| {
            metric.metric_id.as_str() == "debt_and_credit.detail_senior_notes_issuance"
        });
        let subordinated_issuance = extracted.iter().find(|metric| {
            metric.metric_id.as_str() == "debt_and_credit.detail_subordinated_debt_issuance"
        });
        let senior_maturities = extracted.iter().find(|metric| {
            metric.metric_id.as_str() == "debt_and_credit.detail_senior_notes_maturities"
        });
        let subordinated_maturities = extracted.iter().find(|metric| {
            metric.metric_id.as_str() == "debt_and_credit.detail_subordinated_debt_maturities"
        });

        assert_eq!(senior_issuance.map(|metric| metric.numeric_value.amount), Some(900.0));
        assert_eq!(subordinated_issuance.map(|metric| metric.numeric_value.amount), Some(150.0));
        assert_eq!(senior_maturities.map(|metric| metric.numeric_value.amount), Some(420.0));
        assert_eq!(subordinated_maturities.map(|metric| metric.numeric_value.amount), Some(80.0));
    }

    #[test]
    fn extracts_secured_funding_flow_metrics_from_section_style_table() {
        let extractor = HtmlExtractor::default();
        let filing = sample_filing();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Long-term secured funding</caption>
                  <tr>
                    <th>Year ended December 31,</th>
                    <th>Issuance 2024</th>
                    <th>Maturities/Redemptions 2024</th>
                  </tr>
                  <tr>
                    <th>Credit card securitization</th>
                    <td>1396</td>
                    <td>1590</td>
                  </tr>
                  <tr>
                    <th>FHLB advances</th>
                    <td>2200</td>
                    <td>1800</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("html fallback extraction should succeed");

        let secured_issuance = extracted.iter().find(|metric| {
            metric.metric_id.as_str() == "debt_and_credit.detail_secured_borrowings_issuance"
        });
        let secured_maturities = extracted.iter().find(|metric| {
            metric.metric_id.as_str() == "debt_and_credit.detail_secured_borrowings_maturities"
        });

        assert_eq!(secured_issuance.map(|metric| metric.numeric_value.amount), Some(1396.0));
        assert_eq!(secured_maturities.map(|metric| metric.numeric_value.amount), Some(1590.0));
    }

    #[test]
    fn extracts_other_borrowed_funds_from_short_term_unsecured_balance_table() {
        let extractor = HtmlExtractor::default();
        let filing = sample_filing();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Short-term unsecured funding</caption>
                  <tr>
                    <th>(in millions)</th>
                    <th>2024</th>
                    <th>2023</th>
                  </tr>
                  <tr>
                    <th>Other borrowed funds</th>
                    <td>8789</td>
                    <td>10727</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("html fallback extraction should succeed");

        let other_borrowed_funds = extracted.iter().find(|metric| {
            metric.metric_id.as_str() == "debt_and_credit.detail_other_borrowed_funds"
        });

        assert_eq!(other_borrowed_funds.map(|metric| metric.numeric_value.amount), Some(8789.0));
    }

    #[test]
    fn extracts_debt_metrics_from_no_caption_tables_when_debt_columns_are_present() {
        let extractor = HtmlExtractor::default();
        let filing = sample_filing();
        let html = r#"
            <html>
              <body>
                <table>
                  <tr>
                    <th>Description</th>
                    <th>Effective Interest Rate</th>
                    <th>Carrying Value</th>
                  </tr>
                  <tr>
                    <th>Medium-term note</th>
                    <td>3.03%</td>
                    <td>550</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("html fallback extraction should succeed");

        assert!(
            extracted
                .iter()
                .any(|metric| metric.metric_id.as_str() == "debt_and_credit.notes_and_bonds")
        );
        assert!(
            extracted
                .iter()
                .any(|metric| metric.metric_id.as_str() == "debt_and_credit.interest_rate")
        );
    }

    #[test]
    fn rejects_long_term_debt_and_interest_rate_from_fair_value_tables() {
        let extractor = HtmlExtractor::default();
        let filing = sample_filing();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Assets and liabilities measured at fair value on a recurring basis</caption>
                  <tr>
                    <th>Fair value hierarchy</th>
                    <th>Long-term debt</th>
                    <th>Interest rate</th>
                  </tr>
                  <tr>
                    <th>Level 2</th>
                    <td>31394</td>
                    <td>2.31%</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("html fallback extraction should succeed");

        assert!(
            extracted
                .iter()
                .all(|metric| metric.metric_id.as_str() != "balance_sheet.long_term_debt")
        );
        assert!(
            extracted
                .iter()
                .all(|metric| metric.metric_id.as_str() != "debt_and_credit.interest_rate")
        );
    }

    #[test]
    fn rejects_interest_rate_from_free_standing_derivative_tables() {
        let extractor = HtmlExtractor::default();
        let filing = sample_filing();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Free-standing derivative receivables and payables (a)</caption>
                  <tr>
                    <th>Interest rate</th>
                    <th>2024</th>
                  </tr>
                  <tr>
                    <th>Interest rate</th>
                    <td>546625</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("html fallback extraction should succeed");

        assert!(
            extracted
                .iter()
                .all(|metric| metric.metric_id.as_str() != "debt_and_credit.interest_rate")
        );
    }

    #[test]
    fn rejects_implausibly_large_interest_rate_values_in_generic_tables() {
        let extractor = HtmlExtractor::default();
        let filing = sample_filing();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Long-Term Debt Summary</caption>
                  <tr>
                    <th>Description</th>
                    <th>2024</th>
                  </tr>
                  <tr>
                    <th>Interest rate</th>
                    <td>26429</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("html fallback extraction should succeed");

        assert!(
            extracted
                .iter()
                .all(|metric| metric.metric_id.as_str() != "debt_and_credit.interest_rate")
        );
    }

    #[test]
    fn keeps_interest_rate_from_funding_summary_table() {
        let extractor = HtmlExtractor::default();
        let filing = sample_filing();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>June 30, 2024 December 31, 2023 Long-term debt Short-term borrowings Deposits Total</caption>
                  <tr>
                    <th>Category</th>
                    <th>Interest rate</th>
                  </tr>
                  <tr>
                    <th>Long-term debt</th>
                    <td>4.29%</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("html fallback extraction should succeed");

        assert!(
            extracted
                .iter()
                .any(|metric| metric.metric_id.as_str() == "debt_and_credit.interest_rate")
        );
    }

    #[test]
    fn skips_generic_extraction_for_reference_only_tables() {
        let extractor = HtmlExtractor::default();
        let filing = sample_filing();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Reference table and derivative netting adjustments</caption>
                  <tr>
                    <th>Reference</th>
                    <th>Amount</th>
                  </tr>
                  <tr>
                    <th>Long-term debt</th>
                    <td>31394</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("html fallback extraction should succeed");

        assert!(extracted.is_empty());
    }

    #[test]
    fn skips_generic_extraction_for_fair_value_recurring_basis_tables() {
        let extractor = HtmlExtractor::default();
        let filing = sample_filing();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Assets and liabilities measured at fair value on a recurring basis</caption>
                  <tr>
                    <th>Category</th>
                    <th>Long-term debt</th>
                  </tr>
                  <tr>
                    <th>Level 2</th>
                    <td>31394</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("html fallback extraction should succeed");

        assert!(extracted.is_empty());
    }

    #[test]
    fn extracts_derivative_gain_loss_from_note_table_labels() {
        let extractor = HtmlExtractor::default();
        let filing = sample_filing();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Derivatives and hedging activities</caption>
                  <tr>
                    <th></th>
                    <th>Year ended December 31, 2024</th>
                  </tr>
                  <tr>
                    <th>Gain (loss) recognized in income on derivatives</th>
                    <td>42</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("html fallback extraction should succeed");

        let derivative_gain_loss = extracted
            .iter()
            .find(|metric| {
                metric.metric_id.as_str() == "derivatives_and_securities.derivative_gain_loss"
            })
            .expect("derivative gain/loss should be extracted");

        assert_eq!(derivative_gain_loss.numeric_value.amount, 42.0);
    }

    #[test]
    fn extracts_debt_securities_value_from_available_for_sale_note_labels() {
        let extractor = HtmlExtractor::default();
        let filing = sample_filing();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Available-for-sale securities</caption>
                  <tr>
                    <th></th>
                    <th>December 31, 2024</th>
                  </tr>
                  <tr>
                    <th>Available-for-sale debt securities at fair value</th>
                    <td>315</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("html fallback extraction should succeed");

        let debt_securities_value = extracted
            .iter()
            .find(|metric| {
                metric.metric_id.as_str() == "derivatives_and_securities.debt_securities_value"
            })
            .expect("available-for-sale debt securities value should be extracted");

        assert_eq!(debt_securities_value.numeric_value.amount, 315.0);
    }

    #[test]
    fn aggregates_inline_xbrl_derivative_gain_loss_by_period() {
        let extractor = HtmlExtractor::default();
        let filing = sample_filing();
        let html = r#"
            <html>
              <body>
                <div style="display:none">
                  <ix:resources>
                    <xbrli:context id="deriv_fair_value">
                      <xbrli:period>
                        <xbrli:startDate>2024-01-01</xbrli:startDate>
                        <xbrli:endDate>2024-12-31</xbrli:endDate>
                      </xbrli:period>
                      <xbrli:entity>
                        <xbrli:segment>
                          <xbrldi:explicitMember dimension="us-gaap:DerivativeInstrumentRiskAxis">us-gaap:InterestRateSwapMember</xbrldi:explicitMember>
                          <xbrldi:explicitMember dimension="us-gaap:DerivativeInstrumentsGainLossByHedgingRelationshipAxis">us-gaap:FairValueHedgingMember</xbrldi:explicitMember>
                          <xbrldi:explicitMember dimension="us-gaap:IncomeStatementLocationAxis">us-gaap:OtherOperatingIncomeExpenseMember</xbrldi:explicitMember>
                        </xbrli:segment>
                      </xbrli:entity>
                    </xbrli:context>
                    <xbrli:context id="deriv_nondesignated">
                      <xbrli:period>
                        <xbrli:startDate>2024-01-01</xbrli:startDate>
                        <xbrli:endDate>2024-12-31</xbrli:endDate>
                      </xbrli:period>
                      <xbrli:entity>
                        <xbrli:segment>
                          <xbrldi:explicitMember dimension="us-gaap:DerivativeInstrumentRiskAxis">us-gaap:ForeignExchangeContractMember</xbrldi:explicitMember>
                          <xbrldi:explicitMember dimension="us-gaap:HedgingDesignationAxis">us-gaap:NondesignatedMember</xbrldi:explicitMember>
                          <xbrldi:explicitMember dimension="us-gaap:IncomeStatementLocationAxis">us-gaap:CostOfSalesMember</xbrldi:explicitMember>
                        </xbrli:segment>
                      </xbrli:entity>
                    </xbrli:context>
                    <xbrli:context id="deriv_disallowed">
                      <xbrli:period>
                        <xbrli:startDate>2024-01-01</xbrli:startDate>
                        <xbrli:endDate>2024-12-31</xbrli:endDate>
                      </xbrli:period>
                      <xbrli:entity>
                        <xbrli:segment>
                          <xbrldi:explicitMember dimension="us-gaap:DerivativeInstrumentRiskAxis">us-gaap:ForeignExchangeContractMember</xbrldi:explicitMember>
                          <xbrldi:explicitMember dimension="us-gaap:HedgingDesignationAxis">us-gaap:NondesignatedMember</xbrldi:explicitMember>
                          <xbrldi:explicitMember dimension="mmm:CustomBreakdownAxis">mmm:CustomMember</xbrldi:explicitMember>
                        </xbrli:segment>
                      </xbrli:entity>
                    </xbrli:context>
                  </ix:resources>
                </div>
                <ix:nonFraction unitRef="usd" contextRef="deriv_fair_value" name="us-gaap:DerivativeGainLossOnDerivativeNet" scale="6">6</ix:nonFraction>
                <ix:nonFraction unitRef="usd" contextRef="deriv_nondesignated" name="us-gaap:DerivativeInstrumentsNotDesignatedAsHedgingInstrumentsGainLossNet" scale="6">22</ix:nonFraction>
                <ix:nonFraction unitRef="usd" contextRef="deriv_disallowed" name="us-gaap:DerivativeInstrumentsNotDesignatedAsHedgingInstrumentsGainLossNet" scale="6">999</ix:nonFraction>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("inline xbrl derivative extraction should succeed");

        let derivative_gain_loss = extracted
            .iter()
            .find(|metric| {
                metric.metric_id.as_str() == "derivatives_and_securities.derivative_gain_loss"
            })
            .expect("aggregated derivative gain/loss should be extracted");

        assert_eq!(derivative_gain_loss.numeric_value.amount, 28.0);
        assert_eq!(derivative_gain_loss.numeric_value.scale, ValueScale::Millions);
        assert!(matches!(
            derivative_gain_loss.numeric_value.reporting_period.context,
            PeriodContext::Duration { start, end }
                if start == date!(2024 - 01 - 01) && end == date!(2024 - 12 - 31)
        ));
    }

    #[test]
    fn extracts_inline_xbrl_share_issuance_proceeds_from_award_context() {
        let extractor = HtmlExtractor::default();
        let filing = sample_filing();
        let html = r#"
            <html>
              <body>
                <div style="display:none">
                  <ix:resources>
                    <xbrli:context id="option_award_2024">
                      <xbrli:period>
                        <xbrli:startDate>2024-01-01</xbrli:startDate>
                        <xbrli:endDate>2024-12-31</xbrli:endDate>
                      </xbrli:period>
                      <xbrli:entity>
                        <xbrli:segment>
                          <xbrldi:explicitMember dimension="us-gaap:AwardTypeAxis">us-gaap:EmployeeStockOptionMember</xbrldi:explicitMember>
                        </xbrli:segment>
                      </xbrli:entity>
                    </xbrli:context>
                    <xbrli:context id="option_award_disallowed">
                      <xbrli:period>
                        <xbrli:startDate>2024-01-01</xbrli:startDate>
                        <xbrli:endDate>2024-12-31</xbrli:endDate>
                      </xbrli:period>
                      <xbrli:entity>
                        <xbrli:segment>
                          <xbrldi:explicitMember dimension="us-gaap:AwardTypeAxis">us-gaap:EmployeeStockOptionMember</xbrldi:explicitMember>
                          <xbrldi:explicitMember dimension="mmm:CustomAxis">mmm:CustomMember</xbrldi:explicitMember>
                        </xbrli:segment>
                      </xbrli:entity>
                    </xbrli:context>
                  </ix:resources>
                </div>
                <ix:nonFraction unitRef="usd" contextRef="option_award_2024" name="us-gaap:ProceedsFromStockOptionsExercised" scale="6">26</ix:nonFraction>
                <ix:nonFraction unitRef="usd" contextRef="option_award_disallowed" name="us-gaap:ProceedsFromStockOptionsExercised" scale="6">999</ix:nonFraction>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("inline xbrl equity compensation extraction should succeed");

        let proceeds = extracted
            .iter()
            .find(|metric| metric.metric_id.as_str() == "cash_flow.share_issuance_proceeds")
            .expect("share issuance proceeds should be extracted");

        assert_eq!(proceeds.numeric_value.amount, 26.0);
        assert_eq!(proceeds.numeric_value.scale, ValueScale::Millions);
        assert!(matches!(
            proceeds.numeric_value.reporting_period.context,
            PeriodContext::Duration { start, end }
                if start == date!(2024 - 01 - 01) && end == date!(2024 - 12 - 31)
        ));
    }

    #[test]
    fn extracts_zero_revolver_balance_when_credit_facility_is_explicitly_undrawn() {
        let extractor = HtmlExtractor::default();
        let filing = sample_filing();
        let html = r#"
            <html>
              <body>
                <p>
                  The Company has a $4.25 billion five-year revolving credit facility that expires in May 2028.
                  The credit facility was undrawn at December 31, 2024.
                </p>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &filing)
            .expect("undrawn revolver disclosure should extract");

        let revolver = extracted
            .iter()
            .find(|metric| metric.metric_id.as_str() == "debt_and_credit.revolver_balance")
            .expect("revolver balance should be extracted as zero");

        assert_eq!(revolver.numeric_value.amount, 0.0);
        assert_eq!(revolver.numeric_value.provenance.source_method, FilingSourceMethod::FilingText);
        assert_eq!(
            revolver.numeric_value.provenance.source_location.section_name.as_deref(),
            Some("credit_facility_text_disclosure")
        );
    }

    #[test]
    fn selects_current_period_cell_instead_of_last_numeric_cell() {
        let extractor = HtmlExtractor::default();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Consolidated Statements of Operations (in millions)</caption>
                  <tr>
                    <th></th>
                    <th>December 31, 2024</th>
                    <th>December 31, 2023</th>
                    <th>Reference</th>
                  </tr>
                  <tr>
                    <th>Total revenue</th>
                    <td>17,737</td>
                    <td>16,226</td>
                    <td>7</td>
                  </tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &sample_filing())
            .expect("html fallback extraction should succeed");
        let revenue = extracted
            .iter()
            .find(|metric| metric.metric_id.as_str() == "income_statement.revenue")
            .expect("revenue should be extracted");

        assert_eq!(revenue.numeric_value.amount, 17_737.0);
    }

    #[test]
    fn property_plant_equipment_html_requires_net_style_label() {
        let extractor = HtmlExtractor::default();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Consolidated Balance Sheets (in millions)</caption>
                  <tr><th>Property and equipment</th><td>1,156</td></tr>
                  <tr><th>Property and equipment, net</th><td>1,628</td></tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &sample_filing())
            .expect("html fallback extraction should succeed");
        let ppe = extracted
            .iter()
            .filter(|metric| metric.metric_id.as_str() == "balance_sheet.property_plant_equipment")
            .collect::<Vec<_>>();

        assert_eq!(ppe.len(), 1);
        assert_eq!(ppe[0].numeric_value.amount, 1_628.0);
    }

    #[test]
    fn rejects_loose_numeric_html_matches() {
        let extractor = HtmlExtractor::default();
        let html = r#"
            <html>
              <body>
                <table>
                  <caption>Consolidated Balance Sheets (in millions)</caption>
                  <tr><th>Total current liabilities</th><td>1502</td></tr>
                  <tr><th></th><td>1</td></tr>
                </table>
                <table>
                  <caption>Consolidated Statements of Operations (in millions, except per share data)</caption>
                  <tr><th>Total expenses</th><td>961</td></tr>
                  <tr><th>Diluted</th><td>243</td></tr>
                  <tr><th>(In millions, except per share data)</th><td>2012</td></tr>
                </table>
                <table>
                  <caption>Consolidated Statements of Cash Flows</caption>
                  <tr><th>Operating cash flow</th><td>47</td></tr>
                </table>
              </body>
            </html>
        "#;

        let extracted = extractor
            .extract_numeric_fallbacks(html, &sample_filing())
            .expect("html fallback extraction should succeed");

        assert!(extracted.is_empty());
    }

    #[test]
    fn extracts_footnotes_and_mda_sections() {
        let extractor = HtmlExtractor::default();
        let html = r#"
            <html>
              <body>
                <h2>Note 1. Summary of Significant Accounting Policies</h2>
                <p>The company prepares its statements in accordance with GAAP.</p>
                <p>Cash equivalents have original maturities of three months or less.</p>

                <h2>Management's Discussion and Analysis of Financial Condition and Results of Operations</h2>
                <p>Revenue increased due to product demand.</p>
                <p>Operating cash flow improved year over year.</p>

                <h2>Risk Factors</h2>
                <p>This should remain a skeleton area for now.</p>
              </body>
            </html>
        "#;

        let sections = extractor.extract_narrative_sections(html, &sample_filing());

        assert!(sections.iter().any(|section| {
            section.metric_id.as_str() == "footnotes.disclosure_text"
                && matches!(section.value, MetricValue::Text(_))
        }));
        assert!(sections.iter().any(|section| {
            section.metric_id.as_str() == "mda.management_discussion_text"
                && matches!(section.value, MetricValue::Text(_))
        }));
        assert!(
            !sections
                .iter()
                .any(|section| section.metric_id.as_str() == "risk_factors.placeholder")
        );
    }

    #[test]
    fn extracts_narrative_sections_from_inline_sec_style_headings() {
        let extractor = HtmlExtractor::default();
        let html = r#"
            <html>
              <body>
                <div>Item 7. Management's Discussion and Analysis of Financial Condition and Results of Operations</div>
                <div>Revenue and margins changed during the year.</div>
                <div>Liquidity remained sufficient.</div>

                <span>Notes to Consolidated Financial Statements</span>
                <div>Note 1. Basis of Presentation</div>
                <div>The company follows GAAP.</div>
              </body>
            </html>
        "#;

        let sections = extractor.extract_narrative_sections(html, &sample_filing());

        assert!(sections.iter().any(|section| {
            section.metric_id.as_str() == "mda.management_discussion_text"
                && matches!(section.value, MetricValue::Text(_))
        }));
        assert!(sections.iter().any(|section| {
            section.metric_id.as_str() == "footnotes.disclosure_text"
                && matches!(section.value, MetricValue::Text(_))
        }));
    }

    #[test]
    fn fixture_html_for_selected_cik_extracts_fallbacks_and_narrative() {
        let extractor = HtmlExtractor::default();
        let html = include_str!("../../../fixtures/0000798354/filing_sample.html");
        let result =
            extractor.extract(html, &sample_filing()).expect("fixture html should extract");

        assert!(
            result
                .numeric_fallbacks
                .iter()
                .any(|metric| metric.metric_id.as_str() == "balance_sheet.cash_and_equivalents")
        );
        assert!(
            result
                .numeric_fallbacks
                .iter()
                .any(|metric| metric.metric_id.as_str() == "income_statement.net_income")
        );
        assert!(
            result
                .narrative_sections
                .iter()
                .any(|section| section.metric_id.as_str() == "footnotes.disclosure_text")
        );
        assert!(
            result
                .narrative_sections
                .iter()
                .any(|section| section.metric_id.as_str() == "mda.management_discussion_text")
        );
    }
}
