use anyhow::Result;
use app_core::{AppConfig, telemetry};
use app_workflow::{FetchExportRequest, SecFetchExportWorkflow};
use clap::{Parser, Subcommand};
use filing_discovery::FilingDiscoveryService;
use filing_models::{Cik, CompanyId, CompanyIdentity, Ticker};
use normalization::NormalizationResult;
use sec_client::{
    CompanyTickersResponse, RecentFilingLists, SecClient, SubmissionsResponse,
    ticker_lookup_records_from_response,
};
use std::path::PathBuf;
use tracing::info;
use valuation::ValuationEngine;
use workbook_io::WorkbookExporter;

#[derive(Debug, Parser)]
#[command(name = "sec-edgar-scraper")]
#[command(about = "Single-company SEC EDGAR extraction tool")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show configured workspace and SEC policy status.
    Status,
    /// Show the lookup target that would be used for ticker/CIK resolution.
    Resolve {
        #[arg(long)]
        ticker: Option<String>,
        #[arg(long)]
        cik: Option<String>,
    },
    /// Show the SEC submissions request for a CIK.
    Discover {
        #[arg(long)]
        cik: Option<String>,
        #[arg(long)]
        ticker: Option<String>,
        #[arg(long, default_value_t = 10)]
        years: u8,
    },
    /// Show extraction entry points for a CIK.
    Extract {
        #[arg(long)]
        cik: Option<String>,
        #[arg(long)]
        ticker: Option<String>,
    },
    /// Export a fixture-shaped workbook. Full live export will be wired after fixture downloads.
    Export {
        #[arg(long)]
        output: PathBuf,
    },
    /// Fetch SEC filings/data for a CIK and export an analyst workbook.
    FetchExport {
        #[arg(long)]
        cik: Option<String>,
        #[arg(long)]
        ticker: Option<String>,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value_t = 10)]
        years: u8,
        #[arg(long)]
        skip_html: bool,
    },
    /// Validate an existing workbook and report schema details.
    Import {
        #[arg(long)]
        input: PathBuf,
    },
    /// Show a concise pipeline summary.
    Summary,
    /// Show how provenance will be inspected for a metric.
    Provenance {
        #[arg(long)]
        metric: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    telemetry::init_tracing()?;

    let cli = Cli::parse();
    let config = AppConfig::from_env();
    let sec_client = SecClient::from_app_config(&config)?;

    match cli.command.unwrap_or(Command::Status) {
        Command::Status => status(&config),
        Command::Resolve { ticker, cik } => resolve(&sec_client, ticker, cik).await?,
        Command::Discover { cik, ticker, years } => {
            discover(&sec_client, cik, ticker, years).await?
        }
        Command::Extract { cik, ticker } => extract(&sec_client, cik, ticker).await?,
        Command::Export { output } => export_fixture_workbook(output)?,
        Command::FetchExport { cik, ticker, output, years, skip_html } => {
            fetch_export(&config, cik, ticker, output, years, skip_html).await?
        }
        Command::Import { input } => import_workbook(input)?,
        Command::Summary => summary(),
        Command::Provenance { metric } => provenance(metric),
    }

    Ok(())
}

