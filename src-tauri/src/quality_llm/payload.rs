// The standardized per-candidate payload sent to the quality rubrics.
//
// THE GOVERNING INVARIANT: every candidate's payload has EXACTLY the same key set.
// A value the app doesn't have is an explicit `null` plus an entry in
// `data_quality.missing` — never an absent key.
//
// Why it matters: the model reads the key set as much as the values. If a ticker
// with no short-interest data simply lacked the field, the model would infer "this
// one is different" or, worse, quietly not look for it; with an explicit null AND a
// name in `missing`, absence is a stated fact it can lower `confidence` over. This
// also keeps the JSON prefix stable enough to be worth caching.
//
// `#[serde(skip_serializing_if)]` is therefore BANNED in this file. The test
// `payload_shape_is_identical_when_everything_is_missing` enforces it.
//
// FRESHNESS IS PART OF THE PAYLOAD. Unlike panic_llm, which reads a nightly cache,
// this pipeline fetches float / short interest / filings / financials live at
// qualification time and falls back to the DB when a source is slow or down.
// `data_quality.sources` records, field group by field group, which happened — so
// the model knows whether it is reading today's filing list or last week's.

use std::collections::BTreeMap;

use serde::Serialize;

/// Where one field group's value came from. Ordered worst-to-best is not meaningful
/// here; what matters is that the model can tell a fresh fact from a stale one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Fetched from the provider during this qualification.
    Live,
    /// Served from the local cache; may be days old.
    Db,
    /// Neither available. The DEFAULT on purpose: a group nobody filled in must read
    /// as absent, never as fresh.
    #[default]
    Missing,
}

