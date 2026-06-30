// Alert enrichment pipeline. Triggered when an alert is shown in a zone (see
// the `start_alert_enrichment` command). Runs as a single async task that fills
// an AlertEnrichment record step by step, writing a partial result to the shared
// store after each step so the info band fills in progressively without ever
// blocking the UI. Nothing here is on the hot path.

pub mod classify;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use chrono::{DateTime, NaiveDate, TimeZone, Utc};

use crate::alpaca::bars::BarData;
use crate::config::secrets::Secrets;
use crate::local_db::{cache_repository, company_meta_repository};
use crate::market_state::aggregators::Bar;
use crate::market_state::MarketState;
use crate::types::{AlertEnrichment, SplitMarker};

/// Daily bars fetched for the left pane — enough history for a 200-day SMA.
const DAILY_FETCH_DAYS: u32 = 250;

const SYSTEM_FR: &str =
    "Tu es un assistant de trading intraday small-cap. Réponds en français, de façon très concise (une phrase maximum).";

/// System prompt for the panic mean-reversion read (allowed two short lines).
const SYSTEM_PANIC_FR: &str =
    "Tu es un assistant de trading intraday spécialisé en mean-reversion sur small-caps. \
     Tu juges si un mouvement de prix est solide (justifié par une nouvelle de fond) ou \
     artificiel/bluff (donc forte probabilité de retour à l'équilibre). Réponds en français, \
     très concis, exactement deux lignes au format demandé.";

/// Shared enrichment store, keyed by symbol. Lives in AppState.
pub type Store = Arc<RwLock<HashMap<String, AlertEnrichment>>>;

/// True when enrichment for `symbol` already exists or is in flight (idempotency).
pub fn is_present(store: &Store, symbol: &str) -> bool {
    store.read().unwrap().contains_key(symbol)
}

/// Run the full enrichment pipeline for one symbol. Spawn this on the async
/// runtime; it returns when every step has completed (or been skipped).
pub async fn run(
    symbol: String,
    strategy_id: String,
    db: Arc<Mutex<rusqlite::Connection>>,
    secrets: Arc<RwLock<Secrets>>,
    market: Arc<RwLock<MarketState>>,
    store: Store,
) {
    // Seed a loading record so the UI can show spinners immediately.
    update(&store, &symbol, |e| {
        e.status = "loading".into();
        e.strategy_id = strategy_id.clone();
    });

    // Hydrate the last persisted LLM result so a re-opened zone shows the previous
    // read immediately, without re-calling the model. A fresh button click
    // overwrites it (and appends a new history row). Both panic (context/verdict)
    // and micro_pullback (dilution/news) results are appended to the same table;
    // map them back to the fields each strategy's card actually reads.
    if strategy_id == crate::strategies::panic_mean_reversion::ID {
        let latest = {
            let conn = db.lock().unwrap();
            crate::local_db::llm_repository::get_latest(&conn, &symbol).ok().flatten()
        };
        if let Some(a) = latest {
            update(&store, &symbol, |e| {
                e.llm_context = a.context;
                e.llm_reversion = a.verdict;
            });
        }
    } else if strategy_id == crate::strategies::micro_pullback::ID {
        let latest = {
            let conn = db.lock().unwrap();
            crate::local_db::llm_repository::get_latest(&conn, &symbol).ok().flatten()
        };
        if let Some(a) = latest {
            update(&store, &symbol, |e| {
                e.llm_dilution = a.context;
                e.llm_news = a.verdict;
            });
        }
    }

    // Snapshot the secrets we need (don't hold the lock across awaits). The
    // Deepseek key isn't needed here — both strategies' LLM reads are
    // user-triggered (see `run_panic_llm` / `run_micro_pullback_llm`).
    let (alpaca_key, alpaca_secret) = {
        let s = secrets.read().unwrap();
        (s.alpaca_key.clone(), s.alpaca_secret.clone())
    };

    // ── Step 1: immediate, from the local DB (country / industry / float) ──
    {
        let conn = db.lock().unwrap();
        let meta = company_meta_repository::get_by_symbol(&conn, &symbol).ok().flatten();
        let fund = cache_repository::get_fundamental(&conn, &symbol).ok().flatten();
        drop(conn);
        update(&store, &symbol, |e| {
            if let Some(m) = &meta {
                e.country = m.country.clone();
                e.industry = m.industry.clone();
                e.country_flagged = m.country.as_deref().map(is_flagged_country).unwrap_or(false);
            }
            if let Some(f) = &fund {
                e.float_shares = f.float_shares.map(|v| v as f64);
            }
        });
    }

    // ── Step 2: daily history fetch + price-action classification ──
    let mut daily_bars: Vec<Bar> = Vec::new();
    if let (Some(k), Some(sec)) = (&alpaca_key, &alpaca_secret) {
        if let Ok(map) =
            crate::alpaca::bars::fetch_daily_bars(k, sec, &[symbol.clone()], DAILY_FETCH_DAYS).await
        {
            if let Some(bd) = map.get(&symbol) {
                daily_bars = bd.iter().filter_map(bardata_to_bar).collect();
            }
        }
    }
    let classification = classify::classify(&daily_bars);
    update(&store, &symbol, |e| {
        e.classification = classification;
        e.daily_bars = daily_bars.clone();
        e.daily_done = true;
    });

    // ── Step 3: historical splits (Alpaca corporate-actions, last 2 years) ──
    // One red marker per split day on the daily pane; the info band shows the most
    // recent split's label + age. Alpaca serves real ex-dates (the Massive endpoint
    // was a guessed shape that returned nothing), and 2y covers what's chart-visible.
    if let (Some(k), Some(sec)) = (&alpaca_key, &alpaca_secret) {
        if let Ok(splits) = crate::alpaca::corporate_actions::fetch_splits(k, sec, &symbol, 2).await {
            let markers: Vec<SplitMarker> = splits
                .iter()
                .filter_map(|s| {
                    parse_date(&s.date).map(|d| SplitMarker {
                        time:  d.timestamp(),
                        label: s.label.clone(),
                    })
                })
                .collect();
            // fetch_splits returns newest first, so splits[0] is the most recent.
            let latest = splits.first().cloned();
            update(&store, &symbol, |e| {
                if let Some(s) = &latest {
                    e.split_label = Some(s.label.clone());
                    e.days_since_split = days_since(&s.date);
                }
                if !markers.is_empty() {
                    e.split_markers = markers.clone();
                }
            });
        }
    }

    // ── Step 4: most recent news, read from the live Alpaca news feed (RAM) ──
    // micro_pullback correlates against the Alpaca news WebSocket; Massive has no
    // news, so reading the same RAM store is both correct and instant (no API
    // call, no "no news" false negative when Alpaca did carry a headline).
    let news_item = market.read().unwrap().latest_news(&symbol);
    update(&store, &symbol, |e| {
        if let Some(n) = &news_item {
            e.news_title = Some(n.headline.clone());
            e.news_url = n.url.clone();
        }
        e.news_checked = true;
    });

    // Neither strategy auto-runs the LLM step — both reads are user-triggered via
    // the info-bar button (see `run_panic_llm` / `run_micro_pullback_llm` + the
    // `run_alert_llm` command).
    update(&store, &symbol, |e| e.status = "done".into());
}

