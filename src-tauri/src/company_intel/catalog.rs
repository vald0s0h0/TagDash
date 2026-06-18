// The `company_intel` catalog: a flat, self-describing list of every datum the
// collection job captures — its stable key, a human label, the section it belongs
// to, its priority data source and its value type. This is the single place the
// UI (or any consumer) reads to know what exists and where it comes from, so field
// names never get hard-coded in two places.
//
// `key` matches the JSON field path the UI receives from `get_company_intel`
// (section.field), e.g. "short_interest.days_to_cover".

use serde::Serialize;

/// Value type hint for the UI (formatting / units).
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntelType {
    /// Whole share / filing count.
    Shares,
    /// USD currency amount.
    Usd,
    /// Ratio / count of days.
    Number,
    /// 0–100 percentage.
    Percent,
    /// Calendar date.
    Date,
    /// True/false flag.
    Bool,
    /// Free text / identifier.
    Text,
    /// A structured list (rendered specially).
    List,
}

/// One catalog entry.
#[derive(Debug, Clone, Serialize)]
pub struct IntelField {
    /// Stable machine key (`section.field`).
    pub key: &'static str,
    /// Human-readable label.
    pub label: &'static str,
    /// Logical section (matches the data model sections).
    pub section: &'static str,
    /// Priority data source for this datum.
    pub source: &'static str,
    pub kind: IntelType,
}

use IntelType::*;

/// The full catalog. Ordered by section, then by display priority within a
/// section.
pub fn catalog() -> &'static [IntelField] {
    &[
        // ── Short interest (Massive) ──────────────────────────────────────────
        IntelField { key: "short_interest.short_interest",  label: "Short interest",       section: "short_interest", source: "Massive", kind: Shares },
        IntelField { key: "short_interest.days_to_cover",   label: "Days to cover",        section: "short_interest", source: "Massive", kind: Number },
        IntelField { key: "short_interest.settlement_date", label: "Settlement date",      section: "short_interest", source: "Massive", kind: Date },

        // ── Financial health (SEC Company Facts → FMP) ────────────────────────
        IntelField { key: "financials.net_income_last_q",        label: "Net income (last Q)",  section: "financials", source: "SEC Company Facts", kind: Usd },
        IntelField { key: "financials.net_income_ttm",           label: "Net income (TTM)",     section: "financials", source: "SEC Company Facts", kind: Usd },
        IntelField { key: "financials.negative_quarters_last4",  label: "Negative quarters /4", section: "financials", source: "SEC Company Facts", kind: Number },
        IntelField { key: "financials.operating_cash_flow_ttm",  label: "Operating cash flow (TTM)", section: "financials", source: "SEC Company Facts", kind: Usd },
        IntelField { key: "financials.cash_and_equivalents",     label: "Cash & equivalents",   section: "financials", source: "SEC Company Facts", kind: Usd },
        IntelField { key: "financials.period_end",               label: "Reporting period end", section: "financials", source: "SEC Company Facts", kind: Date },

        // ── Dilution / S-3 (SEC EDGAR) ────────────────────────────────────────
        IntelField { key: "dilution.has_recent_shelf",       label: "Recent shelf (S-3)",   section: "dilution", source: "SEC EDGAR", kind: Bool },
        IntelField { key: "dilution.latest_form",            label: "Latest dilution form", section: "dilution", source: "SEC EDGAR", kind: Text },
        IntelField { key: "dilution.latest_date",            label: "Latest dilution date", section: "dilution", source: "SEC EDGAR", kind: Date },
        IntelField { key: "dilution.flags.atm",              label: "ATM offering",         section: "dilution", source: "SEC EDGAR", kind: Bool },
        IntelField { key: "dilution.flags.resale",           label: "Resale registration",  section: "dilution", source: "SEC EDGAR", kind: Bool },
        IntelField { key: "dilution.flags.warrants",         label: "Warrants",             section: "dilution", source: "SEC EDGAR", kind: Bool },
        IntelField { key: "dilution.flags.offering_amount",  label: "Offering amount",      section: "dilution", source: "SEC EDGAR", kind: Usd },

        // ── Ownership / locked shares (SEC 13D/13G → FMP) ─────────────────────
        IntelField { key: "ownership.institutional_ownership_pct", label: "Institutional ownership", section: "ownership", source: "FMP", kind: Percent },
        IntelField { key: "ownership.insider_ownership_pct",       label: "Insider ownership",       section: "ownership", source: "FMP", kind: Percent },
        IntelField { key: "ownership.holders_5pct",                label: ">5% holders (13D/13G)",   section: "ownership", source: "SEC EDGAR", kind: List },
        IntelField { key: "ownership.restricted_shares",           label: "Restricted / locked shares", section: "ownership", source: "SEC EDGAR", kind: Shares },
    ]
}
