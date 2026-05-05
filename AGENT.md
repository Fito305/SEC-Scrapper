# AGENT.md

## Purpose

Build a Rust application that retrieves original SEC EDGAR filings for a single company at a time, extracts accounting and related disclosure data from up to 10 years of Form 10-K and 10-Q filings, normalizes the results while preserving filing-oriented structure, computes valuation inputs in separate modules, and exports side-by-side company comparison output to a versioned `.xlsx` workbook suitable for later review, upload, and future web display.

This specification is architecture-guiding rather than overly prescriptive. The implementation must favor clean Rust design, refactor-friendly boundaries, and strong auditability.

---

## Product Goals

1. Retrieve original SEC 10-K and 10-Q filings for one company identified by ticker and/or CIK.
2. Support historical retrieval across multiple years and quarters, up to 10 years.
3. Extract numerical accounting data from filing materials with strong traceability.
4. Separate data domains so statements, footnotes, debt, derivatives, and equity compensation are independently modeled and refactorable.
5. Use XBRL preferentially for exact numeric extraction when available.
6. Use HTML/text preferentially for presentation context, source labeling, footnotes, and MD&A text.
7. Preserve human readability and filing structure wherever practical.
8. Export one wide comparison workbook per company spanning all retrieved reporting periods.
9. Allow workbook re-ingestion into the program for later viewing.
10. Keep valuation logic isolated from retrieval and extraction logic.
11. Keep SEC retrieval separate from foreign-exchange conversion logic.
12. Reserve extension points for future support of additional SEC forms, risk-factor extraction, and a future HTTP/API adapter.

---

## Non-Goals

1. Do not handle amended filings such as 10-K/A or 10-Q/A.
2. Do not support multi-company batch ingestion in the initial implementation.
3. Do not produce opinionated fair-value estimates or narrative investment conclusions.
4. Do not mix SEC retrieval concerns with foreign-exchange retrieval concerns.
5. Do not tightly couple core logic to a web framework.
6. Do not prioritize ownership-only forms in the current accounting extraction pipeline.

---

## Filing Scope

### In Scope

- Original Form 10-K
- Original Form 10-Q
- Filing years/quarters covering up to 10 years for one company at a time

### Out of Scope for Now, but Reserve Skeletons

Create extension points, traits, enums, and module placeholders for later support of:

- 20-F
- 6-K
- 8-K with numeric accounting disclosures
- ownership-related forms as a separate future workflow

Do not implement those forms now beyond skeleton interfaces and comments describing intended responsibilities.

---

## Source Priority Rules

### Numeric Extraction Priority

1. Prefer XBRL when exact numeric extraction is available.
2. Fall back to HTML/text extraction when XBRL is missing, incomplete, inconsistent, or unsuitable for the needed value.
3. Preserve both the machine-extracted value and the human-facing filing label whenever possible.

### Presentation and Context Priority

1. Prefer HTML/text for presentation context.
2. Extract footnotes, MD&A, and section-level explanatory text from filing HTML/text.
3. Attach lightweight source metadata to text results.
4. Keep a risk-factor extraction skeleton only in the initial implementation.

### Structure Preservation

Normalization must not erase the original filing meaning. Preserve, wherever practical:

- statement grouping
- reported period labels
- filing date
- fiscal year / fiscal quarter context
- units/scaling
- label wording
- sign conventions
- whether the number came from XBRL or HTML/text fallback

---

## Architectural Requirements

Design the system as a Rust workspace or otherwise clearly separated crate/module layout with strong boundaries.

### Required High-Level Separation

1. **SEC retrieval layer**
   - Handles SEC endpoints, filing discovery, metadata, rate limiting, identity headers, retries, and document downloads.
   - Must be separate from parsers and separate from valuation code.

2. **XBRL extraction layer**
   - Responsible for structured numeric extraction.
   - Must be isolated so it can be refactored independently.

3. **HTML/text extraction layer**
   - Responsible for extracting contextual text and numeric fallbacks from filing HTML/text.
   - Must be isolated from XBRL extraction.