/// Days of news + daily OHLC to gather for the panic read.
const PANIC_NEWS_DAYS: i64 = 5;
/// Cap the number of articles + the per-article body length fed to the model.
const PANIC_NEWS_LIMIT: u32 = 10;
const PANIC_CONTENT_CHARS: usize = 900;

/// On-demand panic mean-reversion LLM read (button-triggered, never automatic).
/// On click we (1) call the Alpaca news REST API for the ticker's articles over
/// the last 5 days (with full content), (2) read the last 5 daily OHLC bars, then
/// (3) hand both to Deepseek, which returns two short French lines — a context
/// summary (why the stock is moving / which news pushed it) and a mean-reversion
/// verdict (solid vs bluff + probability of a return to equilibrium). The model is
/// told that some of the articles are noise and to use only the relevant ones.
pub async fn run_panic_llm(
    symbol: String,
    db: Arc<Mutex<rusqlite::Connection>>,
    secrets: Arc<RwLock<Secrets>>,
    store: Store,
) {
    let (alpaca_key, alpaca_secret, deepseek_key) = {
        let s = secrets.read().unwrap();
        (s.alpaca_key.clone(), s.alpaca_secret.clone(), s.deepseek_api_key.clone())
    };
    let Some(dk) = deepseek_key else { return };

    update(&store, &symbol, |e| e.llm_pending = true);

    // ── 1. Alpaca news (last 5 days, with content) ──
    let mut articles = Vec::new();
    if let (Some(k), Some(sec)) = (&alpaca_key, &alpaca_secret) {
        match crate::alpaca::news::fetch_recent_news(
            k, sec, &symbol, PANIC_NEWS_DAYS, PANIC_NEWS_LIMIT,
        ).await {
            Ok(a) => articles = a,
            Err(e) => eprintln!("[tagdash] panic llm: news fetch failed for {symbol}: {e}"),
        }
    }

    // ── 2. Last 5 daily OHLC bars (date-ascending), from the local cache ──
    // Bounded at the app-clock "today" so a Market Replay never feeds the model
    // bars from the simulated future (inert in live mode).
    let ohlc_block = {
        let today = crate::time::et_date(crate::time::now());
        let conn = db.lock().unwrap();
        let mut bars = cache_repository::get_daily_bars_before(
            &conn, &symbol, &today, PANIC_NEWS_DAYS as u32,
        )
        .unwrap_or_default();
        drop(conn);
        bars.reverse(); // get_daily_bars is date-DESC → make ascending
        format_ohlc(&bars)
    };

    // ── 3. Build the prompt + call Deepseek (temperature 0.2) ──
    let news_block = format_news(&articles);
    let prompt = format!(
        "Ticker {symbol}.\n\n\
         === Prix journaliers (OHLC, 5 derniers jours) ===\n{ohlc_block}\n\n\
         === Nouvelles récentes (≤5 jours) ===\n{news_block}\n\n\
         IMPORTANT: parmi ces nouvelles, certaines n'ont AUCUN intérêt (listes \
         génériques de titres type \"stocks moving\", doublons, articles non \
         spécifiques au catalyseur). Ne tiens compte QUE des nouvelles réellement \
         pertinentes pour expliquer le mouvement ; ignore le reste.\n\n\
         Analyse pour une stratégie de mean-reversion intraday: (1) pourquoi l'action \
         bouge et quelle(s) nouvelle(s) pertinente(s) ont poussé le prix ; (2) cette \
         nouvelle est-elle assez solide pour changer durablement l'équilibre du prix, \
         ou est-ce du bluff/artificiel — et quelle est la probabilité de retour à \
         l'équilibre. Ne cite pas de sources.\n\
         Réponds en EXACTEMENT deux lignes, format strict:\n\
         Contexte: <une phrase très courte>\n\
         Verdict: <retour à l'équilibre faible/moyenne/forte — une phrase très courte>"
    );

    let ds = crate::llm::deepseek::Deepseek::new(dk);
    match ds.complete_with_temperature(SYSTEM_PANIC_FR, &prompt, 0.2).await {
        Ok(ans) => {
            let (ctx, verdict) = parse_panic_answer(&ans);
            // Persist ONLY the result (context + verdict) — appended to history.
            {
                let conn = db.lock().unwrap();
                let _ = crate::local_db::llm_repository::insert_result(
                    &conn,
                    &symbol,
                    crate::strategies::panic_mean_reversion::ID,
                    ctx.as_deref(),
                    verdict.as_deref(),
                );
            }
            update(&store, &symbol, |e| {
                e.llm_context = ctx;
                e.llm_reversion = verdict;
            });
        }
        Err(err) => {
            update(&store, &symbol, |e| e.llm_context = Some(format!("Erreur LLM: {err}")));
        }
    }

    update(&store, &symbol, |e| {
        e.llm_pending = false;
        e.status = "done".into();
    });
}

