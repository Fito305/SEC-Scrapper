# Workbook Schema

Schema version: `0.1.0`

The workbook is an `.xlsx` file with one worksheet per domain.

## Layout

- Each domain sheet uses one row per metric.
- `segment_data` is the exception: it uses one row per `segment_name + metric`.
- Periods are represented as columns.
- The `schema` sheet stores schema version and layout metadata.
- The `company_overview` sheet stores issuer identity.

## Core Sheets

- `company_overview`
- `filing_index`
- `balance_sheet`
- `income_statement`
- `cash_flow`
- `shareholders_equity`
- `segment_data`
- `debt_and_credit`
- `derivatives_and_securities`
- `equity_compensation`
- `footnotes`
- `mda`
- `valuation`
- `provenance`
- `schema`

## Import Rule

Import must validate the `schema` sheet before trusting workbook content. Unsupported schema versions should fail clearly.
