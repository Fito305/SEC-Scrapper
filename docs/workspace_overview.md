# Workspace Overview

This document describes the current workspace scaffold and why each crate exists.

## Root Layout

- `crates/`: Rust crates that map to the architectural boundaries from `AGENT.md`
- `docs/`: developer-facing notes, design docs, and future implementation guidance
- `fixtures/`: real SEC filing fixtures and mocks used by tests

## Crate Responsibilities

- `sec_client`: SEC HTTP policy, headers, throttling, retries, and document download entry points
- `filing_discovery`: lookup and filtering of filing metadata for a single issuer
- `filing_models`: shared types for companies, filings, periods, and provenance
- `xbrl_extractor`: structured numeric extraction from XBRL sources
- `html_extractor`: HTML/text fallback extraction for numeric and narrative sections
- `normalization`: mapping raw extracted values into stable internal domain models
- `accounting_domains`: domain-oriented accounting models grouped by business meaning
- `valuation`: placeholder valuation formulas and later user-editable derived calculations
- `forex`: future currency conversion abstraction
- `workbook_io`: export/import of versioned `.xlsx` workbooks
- `app_core`: shared application config, errors, and tracing bootstrap
- `cli`: command-line entry point and workflow orchestration
- `api_adapter_skeleton`: future API-facing traits and DTO placeholders

## Immediate Next Work

1. Wire live SEC fetch orchestration behind explicit CLI flags.
2. Expand real filing fixtures for CIK `0000798354`.
3. Add end-to-end tests that run from fixture submissions through workbook export.
4. Replace placeholder valuation formulas when the final formulas are defined.