/// On-demand micro_pullback LLM read (button-triggered, never automatic). Used
/// to fire on every alert — that hammered the Deepseek quota on a busy scanner
/// day, so the read is now a click like panic's. Reuses the news headline
/// already gathered by the automatic enrichment steps instead of re-fetching it.
pub async fn run_micro_pullback_llm(
    symbol: String,
    db: Arc<Mutex<rusqlite::Connection>>,
    secrets: Arc<RwLock<Secrets>>,
    store: Store,
) {
    let deepseek_key = secrets.read().unwrap().deepseek_api_key.clone();
    let Some(dk) = deepseek_key else { return };

    update(&store, &symbol, |e| e.llm_pending = true);
    let ds = crate::llm::deepseek::Deepseek::new(dk);
    let news_title = store.read().unwrap().get(&symbol).and_then(|e| e.news_title.clone());

    // (a) Recent risks, dilution-focused (always).
    let dilution_prompt = format!(
        "Quels sont les risques récents autour de {symbol}, surtout la dilution \
         (contexte: scalping intraday, pas d'analyse long terme) ? Réponds en quelques mots."
    );
    let dilution_ans = ds.complete(SYSTEM_FR, &dilution_prompt).await.ok();
    if let Some(ans) = &dilution_ans {
        update(&store, &symbol, |e| e.llm_dilution = Some(ans.clone()));
    }

    // (b) News bluff vs solid (only when a news item was found).
    let mut news_ans = None;
    if let Some(headline) = &news_title {
        let news_prompt = format!(
            "Voici une news sur {symbol}: \"{headline}\". Est-ce du bluff ou solide ? \
             Réponds très brièvement (mots-clés, arguments)."
        );
        news_ans = ds.complete(SYSTEM_FR, &news_prompt).await.ok();
        if let Some(ans) = &news_ans {
            update(&store, &symbol, |e| e.llm_news = Some(ans.clone()));
        }
    }

    // Persist only the outputs, appended to history (mirrors `run_panic_llm`),
    // so a re-opened zone hydrates the last read without re-calling the model.
    {
        let conn = db.lock().unwrap();
        let _ = crate::local_db::llm_repository::insert_result(
            &conn,
            &symbol,
            crate::strategies::micro_pullback::ID,
            dilution_ans.as_deref(),
            news_ans.as_deref(),
        );
    }

    update(&store, &symbol, |e| {
        e.llm_pending = false;
        e.status = "done".into();
    });
}

