# HTML Multi-Period Extraction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract multiple historical periods from SEC HTML tables by mapping each numeric column to the exact period stated in the table headers, then export those values into the correct workbook period columns.

**Architecture:** Extend `html_extractor` so table parsing becomes header-aware instead of row-only. The extractor should parse header cells into exact `ReportingPeriod` values, map each row’s numeric cells to those periods, and emit one `ExtractedHtmlMetricValue` per metric-period pair. `app_workflow` keeps using XBRL first and HTML second, but HTML fallback will now be able to recover prior-period values when the table headers explicitly state the date. Review warnings remain the safety mechanism when headers cannot be parsed confidently.

**Tech Stack:** Rust workspace, `scraper`, `time`, current `filing_models` / `html_extractor` / `app_workflow` crates, existing `cargo test` workflow.

---

## File Structure

- Modify: `crates/html_extractor/src/lib.rs`
  Purpose: add header parsing, per-column period mapping, and multi-period HTML numeric extraction.
- Modify: `crates/app_workflow/src/lib.rs`
  Purpose: keep XBRL-vs-HTML de-duplication aligned with the new HTML period handling.
- Modify: `crates/workbook_io/src/lib.rs`
  Purpose: verify exported period columns remain stable once HTML starts contributing multiple explicit historical periods.
- Modify: `docs/implementation_status.md`
  Purpose: record that HTML fallback now supports explicit multi-period recovery, but still does not infer dates not stated in the table.

### Task 1: Add Header-Aware HTML Extraction

**Files:**
- Modify: `crates/html_extractor/src/lib.rs`
- Test: `crates/html_extractor/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Add these tests to `crates/html_extractor/src/lib.rs` inside the existing test module:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p html_extractor extracts_multiple_periods_from_explicit_balance_sheet_headers
cargo test -p html_extractor extracts_multiple_periods_from_explicit_duration_headers
```

Expected:
- both tests fail because `extract_numeric_fallbacks_from_document` currently emits only one numeric cell per row

- [ ] **Step 3: Add header-period parsing and per-column extraction**

In `crates/html_extractor/src/lib.rs`, add:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
struct HtmlColumnPeriod {
    column_index: usize,
    reporting_period: ReportingPeriod,
}
```

Add helper functions near the existing table helpers:

```rust
fn table_column_periods(
    table: &ElementRef<'_>,
    cell_selector: &Selector,
    filing: &FilingMetadata,
) -> Vec<HtmlColumnPeriod> {
    let row_selector = selector("tr");
    let Some(header_row) = table.select(&row_selector).next() else {
        return Vec::new();
    };

    header_row
        .select(cell_selector)
        .enumerate()
        .filter_map(|(column_index, cell)| {
            let text = text_content(cell);
            parse_header_reporting_period(&text, filing)
                .map(|reporting_period| HtmlColumnPeriod { column_index, reporting_period })
        })
        .collect()
}

fn parse_header_reporting_period(
    header_text: &str,
    filing: &FilingMetadata,
) -> Option<ReportingPeriod> {
    let normalized = normalize_label(header_text);
    let end = parse_header_end_date(header_text)?;

    if normalized.contains("three months ended") {
        return Some(ReportingPeriod {
            context: PeriodContext::Duration {
                start: end.previous_day()?.previous_day()?.previous_day().unwrap_or(end),
                end,
            },
            fiscal_period: filing.fiscal_period.clone(),
            label: Some("Three months ended".to_string()),
        });
    }

    if normalized.contains("nine months ended") {
        return Some(ReportingPeriod {
            context: PeriodContext::Duration {
                start: time::Date::from_calendar_date(end.year(), time::Month::January, 1).ok()?,
                end,
            },
            fiscal_period: filing.fiscal_period.clone(),
            label: Some("Nine months ended".to_string()),
        });
    }

    Some(ReportingPeriod {
        context: PeriodContext::Instant { as_of: end },
        fiscal_period: filing.fiscal_period.clone(),
        label: Some("Explicit table header".to_string()),
    })
}
```

Then update `extract_numeric_fallbacks_from_document` so it loops through header-mapped numeric columns instead of selecting just one cell:

```rust
let column_periods = table_column_periods(&table, &cell_selector, filing);