#[derive(Debug, Clone, Serialize)]
pub struct Identity {
    pub symbol: String,
    pub name: Option<String>,
    pub exchange: Option<String>,
    pub country: Option<String>,
    /// Domicile with a high pump & dump base rate on US small caps.
    pub country_risk: bool,
    pub sector: Option<String>,
    pub industry: Option<String>,
    pub sic: Option<String>,
    /// Industry that structurally pulls a crowd (biotech, space, crypto…).
    pub sector_hot: bool,
    pub sector_bucket: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Trigger {
    /// The tempo that armed the watch (seconds).
    pub tempo_secs: i64,
    /// Every tempo that fired, ascending. Depth is signal: one 10s spike is not the
    /// same event as 10s + 20s + 40s agreeing.
    pub tempo_depth: Vec<i64>,
    pub price_return_pct: f64,
    pub volume_ratio: f64,
    /// Null on the Massive aggregate feed, which carries no trade count.
    pub trade_rate_ratio: Option<f64>,
    pub range_ratio: f64,
    pub detected_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Tape {
    /// `prints` (Alpaca) or `second_aggregates` (Massive).
    pub source: String,
    pub rate_per_sec: Option<f64>,
    pub dollar_per_sec: f64,
    pub active_seconds: Option<u32>,
    pub window_secs: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Price {
    pub last: f64,
    pub session_high: Option<f64>,
    pub session_low: Option<f64>,
    pub session_vwap: Option<f64>,
    pub distance_to_vwap_pct: Option<f64>,
    pub prev_close: Option<f64>,
    pub change_day_pct: Option<f64>,
    pub spread_pct: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Volume {
    pub session_volume: u64,
    pub session_dollar_volume: Option<f64>,
    pub rvol: Option<f64>,
    pub avg_daily_volume: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FloatAndShort {
    pub float_shares: Option<u64>,
    pub shares_outstanding: Option<u64>,
    pub free_float_pct: Option<f64>,
    pub market_cap: Option<f64>,
    pub short_interest: Option<f64>,
    pub short_interest_pct_float: Option<f64>,
    pub days_to_cover: Option<f64>,
    pub shortable: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NewsItem {
    pub published_at: String,
    /// Minutes between publication and now. Freshness is the single most load-bearing
    /// news field in the rubric.
    pub age_minutes: i64,
    /// Minutes between publication and the detected departure. NEGATIVE means the
    /// headline came FIRST (it can be the cause); positive means it came after (it is
    /// a report of the move, never its cause).
    pub minutes_vs_move: i64,
    pub headline: String,
    pub source: Option<String>,
    pub body_excerpt: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Risk {
    pub dilution_score: Option<f64>,
    pub dilution_capacity: Option<f64>,
    pub dilution_need: Option<f64>,
    pub dilution_pct_12m: Option<f64>,
    pub pump_dump_score: Option<f64>,
    pub has_recent_shelf: Option<bool>,
    pub latest_dilution_form: Option<String>,
    pub latest_dilution_date: Option<String>,
    pub atm_active: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Filing {
    pub form_type: Option<String>,
    pub date: Option<String>,
    pub category: Option<String>,
    pub atm: Option<bool>,
    pub resale: Option<bool>,
    pub warrants: Option<bool>,
    pub offering_amount: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Financials {
    pub net_income_last_q: Option<f64>,
    pub net_income_ttm: Option<f64>,
    pub negative_quarters_last4: Option<i64>,
    pub operating_cash_flow_ttm: Option<f64>,
    pub cash_and_equivalents: Option<f64>,
    pub period_end: Option<String>,
    /// Cash ÷ quarterly burn. The number that decides whether an ATM is a live threat
    /// or a dormant facility — derived here so the model doesn't have to divide.
    pub cash_runway_quarters: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Splits {
    pub last_split_date: Option<String>,
    pub split_label: Option<String>,
    pub days_since_split: Option<i64>,
    pub splits_last_1y: Option<i64>,
}

/// The field groups whose freshness is reported. Fixed and exhaustive: `sources`
/// always carries every one of these keys, so the payload's shape does not depend on
/// which providers happened to answer. Adding a group here is the only way to add one
/// to the payload.
pub const SOURCE_GROUPS: [&str; 5] = [
    "financials",
    "float_and_short",
    "news",
    "risk",
    "sec_filings",
];

#[derive(Debug, Clone, Serialize)]
pub struct DataQuality {
    /// `sip`, `iex`, `massive_delayed` or `inconnu`.
    pub feed: String,
    /// Per field group: `live`, `db` or `missing`. Always holds exactly
    /// `SOURCE_GROUPS` — see `DataQuality::new`. A BTreeMap so the key order is
    /// deterministic; a shuffled object would break the cacheable prefix.
    pub sources: BTreeMap<String, Source>,
    /// Field paths with no value at all. Absent, not zero.
    pub missing: Vec<String>,
}

impl DataQuality {
    /// Build with every group present. Groups the caller doesn't mention default to
    /// `Missing`, which is the honest reading: we asked for nothing and got nothing.
    ///
    /// Constructing the map directly would let a group silently disappear from one
    /// candidate's payload and not another's — the exact drift the shape invariant
    /// exists to prevent.
    pub fn new(feed: &str, known: &[(&str, Source)], missing: Vec<String>) -> Self {
        let mut sources: BTreeMap<String, Source> = SOURCE_GROUPS
            .iter()
            .map(|g| ((*g).to_string(), Source::Missing))
            .collect();
        for (group, source) in known {
            debug_assert!(
                SOURCE_GROUPS.contains(group),
                "unknown source group {group} — add it to SOURCE_GROUPS",
            );
            sources.insert((*group).to_string(), *source);
        }
        let mut missing = missing;
        missing.sort();
        missing.dedup();
        Self {
            feed: feed.to_string(),
            sources,
            missing,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Context {
    /// `live` or `replay`.
    pub mode: String,
    pub as_of: String,
    pub trading_day: String,
    /// `premarket`, `pre_open`, `open` or `afterhours`. Drives the rubric's
    /// open exception, where an overnight headline legitimately explains the move.
    pub session: String,
    /// Signed minutes to the 09:30 open; negative after it.
    pub minutes_to_open: i64,
}

/// One candidate, in the shape both rubrics read.
#[derive(Debug, Clone, Serialize)]
pub struct QualityPayload {
    pub identity: Identity,
    pub trigger: Trigger,
    pub tape: Tape,
    pub price: Price,
    pub volume: Volume,
    pub float_and_short: FloatAndShort,
    pub news: Vec<NewsItem>,
    pub risk: Risk,
    /// Deep pass only; always present, empty on the triage pass.
    pub sec_filings: Vec<Filing>,
    pub financials: Financials,
    pub splits: Splits,
    pub data_quality: DataQuality,
    pub context: Context,
}

impl QualityPayload {
    /// Compact JSON — the model doesn't need whitespace and we pay for it.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// The triage pass is the one the trader waits on, so it carries only what can be
    /// read in a couple of seconds: the filing list and the deep-only financial detail
    /// are dropped. The KEY SET is unchanged — `sec_filings` becomes an empty array
    /// and the deep-only financial fields become explicit nulls listed in `missing`,
    /// exactly as they would be for a ticker the SEC has nothing on.
    pub fn for_triage(&self) -> Self {
        let mut p = self.clone();
        p.sec_filings = Vec::new();
        p.financials = Financials {
            net_income_last_q: None,
            net_income_ttm: None,
            negative_quarters_last4: None,
            operating_cash_flow_ttm: None,
            cash_and_equivalents: None,
            period_end: None,
            cash_runway_quarters: None,
        };
        p.data_quality
            .sources
            .insert("sec_filings".into(), Source::Missing);
        p.data_quality
            .sources
            .insert("financials".into(), Source::Missing);
        for key in ["sec_filings", "financials"] {
            if !p.data_quality.missing.iter().any(|m| m == key) {
                p.data_quality.missing.push(key.to_string());
            }
        }
        p.data_quality.missing.sort();
        p
    }
}

/// Collect the JSON key paths present in a value, so a test can compare two payloads'
/// shapes rather than their contents.
#[cfg(test)]
pub fn key_paths(v: &serde_json::Value, prefix: &str, out: &mut Vec<String>) {
    match v {
        serde_json::Value::Object(map) => {
            for (k, val) in map {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                out.push(path.clone());
                key_paths(val, &path, out);
            }
        }
        // Arrays hold repeated homogeneous items; their inner shape is checked by
        // the item structs themselves, and their LENGTH legitimately varies.
        serde_json::Value::Array(_) => {}
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full() -> QualityPayload {
        QualityPayload {
            identity: Identity {
                symbol: "TEST".into(),
                name: Some("Test Biosciences".into()),
                exchange: Some("NASDAQ".into()),
                country: Some("U.S.A".into()),
                country_risk: false,
                sector: Some("Healthcare".into()),
                industry: Some("PHARMACEUTICAL PREPARATIONS".into()),
                sic: Some("2834".into()),
                sector_hot: true,
                sector_bucket: Some("biotech".into()),
            },
            trigger: Trigger {
                tempo_secs: 10,
                tempo_depth: vec![10, 20],
                price_return_pct: 1.8,
                volume_ratio: 12.0,
                trade_rate_ratio: Some(9.0),
                range_ratio: 5.0,
                detected_at: "2026-08-20T08:31:00Z".into(),
            },
            tape: Tape {
                source: "prints".into(),
                rate_per_sec: Some(6.0),
                dollar_per_sec: 8_000.0,
                active_seconds: None,
                window_secs: 5,
            },
            price: Price {
                last: 5.30,
                session_high: Some(5.35),
                session_low: Some(4.90),
                session_vwap: Some(5.05),
                distance_to_vwap_pct: Some(4.9),
                prev_close: Some(4.80),
                change_day_pct: Some(10.4),
                spread_pct: Some(0.4),
            },
            volume: Volume {
                session_volume: 900_000,
                session_dollar_volume: Some(4_500_000.0),
                rvol: Some(12.0),
                avg_daily_volume: Some(75_000.0),
            },
            float_and_short: FloatAndShort {
                float_shares: Some(4_000_000),
                shares_outstanding: Some(9_000_000),
                free_float_pct: Some(44.0),
                market_cap: Some(47_700_000.0),
                short_interest: Some(880_000.0),
                short_interest_pct_float: Some(22.0),
                days_to_cover: Some(3.1),
                shortable: Some(true),
            },
            news: vec![NewsItem {
                published_at: "2026-08-20T08:25:00Z".into(),
                age_minutes: 6,
                minutes_vs_move: -6,
                headline: "Positive phase 2 data".into(),
                source: Some("GlobeNewswire".into()),
                body_excerpt: Some("…".into()),
            }],
            risk: Risk {
                dilution_score: Some(30.0),
                dilution_capacity: Some(20.0),
                dilution_need: Some(15.0),
                dilution_pct_12m: Some(4.0),
                pump_dump_score: Some(10.0),
                has_recent_shelf: Some(true),
                latest_dilution_form: Some("S-3".into()),
                latest_dilution_date: Some("2025-02-11".into()),
                atm_active: Some(false),
            },
            sec_filings: vec![Filing {
                form_type: Some("S-3".into()),
                date: Some("2025-02-11".into()),
                category: Some("shelf".into()),
                atm: Some(false),
                resale: Some(false),
                warrants: Some(false),
                offering_amount: Some(50_000_000.0),
            }],
            financials: Financials {
                net_income_last_q: Some(-4_000_000.0),
                net_income_ttm: Some(-16_000_000.0),
                negative_quarters_last4: Some(4),
                operating_cash_flow_ttm: Some(-14_000_000.0),
                cash_and_equivalents: Some(21_000_000.0),
                period_end: Some("2026-06-30".into()),
                cash_runway_quarters: Some(6.0),
            },
            splits: Splits {
                last_split_date: None,
                split_label: None,
                days_since_split: None,
                splits_last_1y: Some(0),
            },
            data_quality: DataQuality::new(
                "sip",
                &[
                    ("float_and_short", Source::Live),
                    ("news", Source::Live),
                    ("risk", Source::Db),
                    ("sec_filings", Source::Live),
                    ("financials", Source::Live),
                ],
                vec!["splits.last_split_date".into()],
            ),
            context: Context {
                mode: "live".into(),
                as_of: "2026-08-20T08:31:00Z".into(),
                trading_day: "2026-08-20".into(),
                session: "premarket".into(),
                minutes_to_open: 59,
            },
        }
    }

    fn empty() -> QualityPayload {
        let f = full();
        QualityPayload {
            identity: Identity {
                symbol: "TEST".into(),
                name: None,
                exchange: None,
                country: None,
                country_risk: false,
                sector: None,
                industry: None,
                sic: None,
                sector_hot: false,
                sector_bucket: None,
            },
            price: Price {
                last: 5.30,
                session_high: None,
                session_low: None,
                session_vwap: None,
                distance_to_vwap_pct: None,
                prev_close: None,
                change_day_pct: None,
                spread_pct: None,
            },
            volume: Volume {
                session_volume: 0,
                session_dollar_volume: None,
                rvol: None,
                avg_daily_volume: None,
            },
            float_and_short: FloatAndShort {
                float_shares: None,
                shares_outstanding: None,
                free_float_pct: None,
                market_cap: None,
                short_interest: None,
                short_interest_pct_float: None,
                days_to_cover: None,
                shortable: None,
            },
            news: Vec::new(),
            risk: Risk {
                dilution_score: None,
                dilution_capacity: None,
                dilution_need: None,
                dilution_pct_12m: None,
                pump_dump_score: None,
                has_recent_shelf: None,
                latest_dilution_form: None,
                latest_dilution_date: None,
                atm_active: None,
            },
            sec_filings: Vec::new(),
            financials: Financials {
                net_income_last_q: None,
                net_income_ttm: None,
                negative_quarters_last4: None,
                operating_cash_flow_ttm: None,
                cash_and_equivalents: None,
                period_end: None,
                cash_runway_quarters: None,
            },
            splits: Splits {
                last_split_date: None,
                split_label: None,
                days_since_split: None,
                splits_last_1y: None,
            },
            tape: Tape {
                source: "second_aggregates".into(),
                rate_per_sec: None,
                dollar_per_sec: 4_000.0,
                active_seconds: Some(7),
                window_secs: 10,
            },
            // The aggregate feed carries no trade count, so the ratio is genuinely
            // absent here — the case the null-not-skipped test is about.
            trigger: Trigger {
                trade_rate_ratio: None,
                ..f.trigger.clone()
            },
            data_quality: DataQuality::new("massive_delayed", &[], Vec::new()),
            ..f
        }
    }

    fn shape(p: &QualityPayload) -> Vec<String> {
        let v: serde_json::Value = serde_json::from_str(&p.to_json()).unwrap();
        let mut out = Vec::new();
        key_paths(&v, "", &mut out);
        out.sort();
        out
    }

    /// THE invariant. A ticker the app knows nothing about must produce the same key
    /// set as a fully-populated one — every gap an explicit null, never a missing key.
    #[test]
    fn payload_shape_is_identical_when_everything_is_missing() {
        let rich = shape(&full());
        let bare = shape(&empty());
        assert_eq!(
            rich, bare,
            "a sparsely-known ticker must not present a different key set to the model",
        );
    }

    /// The rule above is only enforceable if nothing opts out of serialization.
    #[test]
    fn nothing_is_skipped_when_none() {
        let json = empty().to_json();
        for key in [
            "\"float_shares\":null",
            "\"short_interest\":null",
            "\"trade_rate_ratio\":null",
            "\"cash_runway_quarters\":null",
            "\"session_vwap\":null",
        ] {
            assert!(
                json.contains(key),
                "expected an explicit null: {key}\n{json}"
            );
        }
    }

    /// The triage pass drops content, never structure — otherwise the two passes
    /// would be reading two different documents.
    #[test]
    fn triage_narrows_the_content_but_keeps_the_shape() {
        let deep = full();
        let triage = deep.for_triage();
        assert_eq!(shape(&deep), shape(&triage));
        assert!(triage.sec_filings.is_empty());
        assert!(triage.financials.cash_runway_quarters.is_none());
        // …and says so, rather than letting the model read the absence as a fact.
        assert_eq!(
            triage.data_quality.sources.get("financials"),
            Some(&Source::Missing)
        );
        assert!(triage
            .data_quality
            .missing
            .iter()
            .any(|m| m == "sec_filings"));
    }

    /// The prefix has to be stable to be cacheable, so map iteration order can't be
    /// left to chance.
    #[test]
    fn source_map_order_is_deterministic() {
        let mut a = full();
        a.data_quality = DataQuality::new(
            "sip",
            &[
                ("risk", Source::Db),
                ("news", Source::Live),
                ("float_and_short", Source::Live),
            ],
            vec!["b".into(), "a".into()],
        );
        let mut b = full();
        b.data_quality = DataQuality::new(
            "sip",
            &[
                ("float_and_short", Source::Live),
                ("news", Source::Live),
                ("risk", Source::Db),
            ],
            vec!["a".into(), "b".into()],
        );
        assert_eq!(a.to_json(), b.to_json());
    }

    /// `minutes_vs_move` is the field the whole freshness rule hangs on, and its sign
    /// convention is the opposite of what a reader might assume. Pin it.
    #[test]
    fn a_headline_before_the_move_is_negative() {
        let p = full();
        let n = &p.news[0];
        assert!(
            n.minutes_vs_move < 0,
            "published before the departure ⇒ negative"
        );
    }
}
