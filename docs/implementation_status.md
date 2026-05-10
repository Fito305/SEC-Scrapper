# Implementation Status

## Complete

- Rust workspace and crate boundaries
- shared filing, period, numeric, provenance, and workbook models
- SEC request policy and endpoint construction
- filing discovery shaping and amended-filing filtering
- filing asset manifest planning and download helper
- canonical metric registry
- XBRL company facts extraction
- HTML numeric fallback extraction
- HTML multi-period fallback extraction from explicit table headers
- footnote and MD&A extraction
- normalization with XBRL preferred and HTML retained as alternative
- placeholder valuation formulas
- `.xlsx` workbook export/import with schema validation
- CLI command structure
- fixture folder for CIK `0000798354`
- Forex, risk-factor, and API adapter skeletons

## Not Yet Complete

- production live SEC fetch orchestration from CLI
- real downloaded fixture archive for multiple periods
- full end-to-end fixture pipeline test from submissions to workbook
- richer workbook import that reconstructs every typed value
- final user-defined valuation formulas
- production API adapter
- production Forex provider
- risk-factor extraction

## Important Maintenance Notes

- Replace valuation formulas in `crates/valuation/src/lib.rs`.
- Add or fix XBRL tag mappings in `crates/accounting_domains/src/lib.rs`.
- HTML fallback now supports multiple explicit periods when the SEC table headers state those periods directly, and it ignores non-period numeric columns like references or footnotes.
- The extractor does not infer unstated dates from surrounding filing context; future work should expand support for multi-row and atypical SEC header phrasing while preserving exact period assignment.
- Segment workbook rows now support one row per `segment_name + metric`, and the extractor now supplements SEC `companyfacts` with inline-XBRL segment facts from filing HTML when member-context facts are not exposed cleanly in `companyfacts`.
- Segment label cleanup is now conservative: obvious display variants such as `Healthc Care Segment` normalize into cleaner workbook row labels, but repeated historical segment facts across later filings still create review-noise that needs additional ranking/deduping work.
- Debt workbook rows now include optional funding-detail metrics under `debt_and_credit.detail_*` when debt note tables clearly expose category rows such as senior notes, subordinated debt, secured borrowings, or other borrowed funds. Current placeholder valuation formulas intentionally ignore those detail rows; if you want formula-level use later, wire them in explicitly from `crates/valuation/src/lib.rs`.
- Keep SEC networking policy centralized in `crates/sec_client`.
- Keep workbook schema changes versioned in `crates/workbook_io`.