for row in table.select(&row_selector) {
    let cells: Vec<String> = row.select(&cell_selector).map(text_content).collect();
    if cells.len() < 2 {
        continue;
    }

    // existing row-label matching stays

    let explicit_period_values = column_periods
        .iter()
        .filter_map(|column_period| {
            let cell = cells.get(column_period.column_index)?;
            let (amount, sign_convention) = parse_numeric_cell(cell)?;
            Some((column_period.reporting_period.clone(), amount, sign_convention, cell.clone()))
        })
        .collect::<Vec<_>>();

    if !explicit_period_values.is_empty() {
        for (reporting_period, amount, sign_convention, row_value_text) in explicit_period_values {
            // build provenance + ExtractedHtmlMetricValue exactly as today,
            // but use `reporting_period` instead of `reporting_period_from_filing(filing)`
        }
        continue;
    }

    // keep existing single-period fallback path for tables that do not expose explicit periods
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run:

```bash
cargo test -p html_extractor extracts_multiple_periods_from_explicit_balance_sheet_headers
cargo test -p html_extractor extracts_multiple_periods_from_explicit_duration_headers
```

Expected:
- both tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/html_extractor/src/lib.rs
git commit -m "feat: extract explicit multi-period values from html tables"
```

### Task 2: Harden Date Parsing For Explicit HTML Headers

**Files:**
- Modify: `crates/html_extractor/src/lib.rs`
- Test: `crates/html_extractor/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

Add:

```rust
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
                    <th>Reference</th>
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
```

- [ ] **Step 2: Run test to verify it fails if reference columns are still leaking**

Run:

```bash
cargo test -p html_extractor ignores_reference_columns_when_multi_period_headers_exist
```

Expected:
- fail if the extractor still treats non-period columns as metric values

- [ ] **Step 3: Tighten explicit-header filtering**

In `crates/html_extractor/src/lib.rs`, update `table_column_periods` so only columns with parseable periods are used:

```rust
header_row
    .select(cell_selector)
    .enumerate()
    .filter_map(|(column_index, cell)| {
        let text = text_content(cell);
        parse_header_reporting_period(&text, filing)
            .filter(|_| !normalize_label(&text).contains("reference"))
            .map(|reporting_period| HtmlColumnPeriod { column_index, reporting_period })
    })
    .collect()
```

Also add a small helper:

```rust
fn parse_header_end_date(header_text: &str) -> Option<time::Date> {
    let cleaned = header_text.replace('.', "");
    let normalized = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");

    for candidate in normalized.split(',').collect::<Vec<_>>().windows(2) {
        let value = format!("{},{}", candidate[0].trim(), candidate[1].trim());
        if let Ok(date) = time::Date::parse(&value, &time::macros::format_description!(
            "[month repr:long] [day], [year]"
        )) {
            return Some(date);
        }
    }

    time::Date::parse(
        &normalized,
        &time::macros::format_description!("[month repr:long] [day], [year]"),
    )
    .ok()
}
```

- [ ] **Step 4: Run test to verify it passes**

Run:

```bash
cargo test -p html_extractor ignores_reference_columns_when_multi_period_headers_exist
```

Expected:
- PASS

- [ ] **Step 5: Commit**

```bash
git add crates/html_extractor/src/lib.rs
git commit -m "fix: ignore non-period columns in html multi-period tables"
```

### Task 3: Keep Workflow De-Duplication Correct With HTML Historical Periods

**Files:**
- Modify: `crates/app_workflow/src/lib.rs`
- Test: `crates/app_workflow/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Add:

```rust
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
```

- [ ] **Step 2: Run test to verify it fails if current logic over-prunes**

Run:

```bash
cargo test -p app_workflow html_historical_periods_survive_when_xbrl_only_covers_current_period
```

Expected:
- fail if `keep_html_only_where_xbrl_is_missing` incorrectly deletes historical HTML values that do not overlap the XBRL period

- [ ] **Step 3: Keep de-duplication keyed by metric plus explicit period end**

In `crates/app_workflow/src/lib.rs`, keep the existing end-date key logic, but add a comment documenting the intended behavior:

```rust
// HTML fallback can now emit multiple explicit periods from a single table.
// We only remove HTML values when XBRL already covers the same metric and
// same period end. Historical HTML periods with different end dates must stay.
```

If the test fails, adjust `reporting_period_end_key` usage so it remains metric + period-end specific only, without broader filing-level pruning.

- [ ] **Step 4: Run test to verify it passes**

Run:

```bash
cargo test -p app_workflow html_historical_periods_survive_when_xbrl_only_covers_current_period
```

Expected:
- PASS

- [ ] **Step 5: Commit**

```bash
git add crates/app_workflow/src/lib.rs
git commit -m "fix: preserve html historical periods when xbrl covers only current period"
```

### Task 4: Validate Workbook Period Expansion

**Files:**
- Modify: `crates/workbook_io/src/lib.rs`
- Test: `crates/workbook_io/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Add:

```rust
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
        normalized.numeric_metrics.push(prior_period_metric);

        let model = exporter.build_model(sample_company(), &normalized, &[sample_valuation_output()]);

        assert!(model.period_columns.iter().any(|column| column.column_label == "FY2023"));
        assert!(model.period_columns.iter().any(|column| column.column_label == "FY2024"));
    }
```

- [ ] **Step 2: Run test to verify it fails if period columns collapse**

Run:

```bash
cargo test -p workbook_io workbook_builds_multiple_period_columns_when_html_contributes_history
```

Expected:
- fail if period-column generation still collapses historical HTML periods

- [ ] **Step 3: Keep workbook period-column behavior stable**

If the test fails, update `crates/workbook_io/src/lib.rs` so:
- period columns are still built from all normalized numeric metrics
- HTML-contributed historical periods remain distinct
- labels continue to render as `FY2023`, `Q1 2025`, etc.

Use the existing `build_period_columns` / `period_label` path rather than a new workbook-specific path.

- [ ] **Step 4: Run test to verify it passes**

Run:

```bash
cargo test -p workbook_io workbook_builds_multiple_period_columns_when_html_contributes_history
```

Expected:
- PASS

- [ ] **Step 5: Commit**

```bash
git add crates/workbook_io/src/lib.rs
git commit -m "test: preserve workbook period columns for html historical values"
```

### Task 5: Document The New Historical HTML Scope

**Files:**
- Modify: `docs/implementation_status.md`

- [ ] **Step 1: Update documentation**

Append this section to `docs/implementation_status.md`:

```markdown
## HTML Multi-Period Extraction

- HTML fallback now supports extracting multiple explicit periods from a single SEC table when the header states the period directly.
- Current implementation trusts only periods explicitly stated in the table headers.
- The extractor does not infer unstated dates from surrounding filing context.
- Non-period numeric columns such as reference or footnote columns are ignored.
- Future enhancement: support more SEC header phrasings and broader historical-table coverage while preserving exact period assignment.
```

- [ ] **Step 2: Commit**

```bash
git add docs/implementation_status.md
git commit -m "docs: record html multi-period extraction scope"
```
