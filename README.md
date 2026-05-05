# SEC EDGAR Scraper

This repository contains the Rust workspace for a single-company SEC EDGAR extraction tool.

The project is being built from the specification in [AGENT.md](./AGENT.md).

Current status:

- workspace scaffold created
- crate boundaries established
- shared app configuration, error handling, and tracing bootstrap added
- SEC request policy, discovery shaping, filing asset planning, XBRL extraction, HTML fallback extraction, normalization, placeholder valuation, workbook export/import, and CLI command structure added
- live end-to-end SEC network workflow still needs fixture-backed orchestration and production hardening

See [docs/workspace_overview.md](./docs/workspace_overview.md) for the initial crate map.
See [docs/implementation_status.md](./docs/implementation_status.md) for current status and next work.
