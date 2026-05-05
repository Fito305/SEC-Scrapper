# Testing

Tests should prefer fixtures and mocks over live SEC network calls.

## Current Fixtures

Primary fixture CIK: `0000798354`

Fixture files:

- `fixtures/0000798354/companyfacts_sample.json`
- `fixtures/0000798354/filing_sample.html`

## Current Test Coverage

- SEC policy and endpoint construction
- filing discovery parsing/filtering
- metric registry lookups
- XBRL company facts extraction
- HTML numeric fallback and narrative extraction
- normalization source precedence
- placeholder valuation formulas
- workbook export/import schema validation

## Future Test Work

- add real downloaded filing fixtures
- add slow/live tests behind explicit opt-in flags
- add snapshot tests for normalized output and workbook layout
- add property tests for numeric sign/scale handling