4. **Normalization layer**
   - Responsible for converting raw extracted data into internal domain models while preserving filing-oriented semantics.

5. **Accounting domain layer**
   - Defines the main data structures for statements, footnotes, debt, derivatives, equity compensation, etc.

6. **Valuation layer**
   - Strictly separate modules for derived valuation-related calculations.
   - Must consume normalized domain data rather than parsing raw SEC content.

7. **Forex layer**
   - Separate module/service abstraction for currency translation.
   - Must not be implemented as part of the SEC scraper/parser logic.

8. **Export/import layer**
   - Responsible for generating versioned `.xlsx` workbook output and reloading workbook files for later viewing.

9. **CLI layer**
   - User-facing command-line interface.
   - Should orchestrate, not contain business logic.

10. **Future adapter layer skeleton**
   - Reserve a clear seam for a later HTTP/API adapter without implementing a production web service now.

---

## Suggested Rust Workspace Shape

This is guidance, not a mandatory exact layout, but preserve equivalent separation.

```text
workspace/
├── crates/
│   ├── sec_client/
│   ├── filing_discovery/
│   ├── filing_models/
│   ├── xbrl_extractor/
│   ├── html_extractor/
│   ├── normalization/
│   ├── accounting_domains/
│   ├── valuation/
│   ├── forex/
│   ├── workbook_io/
│   ├── app_core/
│   ├── cli/
│   └── api_adapter_skeleton/
├── fixtures/
├── docs/
└── AGENT.md
```

Equivalent module-based organization within a smaller number of crates is acceptable as long as the separation remains explicit and refactor-friendly.

---

## Rust Requirements

Follow Rust best practices and idioms.

### Language and Design Expectations

- Prefer strong types over loosely typed maps.
- Use enums, newtypes, traits, and explicit domain models where they improve correctness.
- Favor composition over inheritance-like patterns.
- Keep modules small and responsibility-driven.
- Avoid giant God structs and giant God modules.
- Prefer iterators and explicit transformation pipelines where clarity is preserved.
- Prefer `Result`-based error handling over panics in normal control flow.
- Use explicit error types at domain boundaries.
- Keep IO concerns separated from parsing and transformation concerns.
- Keep serialization concerns separated from core domain logic when practical.
- Prefer immutable data flow unless mutation materially improves clarity or performance.
- Document invariants in code comments and type definitions.

### Library Preferences

Use these unless there is a clear, documented reason not to:

- `reqwest` for HTTP
- `tokio` for async runtime
- `serde` and `serde_json` for serialization
- `clap` for CLI
- `chrono` or `time` for dates and periods
- `thiserror` and/or `anyhow` for error handling
- `tracing` and `tracing-subscriber` for logging/observability
- `scraper` or equivalent HTML parsing crate for structured HTML traversal
- `rust_xlsxwriter` or equivalent for workbook export
- `calamine` or equivalent for workbook import
- a dedicated XML/XBRL-capable parser as appropriate

The implementation may choose precise crate combinations, but must justify deviations in code comments or docs if core preferences are not used.

---

## SEC Compliance and Access Rules

The implementation must follow SEC developer guidance.

### Required Behavior

- Use `data.sec.gov` APIs where appropriate for submissions/company metadata and extracted XBRL JSON data.
- Use efficient scripted access.
- Download only what is needed.
- Enforce a hard client-side throttle so the total request rate remains at or below 10 requests per second per user.
- Use a descriptive user agent / identity header strategy consistent with SEC expectations.
- Implement retries with backoff for transient failures.
- Avoid behavior that resembles broad crawling.
- Fail safely when rate limits or access restrictions are encountered.

### Implementation Requirements

- Centralize SEC request policy in one place.
- Make rate limiting configurable, but default to safe SEC-compliant values.
- Add tests around throttling and retry policy.
- Log request source, endpoint class, status, and retry behavior.
- Clearly distinguish API retrieval from HTML filing retrieval.

