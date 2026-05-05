# Architecture

The program is organized as a Rust workspace with explicit boundaries.

## Data Flow

1. `cli` accepts ticker/CIK and workflow commands.
2. `sec_client` owns SEC request policy, endpoint construction, and filing asset download helpers.
3. `filing_discovery` converts SEC submissions data into original 10-K/10-Q filing metadata.
4. `xbrl_extractor` extracts numeric facts from SEC company facts JSON using the canonical metric registry.
5. `html_extractor` extracts numeric fallback values plus footnotes and MD&A from HTML.
6. `normalization` reconciles XBRL and HTML values with XBRL preferred and HTML retained as an alternative.
7. `valuation` computes placeholder valuation outputs from normalized metrics.
8. `workbook_io` exports/imports versioned `.xlsx` workbooks.

## Main Rule

Retrieval, parsing, normalization, valuation, workbook IO, Forex, and future API concerns must stay separate.

## Future Extension Seams

- `forex`: add historical exchange-rate providers here, not in SEC parsing.
- `api_adapter_skeleton`: add API DTOs and traits here before introducing a web framework.
- `html_extractor::RiskFactorExtractionSkeleton`: implement risk-factor text extraction here later.