async fn fetch_export(
    config: &AppConfig,
    cik: Option<String>,
    ticker: Option<String>,
    output: PathBuf,
    years: u8,
    skip_html: bool,
) -> Result<()> {
    let company_id = match (cik, ticker) {
        (Some(cik), _) => CompanyId::Cik(Cik::new(cik)),
        (None, Some(ticker)) => CompanyId::Ticker(Ticker::new(ticker)),
        (None, None) => {
            println!("Provide --cik or --ticker.");
            return Ok(());
        }
    };
    let workflow = SecFetchExportWorkflow::from_config(config)?;
    let summary = workflow
        .fetch_export_to_path(
            FetchExportRequest { company_id, years, include_html_fallback: !skip_html },
            &output,
        )
        .await?;

    println!("Exported analyst workbook: {}", output.display());
    println!("Company: {}", summary.company.issuer_name);
    println!("Selected filings: {}", summary.selected_filings.len());
    println!(
        "Coverage: earliest {:?}, latest {:?}, requested span covered: {}",
        summary.coverage.earliest_selected_year,
        summary.coverage.latest_selected_year,
        summary.coverage.has_requested_year_span
    );
    println!("XBRL metrics extracted: {}", summary.xbrl_metric_count);
    println!("HTML fallback metrics extracted: {}", summary.html_fallback_metric_count);
    println!("Narrative sections extracted: {}", summary.narrative_section_count);
    println!("Normalized metrics exported: {}", summary.normalized_metric_count);
    println!("Valuation outputs exported: {}", summary.valuation_output_count);
    println!("Review issues exported: {}", summary.review_issue_count);
    println!("Stage timings (ms):");
    println!("  resolve_cik: {}", summary.stage_timings_ms.resolve_cik_ms);
    println!("  discover_filings: {}", summary.stage_timings_ms.discover_filings_ms);
    println!("  fetch_company_facts: {}", summary.stage_timings_ms.fetch_company_facts_ms);
    println!("  extract_xbrl: {}", summary.stage_timings_ms.extract_xbrl_ms);
    println!("  extract_html: {}", summary.stage_timings_ms.extract_html_ms);
    println!("  normalize: {}", summary.stage_timings_ms.normalize_ms);
    println!("  valuation: {}", summary.stage_timings_ms.valuation_ms);
    println!("  workbook_export: {}", summary.stage_timings_ms.workbook_export_ms);
    println!("  total: {}", summary.stage_timings_ms.total_ms);
    if !summary.slowest_html_filings.is_empty() {
        println!("Slowest HTML filings (ms):");
        for timing in &summary.slowest_html_filings {
            println!(
                "  {} {} download={} extract={} total={}",
                timing.accession_number,
                timing.form_type,
                timing.download_ms,
                timing.extract_ms,
                timing.total_ms
            );
        }
    }

    Ok(())
}

fn status(config: &AppConfig) {
    info!(
        max_requests_per_second = config.sec.max_requests_per_second,
        "workspace and CLI orchestration are ready"
    );

    println!("Workspace is ready.");
    println!("SEC rate limit: {} requests/second", config.sec.max_requests_per_second);
    println!("Workbook export/import: implemented");
    println!(
        "Live CIK fetch/export: implemented with XBRL-first extraction and optional HTML fallback"
    );
}

async fn resolve(
    sec_client: &SecClient,
    ticker: Option<String>,
    cik: Option<String>,
) -> Result<()> {
    let company_id = match (ticker, cik) {
        (Some(ticker), _) => {
            let ticker = Ticker::new(ticker);
            let cik = resolve_ticker_to_cik(sec_client, &ticker).await?;
            println!("Resolved ticker {} to CIK {}", ticker.as_str(), cik.as_str());
            CompanyId::Cik(cik)
        }
        (None, Some(cik)) => CompanyId::Cik(Cik::new(cik)),
        (None, None) => {
            println!("Provide --ticker or --cik.");
            return Ok(());
        }
    };

    println!("Lookup target: {}", sec_client.describe_lookup_target(&company_id));
    println!("Ticker index request: {}", sec_client.company_tickers_request().url);
    Ok(())
}

async fn discover(
    sec_client: &SecClient,
    cik: Option<String>,
    ticker: Option<String>,
    years: u8,
) -> Result<()> {
    let cik = resolve_cik_input(sec_client, cik, ticker).await?;
    let request = sec_client.submissions_request(&cik);
    let discovery = FilingDiscoveryService::new(sec_client.endpoints().clone());

    println!("Discovery CIK: {}", cik.as_str());
    println!("History window: up to {years} years");
    println!("SEC submissions request: {}", request.url);
    println!("Filtering rule: original 10-K and 10-Q only; amendments excluded");

    let recent_response: SubmissionsResponse = sec_client.get_json(&request).await?;
    let mut plan =
        discovery.plan_filing_history_from_submissions(&cik, &recent_response, &[], years)?;

    let mut historical_filing_lists = Vec::new();
    if !plan.historical_files_to_fetch.is_empty() {
        println!(
            "Recent submissions did not cover the requested period; fetching {} historical submissions file(s).",
            plan.historical_files_to_fetch.len()
        );

        for file in &plan.historical_files_to_fetch {
            let historical_request = sec_client.submissions_file_request(&file.name);
            let historical_response: RecentFilingLists =
                sec_client.get_json(&historical_request).await?;
            historical_filing_lists.push(historical_response);
        }

        plan = discovery.plan_filing_history_from_submissions(
            &cik,
            &recent_response,
            &historical_filing_lists,
            years,
        )?;
    }

    println!("Selected filings: {}", plan.selected_filings.len());
    println!(
        "Coverage: earliest selected year {:?}, latest selected year {:?}, requested span covered: {}",
        plan.coverage.earliest_selected_year,
        plan.coverage.latest_selected_year,
        plan.coverage.has_requested_year_span
    );

    if !plan.historical_files_to_fetch.is_empty() {
        println!(
            "Historical files remain available for follow-up: {}",
            plan.historical_files_to_fetch.len()
        );
    }

    Ok(())
}