The SEC states that submissions-by-company and extracted XBRL data are available through `data.sec.gov` REST APIs and that current fair-access guidelines limit each user to no more than 10 requests per second total, regardless of machine count. The SEC also states that excessive or non-compliant automated access may be blocked. citeturn164510view0

---

## Company Identification and Historical Retrieval

Support lookup by:

- ticker
- CIK
- ticker resolved to CIK

For one company per run:

- discover relevant original 10-K and 10-Q filings
- exclude amended filings
- retrieve up to 10 years of filing history
- maintain ordering by reporting period and filing date
- preserve accession numbers and URLs for auditability

Historical coverage should support side-by-side comparison across periods in the exported wide workbook.

---

## Domain Modeling Requirements

All domain data must be categorized into specific related data structures. Do not collapse unrelated concepts into a single unstructured blob.

### Minimum Domain Categories

1. **Company Identity**
   - ticker
   - CIK
   - issuer name
   - exchange if available
   - currency reported
   - fiscal year metadata

2. **Filing Metadata**
   - accession number
   - form type
   - filing date
   - report period end
   - fiscal year / quarter
   - filing URL(s)
   - source type(s): XBRL, HTML, text

3. **Balance Sheet**
   - assets
   - liabilities
   - equity
   - cash and equivalents
   - receivables
   - inventories
   - PP&E
   - goodwill/intangibles
   - current vs long-term debt
   - other relevant line items

4. **Income Statement**
   - revenue
   - COGS
   - gross profit
   - operating expenses
   - operating income
   - interest expense/income
   - tax expense
   - net income
   - EPS fields where available
   - other relevant line items

5. **Cash Flow Statement**
   - operating cash flow
   - investing cash flow
   - financing cash flow
   - depreciation/depletion/amortization
   - capital expenditures
   - stock repurchases
   - share issuance proceeds where available
   - other relevant line items

6. **Statement of Shareholders’ Equity**
   - retained earnings changes
   - common stock changes
   - APIC changes
   - treasury stock changes
   - accumulated OCI changes
   - share-count movements

7. **Segment Data**
   - segment names
   - segment revenue
   - segment profit/loss if available
   - assets by segment if available

8. **Debt and Credit Facilities**
   - revolvers
   - term loans
   - notes/bonds
   - maturities
   - rates when disclosed
   - covenant-related values when numeric and available
   - current vs long-term portions

9. **Derivatives and Debt Securities**
   - derivative instruments
   - fair values
   - gains/losses where disclosed
   - debt securities holdings / classifications / values

10. **Equity Compensation**
   Group stock-based compensation, RSUs, and stock options into a shared domain structure.

   Required fields should allow capture of:
   - GAAP stock-based compensation expense
   - RSU-related data
   - option-related data
   - tax effects related to stock-based compensation when disclosed
   - stock repurchase dollars spent
   - shares repurchased
   - net change in shares outstanding
   - any supporting line items used in derived calculations
   - a calculation breakdown showing which line items were used and how derived values were formed

11. **Footnotes**
   Footnotes must be associated with the related domain area where possible, while also being stored in their own dedicated field(s).

12. **MD&A Text**
   - human-readable text
   - lightweight metadata

13. **Risk Factors Text Skeleton**
   - reserve domain structures and traits only
   - do not implement extraction in the initial version beyond placeholders and comments

14. **Audit Trail / Provenance**
   Every material extracted value must be capable of pointing back to:
   - filing URL
   - accession number
   - form type
   - section or statement
   - XBRL tag when applicable
   - filing label / line item name
   - source location
   - reporting period
   - units/scaling

---

## Text and Metadata Modeling

For text-based sections such as footnotes and MD&A:

- use human-readable string-oriented types
- preserve section title
- preserve filing date
- preserve form type
- preserve source location
- preserve association to the related domain category when known

Do not force narrative text into numeric-only structures.

For risk factors:

- define future-ready placeholder types and extraction interfaces
- document where the implementation should be added later
- do not require live extraction in the initial implementation

---

## Numeric Modeling Rules

Numeric values must support:

