// Alpaca corporate-actions: historical stock splits (forward + reverse) for a
// ticker. Used to draw red split-day markers on the daily chart.
//
// Endpoint: GET https://data.alpaca.markets/v1/corporate-actions
//   ?symbols={T}&types=forward_split,reverse_split&start={d}&end={d}&limit=1000&sort=desc
// Auth: the same APCA-API-KEY-ID / APCA-API-SECRET-KEY headers the bars use.
//
// We only need the split DATES (the ex-date is when the chart price adjusts);
// the ratio is kept solely to label the marker (the caller can ignore it).

use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json;

/// One split event, normalised to (ex-date, human label).
#[derive(Debug, Clone)]
pub struct Split {
    /// Ex-date, YYYY-MM-DD — the day the split takes effect on the chart.
    pub date:  String,
    /// Human label: "x4" for a 4-for-1 forward split, "1:10" for a reverse.
    pub label: String,
}

#[derive(Debug, Deserialize)]
struct CorporateActionsResponse {
    // Alpaca returns null (not missing key) when no events are found for a period;
    // Option handles both absent-key and explicit null.
    #[serde(default)]
    corporate_actions: Option<CorporateActions>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct CorporateActions {
    #[serde(default)]
    forward_splits: Vec<RawSplit>,
    #[serde(default)]
    reverse_splits: Vec<RawSplit>,
}

#[derive(Debug, Deserialize)]
struct RawSplit {
    #[serde(default)]
    symbol:   Option<String>,
    #[serde(default, alias = "ex_date", alias = "process_date")]
    ex_date:  Option<String>,
    #[serde(default)]
    new_rate: Option<f64>,
    #[serde(default)]
    old_rate: Option<f64>,
}

impl RawSplit {
    fn into_split(self) -> Option<Split> {
        let date = self.ex_date?;
        let date = date.get(..10).unwrap_or(&date).to_string();
        let label = split_label(self.new_rate, self.old_rate);
        Some(Split { date, label })
    }
}

/// "x{n}" for a forward split (new_rate ≥ old_rate), "1:{n}" for a reverse.
/// Falls back to "split" when the rates are missing/degenerate.
fn split_label(new_rate: Option<f64>, old_rate: Option<f64>) -> String {
    let (Some(new_rate), Some(old_rate)) = (new_rate, old_rate) else {
        return "split".into();
    };
    if new_rate <= 0.0 || old_rate <= 0.0 {
        return "split".into();
    }
    let trim = |v: f64| {
        if (v - v.round()).abs() < 1e-6 {
            format!("{}", v.round() as i64)
        } else {
            format!("{v:.2}")
        }
    };
    if new_rate >= old_rate {
        format!("x{}", trim(new_rate / old_rate))
    } else {
        format!("1:{}", trim(old_rate / new_rate))
    }
}

/// Fetch every forward/reverse split for `ticker` over the last `years` years,
/// newest first. Empty when the ticker never split in that window.
pub async fn fetch_splits(
    key: &str,
    secret: &str,
    ticker: &str,
    years: i64,
) -> Result<Vec<Split>, String> {
    // App clock: in replay the window ends on the simulated day, so a split with
    // an ex-date after the replayed day can never leak into the chart markers.
    let now = crate::time::now();
    let start = (now - Duration::days(years.max(1) * 366)).format("%Y-%m-%d").to_string();
    let end = now.format("%Y-%m-%d").to_string();

    let client = reqwest::Client::new();
    let mut out: Vec<Split> = Vec::new();
    let mut page_token: Option<String> = None;

    loop {
        let mut url = format!(
            "https://data.alpaca.markets/v1/corporate-actions?symbols={ticker}\
             &types=forward_split,reverse_split&start={start}&end={end}&limit=1000&sort=desc"
        );
        if let Some(tok) = &page_token {
            url.push_str(&format!("&page_token={tok}"));
        }

        let resp = client
            .get(&url)
            .header("APCA-API-KEY-ID", key)
            .header("APCA-API-SECRET-KEY", secret)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("Alpaca corporate-actions HTTP {status}: {body}"));
        }

        let body = resp.text().await.map_err(|e| e.to_string())?;
        let parsed: CorporateActionsResponse = serde_json::from_str(&body)
            .map_err(|e| format!("corporate-actions parse error: {e} — body: {}", &body[..body.len().min(400)]))?;
        if let Some(ca) = parsed.corporate_actions {
            for s in ca.forward_splits.into_iter().chain(ca.reverse_splits) {
                if let Some(split) = s.into_split() {
                    out.push(split);
                }
            }
        }

        match parsed.next_page_token {
            Some(tok) if !tok.is_empty() => page_token = Some(tok),
            _ => break,
        }
    }

    // Both split kinds come back in their own arrays; sort the merged list by
    // date descending so the caller's "most recent split" is out[0].
    out.sort_by(|a, b| b.date.cmp(&a.date));
    Ok(out)
}

/// Distinct symbols among `symbols` that had a forward/reverse split with an
/// ex-date on/after `start` (YYYY-MM-DD), up to today. Used at startup to detect
/// splits that invalidate the split-adjusted daily cache: `adjustment=split`
/// rescales the WHOLE series to the latest split factor, so a fresh split leaves
/// the previously-cached older bars at the old scale (a fake gap) — those symbols
/// must be purged and refetched in full. The bulk corporate-actions endpoint
/// accepts a comma list, so we chunk the universe and follow pagination.
pub async fn fetch_recent_split_symbols(
    key: &str,
    secret: &str,
    symbols: &[String],
    start: &str,
) -> Result<Vec<String>, String> {
    use std::collections::HashSet;
    if symbols.is_empty() {
        return Ok(vec![]);
    }
    let start = start.get(..10).unwrap_or(start).to_string();
    let end = Utc::now().format("%Y-%m-%d").to_string();
    let client = reqwest::Client::new();
    let mut hit: HashSet<String> = HashSet::new();

    for chunk in symbols.chunks(100) {
        let sym_str = chunk.join(",");
        let mut page_token: Option<String> = None;
        loop {
            let mut url = format!(
                "https://data.alpaca.markets/v1/corporate-actions?symbols={sym_str}\
                 &types=forward_split,reverse_split&start={start}&end={end}&limit=1000&sort=desc"
            );
            if let Some(tok) = &page_token {
                url.push_str(&format!("&page_token={tok}"));
            }
            let resp = client
                .get(&url)
                .header("APCA-API-KEY-ID", key)
                .header("APCA-API-SECRET-KEY", secret)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(format!("Alpaca corporate-actions HTTP {status}: {body}"));
            }
            let body = resp.text().await.map_err(|e| e.to_string())?;
            let parsed: CorporateActionsResponse = serde_json::from_str(&body)
                .map_err(|e| format!("corporate-actions parse error: {e} — body: {}", &body[..body.len().min(400)]))?;
            if let Some(ca) = &parsed.corporate_actions {
                for s in ca.forward_splits.iter().chain(ca.reverse_splits.iter()) {
                    if let Some(sym) = &s.symbol {
                        hit.insert(sym.clone());
                    }
                }
            }
            match parsed.next_page_token {
                Some(tok) if !tok.is_empty() => page_token = Some(tok),
                _ => break,
            }
        }
    }

    Ok(hit.into_iter().collect())
}