/// Format the daily OHLC bars as a compact table for the prompt.
fn format_ohlc(bars: &[crate::local_db::cache_repository::DailyBar]) -> String {
    if bars.is_empty() {
        return "(indisponible)".to_string();
    }
    let mut s = String::from("date        open    high     low   close      volume\n");
    for b in bars {
        let f = |o: Option<f64>| o.map(|v| format!("{v:.3}")).unwrap_or_else(|| "—".into());
        let vol = b.volume.map(format_volume).unwrap_or_else(|| "—".into());
        s.push_str(&format!(
            "{}  {:>6}  {:>6}  {:>6}  {:>6}  {:>10}\n",
            b.date, f(b.open), f(b.high), f(b.low), f(b.close), vol,
        ));
    }
    s
}

/// Format the news articles (date, headline, stripped+truncated body) for the
/// prompt. Empty bodies fall back to the summary.
fn format_news(articles: &[crate::alpaca::news::NewsArticle]) -> String {
    if articles.is_empty() {
        return "(aucune nouvelle trouvée sur la période)".to_string();
    }
    let mut s = String::new();
    for (i, a) in articles.iter().enumerate() {
        let when = a.created_at.format("%Y-%m-%d %H:%M UTC");
        let body = {
            let stripped = strip_html(&a.content);
            let text = if stripped.trim().is_empty() { a.summary.clone() } else { stripped };
            truncate(&text, PANIC_CONTENT_CHARS)
        };
        s.push_str(&format!("{}. [{when}] {}\n", i + 1, a.headline));
        if !body.trim().is_empty() {
            s.push_str(&format!("   {body}\n"));
        }
    }
    s
}

/// Strip HTML tags + decode a few common entities; collapse whitespace.
fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…")
}

/// Human volume, e.g. 12_300_000 → "12.3M".
fn format_volume(v: i64) -> String {
    let v = v as f64;
    if v >= 1e9 { format!("{:.1}B", v / 1e9) }
    else if v >= 1e6 { format!("{:.1}M", v / 1e6) }
    else if v >= 1e3 { format!("{:.0}K", v / 1e3) }
    else { format!("{v:.0}") }
}

/// Split the two-line panic answer into (context, verdict). Tolerant of missing
/// labels: a single unlabeled blob becomes the context.
fn parse_panic_answer(ans: &str) -> (Option<String>, Option<String>) {
    let strip = |line: &str, label: &str| -> Option<String> {
        let l = line.trim();
        let lower = l.to_lowercase();
        let pfx = format!("{}:", label.to_lowercase());
        if lower.starts_with(&pfx) {
            Some(l[pfx.len()..].trim().to_string())
        } else {
            None
        }
    };
    let mut context = None;
    let mut verdict = None;
    for line in ans.lines() {
        if let Some(c) = strip(line, "Contexte") {
            context = Some(c);
        } else if let Some(v) = strip(line, "Verdict") {
            verdict = Some(v);
        }
    }
    // Fallback: no labels at all → put the whole trimmed answer in context.
    if context.is_none() && verdict.is_none() {
        let blob = ans.trim();
        if !blob.is_empty() {
            context = Some(blob.to_string());
        }
    }
    (context, verdict)
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn update<F: FnOnce(&mut AlertEnrichment)>(store: &Store, symbol: &str, f: F) {
    let mut map = store.write().unwrap();
    let e = map.entry(symbol.to_string()).or_insert_with(|| AlertEnrichment {
        symbol: symbol.to_string(),
        status: "loading".into(),
        ..Default::default()
    });
    f(e);
}

/// Issuer-country risk flag: China / Hong Kong → red badge in the UI.
fn is_flagged_country(c: &str) -> bool {
    let l = c.to_lowercase();
    l.contains("china") || l.contains("hong kong") || l == "cn" || l == "hk"
}

fn parse_date(d: &str) -> Option<DateTime<Utc>> {
    let s = d.get(..10).unwrap_or(d);
    let nd = NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()?;
    let ndt = nd.and_hms_opt(0, 0, 0)?;
    Some(Utc.from_utc_datetime(&ndt))
}

fn days_since(date: &str) -> Option<i64> {
    let dt = parse_date(date)?;
    // App clock: split age is relative to the simulated day during a replay.
    Some((crate::time::now() - dt).num_days())
}

fn bardata_to_bar(b: &BarData) -> Option<Bar> {
    Some(Bar {
        time: parse_date(&b.date)?,
        open: b.open?,
        high: b.high?,
        low: b.low?,
        close: b.close?,
        volume: b.volume.unwrap_or(0).max(0) as u64,
        vwap: None,
        trade_count: None,
    })
}
