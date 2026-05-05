# Fixture Set: CIK 0000798354

This fixture set anchors local development without requiring live SEC network access.

Purpose:

- validate SEC company facts parsing
- validate XBRL-first extraction against canonical metric IDs
- validate HTML numeric fallback extraction
- validate footnote and MD&A extraction

The files are intentionally small and synthetic-shaped. They use the selected CIK and SEC-like field names, but they are not a full filing archive.

Future fixture expansion should add real downloaded filing assets under period-specific folders:

- `10-K/2024/`
- `10-Q/2025-Q1/`
- `metadata/`
