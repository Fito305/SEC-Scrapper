//! Domain-first accounting models and canonical metric definitions.
//!
//! The registry in this crate is the main place where metric naming and XBRL tag variation should
//! live. Later extractor code should consult this registry instead of scattering tag mappings
//! across parsers.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum DomainName {
    CompanyOverview,
    FilingIndex,
    BalanceSheet,
    IncomeStatement,
    CashFlow,
    ShareholdersEquity,
    SegmentData,
    DebtAndCredit,
    DerivativesAndSecurities,
    EquityCompensation,
    Footnotes,
    Mda,
    Valuation,
    Provenance,
    RiskFactorsSkeleton,
    Schema,
}

impl DomainName {
    pub fn sheet_name(self) -> &'static str {
        match self {
            Self::CompanyOverview => "company_overview",
            Self::FilingIndex => "filing_index",
            Self::BalanceSheet => "balance_sheet",
            Self::IncomeStatement => "income_statement",
            Self::CashFlow => "cash_flow",
            Self::ShareholdersEquity => "shareholders_equity",
            Self::SegmentData => "segment_data",
            Self::DebtAndCredit => "debt_and_credit",
            Self::DerivativesAndSecurities => "derivatives_and_securities",
            Self::EquityCompensation => "equity_compensation",
            Self::Footnotes => "footnotes",
            Self::Mda => "mda",
            Self::Valuation => "valuation",
            Self::Provenance => "provenance",
            Self::RiskFactorsSkeleton => "risk_factors",
            Self::Schema => "schema",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StatementName {
    BalanceSheet,
    IncomeStatement,
    CashFlowStatement,
    ShareholdersEquityStatement,
    SegmentFootnote,
    DebtFootnote,
    DerivativeFootnote,
    EquityCompFootnote,
    Notes,
    Mda,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MetricId(pub String);

impl MetricId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricDefinition {
    pub metric_id: MetricId,
    pub display_name: String,
    pub domain: DomainName,
    pub statement: Option<StatementName>,
    pub preferred_xbrl_tags: Vec<String>,
    pub alternate_xbrl_tags: Vec<String>,
    pub expected_unit_hint: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainMetric {
    pub definition: MetricDefinition,
    pub subdomain: Option<String>,
    pub sort_order: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricRegistry {
    metrics: Vec<DomainMetric>,
}

impl MetricRegistry {
    pub fn new(metrics: Vec<DomainMetric>) -> Self {
        Self { metrics }
    }

    pub fn default() -> Self {
        Self::new(default_metric_registry())
    }

    pub fn all(&self) -> &[DomainMetric] {
        &self.metrics
    }

    pub fn by_domain(&self, domain: DomainName) -> Vec<&DomainMetric> {
        self.metrics.iter().filter(|metric| metric.definition.domain == domain).collect()
    }

    pub fn by_id(&self, metric_id: &str) -> Option<&DomainMetric> {
        self.metrics.iter().find(|metric| metric.definition.metric_id.as_str() == metric_id)
    }

    pub fn match_xbrl_tag(&self, tag: &str) -> Vec<&DomainMetric> {
        self.metrics
            .iter()
            .filter(|metric| {
                metric
                    .definition
                    .preferred_xbrl_tags
                    .iter()
                    .chain(metric.definition.alternate_xbrl_tags.iter())
                    .any(|candidate| candidate.eq_ignore_ascii_case(tag))
            })
            .collect()
    }
}

pub fn default_metric_registry() -> Vec<DomainMetric> {
    vec![
        metric(
            "balance_sheet.cash_and_equivalents",
            "Cash and Cash Equivalents",
            DomainName::BalanceSheet,
            Some(StatementName::BalanceSheet),
            Some("current_assets"),
            10,
            &["CashAndCashEquivalentsAtCarryingValue"],
            &[
                "CashCashEquivalentsRestrictedCashAndRestrictedCashEquivalents",
                "CashAndCashEquivalentsAtCarryingValueIncludingDiscontinuedOperations",
                "CashCashEquivalentsRestrictedCashAndRestrictedCashEquivalentsIncludingDisposalGroupAndDiscontinuedOperations",
            ],
            Some("USD"),
            Some("Core liquidity metric used widely across downstream formulas."),
        ),
        metric(
            "balance_sheet.accounts_receivable",
            "Accounts Receivable",
            DomainName::BalanceSheet,
            Some(StatementName::BalanceSheet),
            Some("current_assets"),
            20,
            &["AccountsReceivableNetCurrent"],
            &["ReceivablesNetCurrent"],
            Some("USD"),
            None,
        ),
        metric(
            "balance_sheet.inventory",
            "Inventory",
            DomainName::BalanceSheet,
            Some(StatementName::BalanceSheet),
            Some("current_assets"),
            30,
            &["InventoryNet"],
            &[],
            Some("USD"),
            None,
        ),
        metric(
            "balance_sheet.property_plant_equipment",
            "Property Plant and Equipment",
            DomainName::BalanceSheet,
            Some(StatementName::BalanceSheet),
            Some("non_current_assets"),
            40,
            &["PropertyPlantAndEquipmentNet"],
            &[],
            Some("USD"),
            None,
        ),
        metric(
            "balance_sheet.goodwill_and_intangibles",
            "Goodwill and Intangibles",
            DomainName::BalanceSheet,
            Some(StatementName::BalanceSheet),
            Some("non_current_assets"),
            50,
            &["FiniteLivedIntangibleAssetsNet", "Goodwill"],
            &["OtherThanGoodwillIntangibleAssetsNet"],
            Some("USD"),
            Some("This metric may normalize multiple related concepts later."),
        ),
        metric(
            "balance_sheet.total_assets",
            "Total Assets",
            DomainName::BalanceSheet,
            Some(StatementName::BalanceSheet),
            Some("totals"),
            60,
            &["Assets"],
            &[],
            Some("USD"),
            None,
        ),
        metric(
            "balance_sheet.current_assets",
            "Current Assets",
            DomainName::BalanceSheet,
            Some(StatementName::BalanceSheet),
            Some("current_assets"),
            65,
            &["AssetsCurrent"],
            &[],
            Some("USD"),
            None,
        ),
        metric(
            "balance_sheet.current_debt",
            "Current Debt",
            DomainName::BalanceSheet,
            Some(StatementName::BalanceSheet),
            Some("current_liabilities"),
            70,
            &["LongTermDebtCurrent"],
            &[
                "ShortTermBorrowings",
                "ShortTermBorrowingsCurrent",
                "LongTermDebtAndFinanceLeaseObligationsCurrent",
            ],
            Some("USD"),
            None,
        ),
        metric(
            "balance_sheet.long_term_debt",
            "Long-Term Debt",
            DomainName::BalanceSheet,
            Some(StatementName::BalanceSheet),
            Some("non_current_liabilities"),
            80,
            &["LongTermDebtNoncurrent"],
            &["LongTermDebt", "LongTermDebtAndFinanceLeaseObligationsNoncurrent"],
            Some("USD"),
            None,
        ),
        metric(
            "balance_sheet.total_liabilities",
            "Total Liabilities",
            DomainName::BalanceSheet,
            Some(StatementName::BalanceSheet),
            Some("totals"),
            90,
            &["Liabilities"],
            &[],
            Some("USD"),
            None,
        ),
        metric(
            "balance_sheet.current_liabilities",
            "Current Liabilities",
            DomainName::BalanceSheet,
            Some(StatementName::BalanceSheet),
            Some("current_liabilities"),
            95,
            &["LiabilitiesCurrent"],
            &[],
            Some("USD"),
            None,
        ),
        metric(
            "balance_sheet.accounts_payable",
            "Accounts Payable",
            DomainName::BalanceSheet,
            Some(StatementName::BalanceSheet),
            Some("current_liabilities"),
            97,
            &["AccountsPayableCurrent"],
            &["AccountsPayableAndAccruedLiabilitiesCurrent"],
            Some("USD"),
            None,
        ),
        metric(
            "balance_sheet.total_equity",
            "Total Equity",
            DomainName::BalanceSheet,
            Some(StatementName::BalanceSheet),
            Some("totals"),
            100,
            &["StockholdersEquity"],
            &["StockholdersEquityIncludingPortionAttributableToNoncontrollingInterest"],
            Some("USD"),
            None,
        ),
        metric(
            "income_statement.revenue",
            "Revenue",
            DomainName::IncomeStatement,
            Some(StatementName::IncomeStatement),
            Some("operating_results"),
            110,
            &["RevenueFromContractWithCustomerExcludingAssessedTax"],
            &["SalesRevenueNet", "Revenues", "SalesRevenueServicesNet"],
            Some("USD"),
            Some(
                "Revenue tag variation is common, so alternate tags belong here instead of parser code.",
            ),
        ),
        metric(
            "income_statement.cost_of_goods_sold",
            "Cost of Goods Sold",
            DomainName::IncomeStatement,
            Some(StatementName::IncomeStatement),
            Some("operating_results"),
            120,
            &["CostOfGoodsSold"],
            &["CostOfSales"],
            Some("USD"),
            None,
        ),
        metric(
            "income_statement.gross_profit",
            "Gross Profit",
            DomainName::IncomeStatement,
            Some(StatementName::IncomeStatement),
            Some("operating_results"),
            130,
            &["GrossProfit"],
            &[],
            Some("USD"),
            None,
        ),
        metric(
            "income_statement.operating_expenses",
            "Operating Expenses",
            DomainName::IncomeStatement,
            Some(StatementName::IncomeStatement),
            Some("operating_results"),
            140,
            &["OperatingExpenses"],
            &[],
            Some("USD"),
            None,
        ),
        metric(
            "income_statement.operating_income",
            "Operating Income",
            DomainName::IncomeStatement,
            Some(StatementName::IncomeStatement),
            Some("operating_results"),
            150,
            &["OperatingIncomeLoss"],
            &[],
            Some("USD"),
            None,
        ),
        metric(
            "income_statement.interest_expense",
            "Interest Expense",
            DomainName::IncomeStatement,
            Some(StatementName::IncomeStatement),
            Some("below_operating"),
            160,
            &["InterestExpense"],
            &[],
            Some("USD"),
            None,
        ),
        metric(
            "income_statement.tax_expense",
            "Income Tax Expense",
            DomainName::IncomeStatement,
            Some(StatementName::IncomeStatement),
            Some("below_operating"),
            170,
            &["IncomeTaxExpenseBenefit"],
            &[],
            Some("USD"),
            None,
        ),
        metric(
            "income_statement.net_income",
            "Net Income",
            DomainName::IncomeStatement,
            Some(StatementName::IncomeStatement),
            Some("totals"),
            180,
            &["NetIncomeLoss"],
            &[],
            Some("USD"),
            Some("This is a core input to placeholder valuation formulas."),
        ),
        metric(
            "income_statement.diluted_eps",
            "Diluted EPS",
            DomainName::IncomeStatement,
            Some(StatementName::IncomeStatement),
            Some("per_share"),
            190,
            &["EarningsPerShareDiluted"],
            &[],
            Some("ratio"),
            None,
        ),
        metric(
            "cash_flow.net_cash_from_operations",
            "Net Cash From Operating Activities",
            DomainName::CashFlow,
            Some(StatementName::CashFlowStatement),
            Some("operating_cash_flow"),
            200,
            &["NetCashProvidedByUsedInOperatingActivities"],
            &["NetCashProvidedByUsedInOperatingActivitiesContinuingOperations"],
            Some("USD"),
            None,
        ),
        metric(
            "cash_flow.depreciation_and_amortization",
            "Depreciation and Amortization",
            DomainName::CashFlow,
            Some(StatementName::CashFlowStatement),
            Some("operating_cash_flow"),
            210,
            &["DepreciationDepletionAndAmortization"],
            &["DepreciationAmortizationAndAccretionNet"],
            Some("USD"),
            Some("Direct placeholder valuation input."),
        ),
        metric(
            "cash_flow.capital_expenditures",
            "Capital Expenditures",
            DomainName::CashFlow,
            Some(StatementName::CashFlowStatement),
            Some("investing_cash_flow"),
            220,
            &["PaymentsToAcquirePropertyPlantAndEquipment"],
            &["CapitalExpendituresIncurredButNotYetPaid"],
            Some("USD"),
            Some("Direct placeholder valuation input."),
        ),
        metric(
            "cash_flow.net_cash_from_investing",
            "Net Cash From Investing Activities",
            DomainName::CashFlow,
            Some(StatementName::CashFlowStatement),
            Some("investing_cash_flow"),
            230,
            &["NetCashProvidedByUsedInInvestingActivities"],
            &[],
            Some("USD"),
            None,
        ),
        metric(
            "cash_flow.net_cash_from_financing",
            "Net Cash From Financing Activities",
            DomainName::CashFlow,
            Some(StatementName::CashFlowStatement),
            Some("financing_cash_flow"),
            240,
            &["NetCashProvidedByUsedInFinancingActivities"],
            &[],
            Some("USD"),
            None,
        ),
        metric(
            "cash_flow.stock_repurchases",
            "Stock Repurchases",
            DomainName::CashFlow,
            Some(StatementName::CashFlowStatement),
            Some("financing_cash_flow"),
            250,
            &["PaymentsForRepurchaseOfCommonStock"],
            &["PaymentsForRepurchaseOfEquity", "PaymentsForRepurchaseOfCommonStocks"],
            Some("USD"),
            Some("Feeds equity compensation and placeholder adjusted earnings logic."),
        ),
        metric(
            "cash_flow.share_issuance_proceeds",
            "Share Issuance Proceeds",
            DomainName::CashFlow,
            Some(StatementName::CashFlowStatement),
            Some("financing_cash_flow"),
            260,
            &["ProceedsFromStockOptionsExercised"],
            &["ProceedsFromIssuanceOfCommonStock"],
            Some("USD"),
            None,
        ),
        metric(
            "shareholders_equity.common_stock",
            "Common Stock",
            DomainName::ShareholdersEquity,
            Some(StatementName::ShareholdersEquityStatement),
            Some("capital_accounts"),
            270,
            &["CommonStocksIncludingAdditionalPaidInCapital"],
            &["CommonStockValue"],
            Some("USD"),
            None,
        ),
        metric(
            "shareholders_equity.additional_paid_in_capital",
            "Additional Paid In Capital",
            DomainName::ShareholdersEquity,
            Some(StatementName::ShareholdersEquityStatement),
            Some("capital_accounts"),
            280,
            &["AdditionalPaidInCapital"],
            &[],
            Some("USD"),
            None,
        ),
        metric(
            "shareholders_equity.retained_earnings",
            "Retained Earnings",
            DomainName::ShareholdersEquity,
            Some(StatementName::ShareholdersEquityStatement),
            Some("retained_earnings"),
            290,
            &["RetainedEarningsAccumulatedDeficit"],
            &[],
            Some("USD"),
            None,
        ),
        metric(
            "shareholders_equity.treasury_stock",
            "Treasury Stock",
            DomainName::ShareholdersEquity,
            Some(StatementName::ShareholdersEquityStatement),
            Some("capital_accounts"),
            300,
            &["TreasuryStockValue"],
            &[],
            Some("USD"),
            None,
        ),
        metric(
            "shareholders_equity.accumulated_oci",
            "Accumulated OCI",
            DomainName::ShareholdersEquity,
            Some(StatementName::ShareholdersEquityStatement),
            Some("capital_accounts"),
            310,
            &["AccumulatedOtherComprehensiveIncomeLossNetOfTax"],
            &[],
            Some("USD"),
            None,
        ),
        metric(
            "shareholders_equity.shares_outstanding",
            "Shares Outstanding",
            DomainName::ShareholdersEquity,
            Some(StatementName::ShareholdersEquityStatement),
            Some("share_counts"),
            320,
            &["CommonStocksIncludingAdditionalPaidInCapitalSharesOutstanding"],
            &["CommonStockSharesOutstanding", "EntityCommonStockSharesOutstanding"],
            Some("shares"),
            Some("Feeds equity compensation and placeholder adjusted earnings logic."),
        ),
        metric(
            "segment_data.segment_revenue",
            "Segment Revenue",
            DomainName::SegmentData,
            Some(StatementName::SegmentFootnote),
            Some("segment_results"),
            330,
            &["RevenueFromExternalCustomersByReportableSegment"],
            &["RevenuesFromExternalCustomers"],
            Some("USD"),
            None,
        ),
        metric(
            "segment_data.segment_profit_or_loss",
            "Segment Profit or Loss",
            DomainName::SegmentData,
            Some(StatementName::SegmentFootnote),
            Some("segment_results"),
            340,
            &["ProfitLossByReportableSegment"],
            &["OperatingIncomeLossBySegment"],
            Some("USD"),
            None,
        ),
        metric(
            "segment_data.segment_assets",
            "Segment Assets",
            DomainName::SegmentData,
            Some(StatementName::SegmentFootnote),
            Some("segment_assets"),
            350,
            &["AssetsByReportableSegment"],
            &[],
            Some("USD"),
            None,
        ),
        metric(
            "debt_and_credit.revolver_balance",
            "Revolver Balance",
            DomainName::DebtAndCredit,
            Some(StatementName::DebtFootnote),
            Some("credit_facilities"),
            360,
            &["LineOfCreditFacilityAmountOutstanding"],
            &["ShortTermBorrowings"],
            Some("USD"),
            None,
        ),
        metric(
            "debt_and_credit.term_loan_balance",
            "Term Loan Balance",
            DomainName::DebtAndCredit,
            Some(StatementName::DebtFootnote),
            Some("term_loans"),
            370,
            &["LongTermDebtAndCapitalLeaseObligations"],
            &["LongTermDebtNoncurrent"],
            Some("USD"),
            None,
        ),
        metric(
            "debt_and_credit.notes_and_bonds",
            "Notes and Bonds",
            DomainName::DebtAndCredit,
            Some(StatementName::DebtFootnote),
            Some("notes_and_bonds"),
            380,
            &["UnsecuredDebt"],
            &["LongTermDebtFairValue"],
            Some("USD"),
            None,
        ),
        metric(
            "debt_and_credit.debt_maturities",
            "Debt Maturities",
            DomainName::DebtAndCredit,
            Some(StatementName::DebtFootnote),
            Some("maturities"),
            390,
            &["LongTermDebtMaturitiesRepaymentsOfPrincipalInNextTwelveMonths"],
            &[],
            Some("USD"),
            None,
        ),
        metric(
            "debt_and_credit.interest_rate",
            "Debt Interest Rate",
            DomainName::DebtAndCredit,
            Some(StatementName::DebtFootnote),
            Some("rates"),
            400,
            &["InterestRate"],
            &[
                "LongtermDebtWeightedAverageInterestRate",
                "ShortTermDebtWeightedAverageInterestRate",
            ],
            Some("percentage"),
            Some("May require HTML extraction when not tagged consistently."),
        ),
        metric(
            "debt_and_credit.detail_senior_notes",
            "Senior Notes",
            DomainName::DebtAndCredit,
            Some(StatementName::DebtFootnote),
            Some("funding_detail"),
            410,
            &[],
            &[],
            Some("USD"),
            Some(
                "Detail debt category emitted from clearly labeled debt note tables. Aggregate debt formulas should continue using domain-level totals unless you intentionally switch to detail rows later.",
            ),
        ),
        metric(
            "debt_and_credit.detail_subordinated_debt",
            "Subordinated Debt",
            DomainName::DebtAndCredit,
            Some(StatementName::DebtFootnote),
            Some("funding_detail"),
            420,
            &[],
            &[],
            Some("USD"),
            Some(
                "Detail debt category emitted from clearly labeled debt note tables. Keep separate from senior debt unless you later choose to combine them in custom formulas.",
            ),
        ),
        metric(
            "debt_and_credit.detail_other_borrowed_funds",
            "Other Borrowed Funds",
            DomainName::DebtAndCredit,
            Some(StatementName::DebtFootnote),
            Some("funding_detail"),
            430,
            &[],
            &[],
            Some("USD"),
            Some(
                "Detail debt category for funding rows that are not clearly notes, subordinated debt, or secured borrowings. This is intended as analyst-visible support data, not an automatic aggregate input.",
            ),
        ),
        metric(
            "debt_and_credit.detail_secured_borrowings",
            "Secured Borrowings",
            DomainName::DebtAndCredit,
            Some(StatementName::DebtFootnote),
            Some("funding_detail"),
            440,
            &[],
            &[],
            Some("USD"),
            Some(
                "Detail debt category for collateralized funding rows such as asset-backed or other secured borrowings when the table context is explicit.",
            ),
        ),
        metric(
            "debt_and_credit.detail_structured_notes",
            "Structured Notes",
            DomainName::DebtAndCredit,
            Some(StatementName::DebtFootnote),
            Some("funding_detail"),
            450,
            &[],
            &[],
            Some("USD"),
            Some(
                "Detail debt category for structured notes or similar labeled funding instruments. Leave as a separate row so future formulas can opt in selectively.",
            ),
        ),
        metric(
            "debt_and_credit.detail_senior_notes_issuance",
            "Senior Notes Issuance",
            DomainName::DebtAndCredit,
            Some(StatementName::DebtFootnote),
            Some("funding_flow_detail"),
            460,
            &[],
            &[],
            Some("USD"),
            Some(
                "Flow-style funding detail from debt note tables. This represents issuance activity, not period-end balance.",
            ),
        ),
        metric(
            "debt_and_credit.detail_senior_notes_maturities",
            "Senior Notes Maturities",
            DomainName::DebtAndCredit,
            Some(StatementName::DebtFootnote),
            Some("funding_flow_detail"),
            470,
            &[],
            &[],
            Some("USD"),
            Some(
                "Flow-style funding detail from debt note tables. This represents maturities or redemptions, not period-end balance.",
            ),
        ),
        metric(
            "debt_and_credit.detail_subordinated_debt_issuance",
            "Subordinated Debt Issuance",
            DomainName::DebtAndCredit,
            Some(StatementName::DebtFootnote),
            Some("funding_flow_detail"),
            480,
            &[],
            &[],
            Some("USD"),
            Some("Flow-style subordinated debt issuance detail from debt note tables."),
        ),
        metric(
            "debt_and_credit.detail_subordinated_debt_maturities",
            "Subordinated Debt Maturities",
            DomainName::DebtAndCredit,
            Some(StatementName::DebtFootnote),
            Some("funding_flow_detail"),
            490,
            &[],
            &[],
            Some("USD"),
            Some(
                "Flow-style subordinated debt maturities or redemptions detail from debt note tables.",
            ),
        ),
        metric(
            "debt_and_credit.detail_other_borrowed_funds_issuance",
            "Other Borrowed Funds Issuance",
            DomainName::DebtAndCredit,
            Some(StatementName::DebtFootnote),
            Some("funding_flow_detail"),
            500,
            &[],
            &[],
            Some("USD"),
            Some("Flow-style issuance detail for other borrowed funds from debt note tables."),
        ),
        metric(
            "debt_and_credit.detail_other_borrowed_funds_maturities",
            "Other Borrowed Funds Maturities",
            DomainName::DebtAndCredit,
            Some(StatementName::DebtFootnote),
            Some("funding_flow_detail"),
            510,
            &[],
            &[],
            Some("USD"),
            Some(
                "Flow-style maturities or repayments detail for other borrowed funds from debt note tables.",
            ),
        ),
        metric(
            "debt_and_credit.detail_secured_borrowings_issuance",
            "Secured Borrowings Issuance",
            DomainName::DebtAndCredit,
            Some(StatementName::DebtFootnote),
            Some("funding_flow_detail"),
            520,
            &[],
            &[],
            Some("USD"),
            Some("Flow-style issuance detail for secured borrowings from debt note tables."),
        ),
        metric(
            "debt_and_credit.detail_secured_borrowings_maturities",
            "Secured Borrowings Maturities",
            DomainName::DebtAndCredit,
            Some(StatementName::DebtFootnote),
            Some("funding_flow_detail"),
            530,
            &[],
            &[],
            Some("USD"),
            Some(
                "Flow-style maturities or repayments detail for secured borrowings from debt note tables.",
            ),
        ),
        metric(
            "debt_and_credit.detail_structured_notes_issuance",
            "Structured Notes Issuance",
            DomainName::DebtAndCredit,
            Some(StatementName::DebtFootnote),
            Some("funding_flow_detail"),
            540,
            &[],
            &[],
            Some("USD"),
            Some("Flow-style issuance detail for structured notes from debt note tables."),
        ),
        metric(
            "debt_and_credit.detail_structured_notes_maturities",
            "Structured Notes Maturities",
            DomainName::DebtAndCredit,
            Some(StatementName::DebtFootnote),
            Some("funding_flow_detail"),
            550,
            &[],
            &[],
            Some("USD"),
            Some(
                "Flow-style maturities or redemptions detail for structured notes from debt note tables.",
            ),
        ),
        metric(
            "derivatives_and_securities.derivative_fair_value",
            "Derivative Fair Value",
            DomainName::DerivativesAndSecurities,
            Some(StatementName::DerivativeFootnote),
            Some("derivatives"),
            410,
            &["DerivativeAssets"],
            &[
                "DerivativeLiabilities",
                "DerivativeAssetsCurrent",
                "DerivativeAssetsNoncurrent",
                "DerivativeLiabilitiesCurrent",
                "DerivativeLiabilitiesNoncurrent",
            ],
            Some("USD"),
            None,
        ),
        metric(
            "derivatives_and_securities.derivative_gain_loss",
            "Derivative Gain or Loss",
            DomainName::DerivativesAndSecurities,
            Some(StatementName::DerivativeFootnote),
            Some("derivatives"),
            420,
            &["DerivativeGainLossRecognizedInIncome"],
            &[
                "DerivativeGainLossOnDerivativeNet",
                "DerivativeInstrumentsNotDesignatedAsHedgingInstrumentsGainLossNet",
            ],
            Some("USD"),
            None,
        ),
        metric(
            "derivatives_and_securities.debt_securities_value",
            "Debt Securities Value",
            DomainName::DerivativesAndSecurities,
            Some(StatementName::DerivativeFootnote),
            Some("debt_securities"),
            430,
            &["AvailableForSaleDebtSecurities"],
            &[
                "DebtSecuritiesAvailableForSaleAmortizedCost",
                "DebtSecuritiesAvailableForSaleExcludingAccruedInterest",
                "DebtSecuritiesAvailableForSaleExcludingAccruedInterestCurrent",
                "DebtSecuritiesAvailableForSaleExcludingAccruedInterestNoncurrent",
            ],
            Some("USD"),
            None,
        ),
        metric(
            "equity_compensation.stock_based_comp_expense",
            "Stock-Based Compensation Expense",
            DomainName::EquityCompensation,
            Some(StatementName::EquityCompFootnote),
            Some("expense"),
            440,
            &["ShareBasedCompensation"],
            &["AllocatedShareBasedCompensationExpense"],
            Some("USD"),
            Some("Direct placeholder adjusted earnings input."),
        ),
        metric(
            "equity_compensation.rsu_activity",
            "RSU Activity",
            DomainName::EquityCompensation,
            Some(StatementName::EquityCompFootnote),
            Some("rsu"),
            450,
            &[
                "ShareBasedCompensationArrangementByShareBasedPaymentAwardNumberOfSharesAvailableForGrant",
            ],
            &[
                "ShareBasedCompensationArrangementByShareBasedPaymentAwardEquityInstrumentsOtherThanOptionsGrantsInPeriodTotal",
                "ShareBasedCompensationArrangementByShareBasedPaymentAwardEquityInstrumentsOtherThanOptionsVestedInPeriodTotal",
            ],
            Some("shares"),
            None,
        ),
        metric(
            "equity_compensation.option_activity",
            "Option Activity",
            DomainName::EquityCompensation,
            Some(StatementName::EquityCompFootnote),
            Some("options"),
            460,
            &[
                "EmployeeServiceShareBasedCompensationNonvestedAwardsTotalCompensationCostNotYetRecognized",
            ],
            &[
                "ShareBasedCompensationArrangementByShareBasedPaymentAwardOptionsExercisableWeightedAverageExercisePrice",
            ],
            Some("USD"),
            None,
        ),
        metric(
            "equity_compensation.tax_effects",
            "Stock Compensation Tax Effects",
            DomainName::EquityCompensation,
            Some(StatementName::EquityCompFootnote),
            Some("tax_effects"),
            470,
            &["ShareBasedCompensationArrangementByShareBasedPaymentAwardTaxBenefitRealized"],
            &["EmployeeServiceShareBasedCompensationTaxBenefitFromCompensationExpense"],
            Some("USD"),
            Some("Direct placeholder adjusted earnings input."),
        ),
        metric(
            "equity_compensation.shares_repurchased",
            "Shares Repurchased",
            DomainName::EquityCompensation,
            Some(StatementName::EquityCompFootnote),
            Some("share_counts"),
            480,
            &["PaymentsForRepurchaseOfCommonStockShares"],
            &[
                "TreasuryStockSharesAcquired",
                "StockRepurchasedDuringPeriodSharesSplitOffTransaction",
            ],
            Some("shares"),
            Some("Used in the placeholder adjusted earnings ratio path."),
        ),
        metric(
            "equity_compensation.net_change_shares_outstanding",
            "Net Change in Shares Outstanding",
            DomainName::EquityCompensation,
            Some(StatementName::EquityCompFootnote),
            Some("share_counts"),
            490,
            &["IncreaseDecreaseInCommonSharesOutstanding"],
            &[],
            Some("shares"),
            Some("Used in the placeholder adjusted earnings ratio path."),
        ),
        metric(
            "footnotes.disclosure_text",
            "Footnote Disclosure Text",
            DomainName::Footnotes,
            Some(StatementName::Notes),
            Some("narrative"),
            500,
            &[],
            &[],
            Some("text"),
            Some(
                "Narrative extraction will map text blocks to this domain rather than a numeric tag.",
            ),
        ),
        metric(
            "mda.management_discussion_text",
            "MD&A Text",
            DomainName::Mda,
            Some(StatementName::Mda),
            Some("narrative"),
            510,
            &[],
            &[],
            Some("text"),
            Some("Narrative extraction entry point for management discussion and analysis."),
        ),
        metric(
            "valuation.owners_earnings_placeholder",
            "Owner's Earnings Placeholder",
            DomainName::Valuation,
            None,
            Some("formula_output"),
            520,
            &[],
            &[],
            Some("USD"),
            Some(
                "This metric is produced by the valuation module, not extracted directly from SEC content.",
            ),
        ),
        metric(
            "valuation.adjusted_earnings_ratio_placeholder",
            "Adjusted Earnings Ratio Placeholder",
            DomainName::Valuation,
            None,
            Some("formula_output"),
            530,
            &[],
            &[],
            Some("ratio"),
            Some(
                "This metric is produced by the valuation module, not extracted directly from SEC content.",
            ),
        ),
        metric(
            "risk_factors.placeholder",
            "Risk Factors Placeholder",
            DomainName::RiskFactorsSkeleton,
            None,
            Some("future_extension"),
            540,
            &[],
            &[],
            Some("text"),
            Some("V1 keeps this as a skeleton only. The registry still reserves the ID now."),
        ),
    ]
}

fn metric(
    metric_id: &str,
    display_name: &str,
    domain: DomainName,
    statement: Option<StatementName>,
    subdomain: Option<&str>,
    sort_order: u32,
    preferred_xbrl_tags: &[&str],
    alternate_xbrl_tags: &[&str],
    expected_unit_hint: Option<&str>,
    notes: Option<&str>,
) -> DomainMetric {
    DomainMetric {
        definition: MetricDefinition {
            metric_id: MetricId::new(metric_id),
            display_name: display_name.to_string(),
            domain,
            statement,
            preferred_xbrl_tags: preferred_xbrl_tags
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            alternate_xbrl_tags: alternate_xbrl_tags
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            expected_unit_hint: expected_unit_hint.map(str::to_string),
            notes: notes.map(str::to_string),
        },
        subdomain: subdomain.map(str::to_string),
        sort_order,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_covers_required_domains() {
        let registry = MetricRegistry::default();

        assert!(!registry.by_domain(DomainName::BalanceSheet).is_empty());
        assert!(!registry.by_domain(DomainName::IncomeStatement).is_empty());
        assert!(!registry.by_domain(DomainName::CashFlow).is_empty());
        assert!(!registry.by_domain(DomainName::ShareholdersEquity).is_empty());
        assert!(!registry.by_domain(DomainName::SegmentData).is_empty());
        assert!(!registry.by_domain(DomainName::DebtAndCredit).is_empty());
        assert!(!registry.by_domain(DomainName::DerivativesAndSecurities).is_empty());
        assert!(!registry.by_domain(DomainName::EquityCompensation).is_empty());
        assert!(!registry.by_domain(DomainName::Footnotes).is_empty());
        assert!(!registry.by_domain(DomainName::Mda).is_empty());
        assert!(!registry.by_domain(DomainName::Valuation).is_empty());
    }

    #[test]
    fn registry_can_lookup_metric_by_id() {
        let registry = MetricRegistry::default();
        let metric = registry.by_id("income_statement.net_income").expect("metric should exist");

        assert_eq!(metric.definition.display_name, "Net Income");
        assert_eq!(metric.definition.domain, DomainName::IncomeStatement);
    }

    #[test]
    fn registry_can_match_preferred_and_alternate_tags() {
        let registry = MetricRegistry::default();

        let preferred = registry.match_xbrl_tag("CashAndCashEquivalentsAtCarryingValue");
        let alternate = registry.match_xbrl_tag("SalesRevenueNet");

        assert_eq!(preferred.len(), 1);
        assert_eq!(
            preferred[0].definition.metric_id.as_str(),
            "balance_sheet.cash_and_equivalents"
        );
        assert_eq!(alternate.len(), 1);
        assert_eq!(alternate[0].definition.metric_id.as_str(), "income_statement.revenue");
    }

    #[test]
    fn registry_matches_common_sec_companyfacts_tag_variants() {
        let registry = MetricRegistry::default();
        let expected_matches = [
            ("AssetsCurrent", "balance_sheet.current_assets"),
            (
                "CashAndCashEquivalentsAtCarryingValueIncludingDiscontinuedOperations",
                "balance_sheet.cash_and_equivalents",
            ),
            (
                "CashCashEquivalentsRestrictedCashAndRestrictedCashEquivalentsIncludingDisposalGroupAndDiscontinuedOperations",
                "balance_sheet.cash_and_equivalents",
            ),
            ("LiabilitiesCurrent", "balance_sheet.current_liabilities"),
            ("AccountsPayableCurrent", "balance_sheet.accounts_payable"),
            ("Revenues", "income_statement.revenue"),
            ("SalesRevenueServicesNet", "income_statement.revenue"),
            ("OperatingIncomeLoss", "income_statement.operating_income"),
            (
                "NetCashProvidedByUsedInOperatingActivitiesContinuingOperations",
                "cash_flow.net_cash_from_operations",
            ),
            ("PaymentsForRepurchaseOfCommonStocks", "cash_flow.stock_repurchases"),
            ("LongtermDebtWeightedAverageInterestRate", "debt_and_credit.interest_rate"),
            ("ShortTermDebtWeightedAverageInterestRate", "debt_and_credit.interest_rate"),
            (
                "ShareBasedCompensationArrangementByShareBasedPaymentAwardEquityInstrumentsOtherThanOptionsGrantsInPeriodTotal",
                "equity_compensation.rsu_activity",
            ),
            ("EntityCommonStockSharesOutstanding", "shareholders_equity.shares_outstanding"),
        ];

        for (xbrl_tag, metric_id) in expected_matches {
            assert!(
                registry.match_xbrl_tag(xbrl_tag).iter().any(|metric| metric
                    .definition
                    .metric_id
                    .as_str()
                    == metric_id),
                "{xbrl_tag} should map to {metric_id}"
            );
        }
    }

    #[test]
    fn registry_matches_domain_specific_sec_tag_variants() {
        let registry = MetricRegistry::default();
        let expected_matches = [
            ("LongTermDebtAndFinanceLeaseObligationsCurrent", "balance_sheet.current_debt"),
            ("LongTermDebtAndFinanceLeaseObligationsNoncurrent", "balance_sheet.long_term_debt"),
            ("RevenuesFromExternalCustomers", "segment_data.segment_revenue"),
            ("OperatingIncomeLossBySegment", "segment_data.segment_profit_or_loss"),
            ("DerivativeAssetsCurrent", "derivatives_and_securities.derivative_fair_value"),
            (
                "DerivativeGainLossOnDerivativeNet",
                "derivatives_and_securities.derivative_gain_loss",
            ),
            (
                "DebtSecuritiesAvailableForSaleExcludingAccruedInterestCurrent",
                "derivatives_and_securities.debt_securities_value",
            ),
            ("TreasuryStockSharesAcquired", "equity_compensation.shares_repurchased"),
            (
                "EmployeeServiceShareBasedCompensationTaxBenefitFromCompensationExpense",
                "equity_compensation.tax_effects",
            ),
        ];

        for (xbrl_tag, metric_id) in expected_matches {
            assert!(
                registry.match_xbrl_tag(xbrl_tag).iter().any(|metric| metric
                    .definition
                    .metric_id
                    .as_str()
                    == metric_id),
                "{xbrl_tag} should map to {metric_id}"
            );
        }
    }
}