- value
- unit/currency
- scale
- sign convention
- source method
- label used in filing
- period context
- provenance metadata

Where practical, model extracted numbers using typed wrappers rather than raw primitives alone.

Examples of concerns the design should account for:

- decimals vs integer counts
- shares vs dollars
- thousands/millions scaling
- negative numbers represented as parentheses in HTML/text
- period instant vs duration semantics

---

## Forex Conversion Requirements

Forex conversion must be designed as a separate integration.

### Rules

- Do not embed Forex logic inside SEC retrieval/parsing modules.
- Provide an abstraction for currency-rate providers.
- Support day-based historical exchange-rate lookup where currency translation is needed.
- Keep converted USD values distinguishable from original reported values.
- Preserve original currency and original reported amount.
- Ensure the conversion layer can be replaced later.

This integration should be optional and invoked only where currency translation is needed.

---

## Extraction Behavior Requirements

### XBRL Extraction

The XBRL path should:

- prefer exact tagged numeric data when available
- preserve tag names
- preserve labels where retrievable
- preserve period coverage
- preserve units/scales
- map values into domain models through normalization
- support conflicting-tag handling with explicit strategy comments/tests

### HTML/Text Extraction

The HTML/text path should:

- parse statement tables when needed as fallback
- parse footnotes and relevant narrative sections
- preserve headings and table/section context
- extract text with source metadata
- support numeric fallback extraction when XBRL is unavailable or insufficient
- extract MD&A text and metadata in the initial implementation
- leave risk-factor extraction as a documented skeleton only

### Conflict Handling

When XBRL and HTML/text disagree:

- record source provenance clearly
- prefer XBRL by default when it is available and suitable for the metric
- use HTML/text fallback when XBRL is missing, incomplete, or clearly unsuitable
- avoid silently discarding important alternative values
- expose enough metadata for later human review

---

## Valuation Module Requirements

Derived valuation logic must live in separate modules from extraction.

### Required Valuation Outputs

Implement valuation-related data derivation only. Do not output investment opinions.

The initial implementation must use placeholder formulas that are intentionally easy to replace later. Those placeholder formulas must still be wired into the CLI, normalization output, and workbook export so the full program flow is exercised from day one.

#### 1. Owner’s Earnings

Implement a dedicated module for owner’s earnings as a placeholder formula.

Initial implementation rules:

- accept normalized inputs through typed parameters or clearly named generic placeholder wrappers
- return a real computed value so CLI and workbook output continue to work
- use `Result`-based handling with explicit `match` arms for edge cases and missing inputs
- add detailed code comments explaining:
  - that the formula is intentionally temporary
  - where the formula is called from
  - which inputs should be replaced later
  - where a future Treasury-yield provider will plug in

The final intended formula will later be replaced by the user.

#### 2. True Owner’s Earnings / Adjusted Earnings Ratio

Implement a separate module for the true owner’s earnings / adjusted earnings ratio as a placeholder formula.

Initial implementation rules:

- return a simple temporary calculation so the end-to-end pipeline works
- use `Result`-based handling with explicit `match` arms for:
  - zero or missing shares repurchased
  - missing stock-based compensation tax effects
  - zero or negative GAAP net income
  - other placeholder edge cases discovered during implementation
- add detailed code comments explaining:
  - why the current formula is temporary
  - where the function is called and surfaced
  - how to replace the formula later

### Valuation Module Rules

- Keep formulas isolated in separate modules/files.
- Include clear breakdown outputs showing inputs and line items used.
- Preserve provenance for each input used in a derived metric.
- Make formulas testable independently from SEC access.
- Accept normalized domain data as inputs.
- Keep the code in these modules especially readable and heavily commented where future edits are expected.

---

## Workbook Export and Import Requirements

### Export

Generate one wide comparison `.xlsx` workbook per company.

The workbook should support side-by-side review across historical periods and should be suitable for later web display or upload back into the program.

The export format should:

