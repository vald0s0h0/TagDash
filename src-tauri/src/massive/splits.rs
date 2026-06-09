// Massive corporate-actions: most recent stock split for a ticker.
//
// Assumed endpoint (modelled on the float endpoint, confirmed base + auth):
//   GET https://api.massive.com/stocks/vX/corporate-actions/splits
//        ?ticker={T}&limit=1&sort=execution_date.desc&apiKey={key}
// Field names are guessed and covered by serde aliases — adjust here once the
// live response shape is confirmed.

use serde::Deserialize;

use super::{with_key, BASE_URL};

/// One split event, normalised.
#[derive(Debug, Clone)]
pub struct Split {
    /// Execution / ex date, YYYY-MM-DD.
    pub date: String,
    pub from: f64,
    pub to:   f64,
}

impl Split {
    /// Human label: "x20" for a 20-for-1 forward split, "1:20" for a reverse.
    pub fn label(&self) -> String {
        if self.from <= 0.0 || self.to <= 0.0 {
            return "split".into();
        }
        let trim = |v: f64| {
            if (v - v.round()).abs() < 1e-6 {
                format!("{}", v.round() as i64)
            } else {
                format!("{v:.2}")
            }
        };
        if self.to >= self.from {
            format!("x{}", trim(self.to / self.from))
        } else {
            format!("1:{}", trim(self.from / self.to))
        }
    }
}

#[derive(Debug, Deserialize)]
struct SplitsResponse {
    #[serde(default)]
    results: Vec<RawSplit>,
}

#[derive(Debug, Deserialize)]
struct RawSplit {
    #[serde(default, alias = "execution_date", alias = "ex_date", alias = "date")]
    execution_date: Option<String>,
    #[serde(default, alias = "split_from", alias = "from_factor", alias = "from")]
    split_from: Option<f64>,
    #[serde(default, alias = "split_to", alias = "to_factor", alias = "to")]
    split_to: Option<f64>,
}

impl RawSplit {
    fn into_split(self) -> Option<Split> {
        let date = self.execution_date?;
        let date = date.get(..10).unwrap_or(&date).to_string();
        Some(Split {
            date,
            from: self.split_from.unwrap_or(1.0),
            to:   self.split_to.unwrap_or(1.0),
        })
    }
}

/// Fetch the most recent split for `ticker`, or None if the ticker never split.
pub async fn fetch_latest_split(api_key: &str, ticker: &str) -> Result<Option<Split>, String> {
    let url = with_key(
        &format!("{BASE_URL}/stocks/vX/corporate-actions/splits?ticker={ticker}&limit=1&sort=execution_date.desc"),
        api_key,
    );
    let client = reqwest::Client::new();
    let resp = client.get(&url).send().await.map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("Massive splits HTTP {}", resp.status()));
    }
    let parsed: SplitsResponse = resp.json().await.map_err(|e| e.to_string())?;
    Ok(parsed.results.into_iter().filter_map(RawSplit::into_split).next())
}