async fn extract(
    sec_client: &SecClient,
    cik: Option<String>,
    ticker: Option<String>,
) -> Result<()> {
    let cik = resolve_cik_input(sec_client, cik, ticker).await?;
    println!("XBRL companyfacts request: {}", sec_client.company_facts_request(&cik).url);
    println!("Extraction order: XBRL first, HTML fallback second, footnotes/MD&A from HTML");
    println!("Risk factors: skeleton only");
    Ok(())
}

async fn resolve_cik_input(
    sec_client: &SecClient,
    cik: Option<String>,
    ticker: Option<String>,
) -> Result<Cik> {
    match (cik, ticker) {
        (Some(cik), _) => Ok(Cik::new(cik)),
        (None, Some(ticker)) => resolve_ticker_to_cik(sec_client, &Ticker::new(ticker)).await,
        (None, None) => anyhow::bail!("provide --cik or --ticker"),
    }
}

async fn resolve_ticker_to_cik(sec_client: &SecClient, ticker: &Ticker) -> Result<Cik> {
    let response: CompanyTickersResponse =
        sec_client.get_json(&sec_client.company_tickers_request()).await?;
    let records = ticker_lookup_records_from_response(response);
    let discovery = FilingDiscoveryService::new(sec_client.endpoints().clone());
    Ok(discovery.resolve_cik_from_ticker(ticker, &records)?)
}

fn export_fixture_workbook(output: PathBuf) -> Result<()> {
    let exporter = WorkbookExporter::new();
    let normalized = NormalizationResult::default();
    let valuation_outputs =
        ValuationEngine::new().compute_placeholder_outputs(&normalized).unwrap_or_default();
    let model = exporter.build_model(sample_company(), &normalized, &valuation_outputs);

    exporter.export_to_path(&model, &output)?;
    println!("Exported fixture-shaped workbook: {}", output.display());
    Ok(())
}

fn import_workbook(input: PathBuf) -> Result<()> {
    let exporter = WorkbookExporter::new();
    let summary = exporter.import_summary(&input)?;

    println!("Workbook schema version: {}", summary.schema_version);
    println!("Worksheet count: {}", summary.sheet_names.len());
    Ok(())
}

fn summary() {
    println!("Pipeline:");
    println!("1. resolve ticker/CIK");
    println!("2. discover original 10-K/10-Q filings");
    println!("3. retrieve filing assets");
    println!("4. extract XBRL and HTML fallback values");
    println!("5. normalize domain-first records");
    println!("6. compute placeholder valuation outputs");
    println!("7. export/import versioned .xlsx workbook");
    println!("Command: fetch-export --cik 0000798354 --output company.xlsx --years 10");
    println!("Command: fetch-export --ticker AAPL --output company.xlsx --years 10");
}

fn provenance(metric: String) {
    println!("Provenance lookup target: {metric}");
    println!(
        "Tracked fields: accession, filing URL, source type, source method, label, XBRL tag, period, unit, scale"
    );
}

fn sample_company() -> CompanyIdentity {
    CompanyIdentity {
        primary_id: CompanyId::Cik(Cik::new("798354")),
        ticker: None,
        cik: Some(Cik::new("798354")),
        issuer_name: "Primary fixture company CIK 0000798354".to_string(),
        exchange: None,
        reported_currency: Some("USD".to_string()),
        fiscal_year_end: None,
    }
}