- include company identifier columns
- include filing metadata columns
- include one row per metric, with historical periods as columns
- include historical periods aligned for comparison, extending as far back as 10 years where available
- use separate worksheets for each domain grouping
- include derived valuation outputs in clearly separated columns
- include enough provenance fields or references to support auditability without making the workbook unusable
- keep domain categorization in the domain worksheet itself rather than duplicating values under source statements

The workbook layout should optimize for human readability first, while remaining machine-parseable.

The workbook schema must be versioned from the initial implementation.

Recommended minimum worksheet layout:

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

### Import

Provide workbook re-ingestion so exported data can later be loaded and viewed by the program.

The import path should:

- validate schema/version
- fail clearly on malformed or incompatible files
- preserve typed interpretation where practical
- preserve all typed values, provenance references, and review/display-ready data required for later reuse

---

## CLI Requirements

Provide a CLI that orchestrates end-to-end workflows.

### Example Capability Areas

- resolve ticker/CIK
- fetch filing metadata
- retrieve 10-K/10-Q history
- extract and normalize data
- export wide workbook
- import existing workbook for review
- show extraction summary
- show provenance summary for a selected metric

Do not hardcode exact command names unless needed. A clean `clap`-based command hierarchy is expected.

---

## Future API Adapter Skeleton

Do not build a full production HTTP API now.

Do reserve a clean seam so a future API adapter can:

- query normalized company/filing data
- serve workbook-derived views
- expose provenance for selected metrics
- support a future web UI without rewriting the core extraction logic

This should remain a skeleton or placeholder only.

---

## Error Handling Requirements

- Use typed errors at module boundaries.
- Avoid `unwrap`/`expect` in production paths unless justified.
- Surface partial-failure states clearly.
- Distinguish retrieval errors, parse errors, normalization errors, export errors, and valuation errors.
- Distinguish workbook schema/version errors from data extraction errors.
- Preserve context in error messages.
- Allow the user to understand which filing/period/source failed.

---

## Logging and Observability

Use structured logging.

At minimum, log:

- company requested
- filing discovered
- endpoint hit
- source type used
- fallback behavior
- extraction coverage summary
- retry/rate-limit behavior
- export completion
- major warnings and data gaps

Prefer `tracing`-based instrumentation.

---

## Testing Requirements

All tests must live in the appropriate modules/crates.

### Required Test Types

1. **Unit Tests**
   - domain transformations
   - formula modules
   - metadata mapping
   - workbook layout logic
   - parsing helpers

2. **Integration Tests**
   - end-to-end extraction for representative filings
   - SEC client policy behavior using mocks/fixtures
   - workbook export/import round trips

3. **Snapshot Tests**
   - representative extracted output from real filing fixtures
   - normalized statement representations
   - workbook sheet/header/layout stability where appropriate

4. **Property-Based Tests**
   - numeric normalization behaviors
   - scaling/sign handling
   - workbook round-trip expectations where feasible

5. **Fixture-Based Tests Using Real SEC Filings**
   - use carefully selected real 10-K and 10-Q fixtures
   - start with CIK `0000798354` as the primary fixture company
   - keep fixtures organized by company/form/period
   - document what each fixture validates

### Testing Rules

- Tests must not rely on live SEC network access by default.
- Prefer fixtures and mocks for reproducibility.
- Clearly separate slow tests from fast tests if needed.
- Test both XBRL-primary and HTML/text-fallback scenarios.
- Test provenance population.
- Test exclusion of amended filings.
- Test rate-limiting and retry policy.
- Test valuation formulas independently from retrieval.
- Support both fixture-based development and optional live SEC retrieval paths.

---

## Documentation Requirements

Include developer-facing documentation that explains:

- workspace/module responsibilities
- data flow from retrieval to export
- provenance model
- XBRL vs HTML/text priority rules
- workbook schema/versioning
- extension points for new form types
- extension points for future API adapter
- extension points for Forex provider replacement
- where placeholder valuation formulas should be edited later

Document important assumptions close to the code.

---

## Extension Points

Design for future addition of:

- more SEC form handlers
- ownership-related workflows as separate modules
- alternate valuation formulas
- alternate export formats
- HTTP/API adapter
- additional storage backends if needed later
- risk-factor extraction

These extension points must not compromise current clarity.

---

## Implementation Guardrails

- Keep modules refactor-friendly.
- Prefer explicitness over cleverness.
- Preserve human readability.
- Prioritize readable Rust code over compact but harder-to-maintain abstractions.
- Preserve traceability of numbers above all.
- Do not sacrifice auditability for convenience.
- Do not tightly couple formula code to parser details.
- Do not tightly couple export code to SEC retrieval code.
- Do not silently coerce ambiguous data without recording enough metadata.
- Add maintenance-oriented comments in code where future edits are expected.

---

## Risk Reduction Strategy

Reduce implementation risk by locking a few design choices early.

### Canonical Metric Registry

Create a canonical metric registry from day one.

Each metric definition should be able to hold:

- internal metric ID
- preferred XBRL tags
- alternate tags
- expected domain/statement mapping
- expected units
- notes about known conflicts or issuer variation

This registry should be the main place where XBRL concept variation is handled.

### HTML Fallback Scope

Treat HTML/text extraction as a fallback and context layer rather than a second primary truth system.

The initial implementation should:

- prefer XBRL for numeric extraction whenever it is suitable
- use HTML/text numeric fallback only for missing, incomplete, or clearly unsuitable XBRL coverage
- preserve the raw table/section context for every fallback value
- mark fallback-derived values clearly in provenance

### Shared Provenance Model

Use one shared provenance model across all domains rather than repeating similar fields in many structures.

At minimum, provenance should preserve:

- accession number
- filing URL
- source type
- source location
- filing label
- XBRL tag when available
- period
- unit/scale

### Versioned Workbook Schema

Version the workbook schema immediately.

Define:

- workbook version
- fixed worksheet names
- required columns per worksheet
- import compatibility behavior by version

### Implementation Order

Build the program in an order that reduces ambiguity:

1. workspace and shared types
2. SEC retrieval and filing discovery
3. XBRL extraction for core statements
4. normalization and provenance
5. workbook export/import
6. domain expansions for debt, derivatives, segment data, equity compensation, footnotes, and MD&A
7. placeholder valuation modules
8. future skeletons such as risk factors, Forex, and API adapter

This order is preferred even though the overall architecture should exist from day one.

---

## Initial Delivery Scope

The first implementation should still be a complete end-to-end program, but it may expand coverage in stages.

The minimum end-to-end slice must include:

- ticker/CIK input
- filing discovery with amended-filing exclusion
- XBRL-first extraction for major statements
- HTML/text fallback for missing numeric data
- footnotes and MD&A support
- normalization into domain models
- workbook export and import
- CLI summary and provenance reporting
- placeholder valuation functions that are actually executed and exported

Debt, derivatives, segment data, and equity compensation remain required in the initial implementation and should be built immediately after the core statement pipeline is stable.

---

## Acceptance Criteria

A correct implementation should be able to:

1. Accept a single ticker/CIK for one company.
2. Resolve and retrieve up to 10 years of original 10-K and 10-Q filings.
3. Exclude amended filings.
4. Use `data.sec.gov` where appropriate for submission/XBRL retrieval.
5. Respect SEC fair-access constraints with client-side throttling and compliant access behavior. citeturn164510view0
6. Extract major statement data into distinct domain structures.
7. Extract footnotes and MD&A with lightweight metadata, and provide a future-ready risk-factor skeleton.
8. Capture debt, derivative, debt-security, and equity-compensation data in dedicated categories.
9. Preserve provenance for material extracted values.
10. Compute the required derived valuation outputs in separate modules using placeholder formulas that are easy to replace later.
11. Export one wide side-by-side `.xlsx` workbook per company with domain worksheets and one row per metric.
12. Re-ingest that workbook for later viewing.
13. Provide module-level tests, integration tests, snapshot tests, property-based tests, and fixture-based tests.
14. Remain organized so future form support, risk-factor extraction, Forex implementation, and a future HTTP/API adapter can be added without major architectural rewrites.
