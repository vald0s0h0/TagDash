// Alpaca REST for the attention map: universe-wide 5-minute bars over a window,
// the previous session's raw closes, and the splits effective on the target day.
//
// WHY RAW BARS (`adjustment=raw`). Dollars are split-invariant, so the medians do
// not care. Prices do: the after-hours of the previous session traded BEFORE a
// split effective this morning, the premarket after it. Asking Alpaca for adjusted
// bars would make the answer depend on whether its own adjustment had already been
// applied when we happened to ask. Raw prices plus the split factor read from the
// corporate actions are deterministic — see `view::Ticker::factor`.
//
// One request carries up to 2 000 symbols (measured: 2 500 pass, URL ≈ 12 kB), so
// the whole universe refreshes in about seven requests instead of sixty-five.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;

use super::frame::{Frame, Part, Slot};
use crate::alpaca::plan::Plan;
use crate::config::secrets::Secrets;
use crate::replay::data::{invalid_symbol_of, is_rest_symbol, RawBar};

const CHUNK: usize = 2_000;
/// Dollars a slot may count per trade it holds, on average. A negotiated block
/// reported after hours puts hundreds of millions in a bar made of three prints
/// (measured: MDT, 1.1 bn$ in 13 slots on 2026-10-09, price flat). That is
/// settlement, not attention, and one such print would outrank every real mover of
/// the evening. A crowded slot is nowhere near this average, so it is untouched.
const BLOCK_TRADE_CAP: f64 = 100_000.0;
/// Alpaca Basic refuses any SIP window ending inside the last 15 minutes (403).
/// One more minute of margin keeps a slow clock out of the refusal.
pub const BASIC_DELAY_MIN: i64 = 16;

#[derive(Clone)]
pub struct Creds {
    pub key: String,
    pub secret: String,
}

pub fn creds(secrets: &Arc<RwLock<Secrets>>) -> Option<Creds> {
    let s = secrets.read().unwrap();
    let key = s.alpaca_key.clone().filter(|k| !k.trim().is_empty())?;
    let secret = s.alpaca_secret.clone().filter(|k| !k.trim().is_empty())?;
    Some(Creds { key, secret })
}

/// Last probed subscription. Unknown reads as Basic: too late costs minutes, too
/// early costs a 403 on every poll.
pub fn plan(db: &Arc<Mutex<rusqlite::Connection>>) -> Plan {
    let conn = db.lock().unwrap();
    crate::local_db::cache_repository::get_app_meta(&conn, crate::alpaca::plan::PLAN_META_KEY)
        .as_deref()
        .and_then(Plan::from_meta)
        .unwrap_or(Plan::Iex)
}

/// Latest instant the provider will serve, on the real clock.
pub fn provider_horizon(plan: Plan) -> DateTime<Utc> {
    match plan {
        Plan::Sip => Utc::now(),
        Plan::Iex => Utc::now() - chrono::Duration::minutes(BASIC_DELAY_MIN),
    }
}

fn rfc(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

#[derive(Debug, Deserialize)]
struct BarsResponse {
    #[serde(default)]
    bars: HashMap<String, Vec<RawBar>>,
    #[serde(default)]
    next_page_token: Option<String>,
}

/// Raw SIP bars of `timeframe` for `symbols` over `[start, end]`. `pace` is slept
/// between requests on top of the shared rate limiter, so a long background fetch
/// (the history bootstrap) leaves room for charts and news.
pub(crate) async fn bars(
    creds: &Creds,
    symbols: &[String],
    timeframe: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    pace: Duration,
) -> Result<HashMap<String, Vec<RawBar>>, String> {
    let mut out: HashMap<String, Vec<RawBar>> = HashMap::new();
    if end <= start {
        return Ok(out);
    }
    let client = crate::http::client();
    let (start, end) = (rfc(start), rfc(end));
    let clean: Vec<String> = symbols.iter().filter(|s| is_rest_symbol(s)).cloned().collect();
    let mut queue: VecDeque<Vec<String>> = clean.chunks(CHUNK).map(|c| c.to_vec()).collect();

    while let Some(mut chunk) = queue.pop_front() {
        let mut page_token: Option<String> = None;
        let mut retries = 0u8;
        loop {
            if chunk.is_empty() {
                break;
            }
            let mut url = format!(
                "https://data.alpaca.markets/v2/stocks/bars?symbols={}&timeframe={timeframe}\
                 &start={start}&end={end}&limit=10000&adjustment=raw&feed=sip",
                chunk.join(",")
            );
            if let Some(tok) = &page_token {
                url.push_str(&format!("&page_token={tok}"));
            }
            crate::alpaca::ratelimit::acquire().await;
            let resp = client
                .get(&url)
                .header("APCA-API-KEY-ID", &creds.key)
                .header("APCA-API-SECRET-KEY", &creds.secret)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let status = resp.status().as_u16();
            if status == 200 {
                let raw: BarsResponse = resp.json().await.map_err(|e| e.to_string())?;
                for (sym, bars) in raw.bars {
                    out.entry(sym).or_default().extend(bars);
                }
                if !pace.is_zero() {
                    tokio::time::sleep(pace).await;
                }
                match raw.next_page_token {
                    Some(tok) if !tok.is_empty() => page_token = Some(tok),
                    _ => break,
                }
                continue;
            }
            let body = resp.text().await.unwrap_or_default();
            match status {
                // One symbol Alpaca does not know sinks the whole request: drop it
                // and restart the chunk from its first page.
                400 => {
                    if let Some(bad) = invalid_symbol_of(&body) {
                        let before = chunk.len();
                        chunk.retain(|s| s != &bad);
                        if chunk.len() < before {
                            page_token = None;
                            continue;
                        }
                    }
                    return Err(format!("Alpaca bars HTTP 400: {body}"));
                }
                // URL too long for a proxy on the way: halve and requeue.
                414 | 431 if chunk.len() > 50 => {
                    let tail = chunk.split_off(chunk.len() / 2);
                    queue.push_front(tail);
                    queue.push_front(std::mem::take(&mut chunk));
                    break;
                }
                429 | 500..=599 if retries < 3 => {
                    retries += 1;
                    tokio::time::sleep(Duration::from_secs(2 * retries as u64)).await;
                }
                _ => return Err(format!("Alpaca bars HTTP {status}: {body}")),
            }
        }
    }
    for v in out.values_mut() {
        v.sort_by(|a, b| a.t.cmp(&b.t));
        v.dedup_by(|a, b| a.t == b.t);
    }
    Ok(out)
}

/// One 5-minute bar on the frame grid. Bars outside both sessions, and bars with
/// no usable price, are dropped rather than repaired.
fn slot_of_bar(frame: &Frame, b: &RawBar) -> Option<Slot> {
    let t = b.t.parse::<DateTime<Utc>>().ok()?;
    let i = frame.slot_of(t)?;
    if ![b.o, b.h, b.l, b.c].iter().all(|p| p.is_finite() && *p > 0.0) || b.v <= 0 {
        return None;
    }
    let price = b.vw.filter(|v| v.is_finite() && *v > 0.0).unwrap_or((b.h + b.l + b.c) / 3.0);
    let trades = b.n.unwrap_or(0).max(0);
    let mut dollars = price * b.v as f64;
    if trades > 0 {
        dollars = dollars.min(trades as f64 * BLOCK_TRADE_CAP);
    }
    Some(Slot {
        i: i as u8,
        o: b.o as f32,
        h: b.h.max(b.o).max(b.c) as f32,
        l: b.l.min(b.o).min(b.c) as f32,
        c: b.c as f32,
        v: b.v.clamp(0, u32::MAX as i64) as u32,
        d: dollars as f32,
        n: trades.min(u32::MAX as i64) as u32,
    })
}

/// The universe's slots of one part of `frame`, read from `from` to `to`.
pub async fn part_slots(
    creds: &Creds,
    symbols: &[String],
    frame: &Frame,
    part: Part,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    pace: Duration,
) -> Result<HashMap<String, Vec<Slot>>, String> {
    let from = from.max(frame.part_start(part));
    // `end` is inclusive on bar timestamps: stop one second short of the part's end
    // so the first bar of what follows (09:30, 20:00) never comes back.
    let to = to.min(frame.part_end(part) - chrono::Duration::seconds(1));
    let raw = bars(creds, symbols, "5Min", from, to, pace).await?;
    let mut out: HashMap<String, Vec<Slot>> = HashMap::with_capacity(raw.len());
    for (sym, bars) in raw {
        let slots: Vec<Slot> = bars.iter().filter_map(|b| slot_of_bar(frame, b)).collect();
        if !slots.is_empty() {
            out.insert(sym, slots);
        }
    }
    Ok(out)
}

/// Raw regular closes of session `day` (the daily bar's close: the closing
/// auction, not the last print before 16:00).
pub async fn daily_closes(
    creds: &Creds,
    symbols: &[String],
    day: NaiveDate,
    horizon: DateTime<Utc>,
) -> Result<HashMap<String, f64>, String> {
    let start = crate::replay::data::noon_utc(day) - chrono::Duration::hours(12);
    let end = (start + chrono::Duration::hours(30)).min(horizon);
    let want = day.to_string();
    let raw = bars(creds, symbols, "1Day", start, end, Duration::ZERO).await?;
    Ok(raw
        .into_iter()
        .filter_map(|(sym, bars)| {
            bars.iter()
                .find(|b| b.t.get(..10) == Some(want.as_str()))
                .filter(|b| b.c.is_finite() && b.c > 0.0)
                .map(|b| (sym, b.c))
        })
        .collect())
}

/// Merge freshly read slots into a symbol's series: a slot read again REPLACES the
/// stored one, which is how a late correction or a cancelled print reaches the map.
pub fn merge(series: &mut HashMap<String, Vec<Slot>>, fresh: HashMap<String, Vec<Slot>>) {
    for (sym, slots) in fresh {
        let cur = series.entry(sym).or_default();
        for s in slots {
            match cur.binary_search_by_key(&s.i, |x| x.i) {
                Ok(pos) => cur[pos] = s,
                Err(pos) => cur.insert(pos, s),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(t: &str, c: f64, v: i64, vw: Option<f64>) -> RawBar {
        RawBar { t: t.into(), o: c, h: c, l: c, c, v, n: Some(4), vw }
    }

    #[test]
    fn bars_land_on_the_grid_and_carry_vwap_dollars() {
        let frame = Frame::for_day(NaiveDate::from_ymd_opt(2026, 10, 12).unwrap());
        // Friday 16:05 ET → slot 1; dollars from the VWAP, not the close.
        let s = slot_of_bar(&frame, &bar("2026-10-09T20:05:00Z", 10.0, 1_000, Some(9.5))).unwrap();
        assert_eq!(s.i, 1);
        assert!((s.d - 9_500.0).abs() < 1e-3);
        // Monday 04:00 ET → first premarket slot.
        let p = slot_of_bar(&frame, &bar("2026-10-12T08:00:00Z", 10.0, 10, None)).unwrap();
        assert_eq!(p.i as usize, super::super::frame::N_AH);
        // Regular hours, an empty bar and a zero price are not slots.
        assert!(slot_of_bar(&frame, &bar("2026-10-12T14:00:00Z", 10.0, 10, None)).is_none());
        assert!(slot_of_bar(&frame, &bar("2026-10-09T20:05:00Z", 10.0, 0, None)).is_none());
        assert!(slot_of_bar(&frame, &bar("2026-10-09T20:05:00Z", 0.0, 10, None)).is_none());
    }

    #[test]
    fn a_block_print_does_not_count_as_a_crowd() {
        let frame = Frame::for_day(NaiveDate::from_ymd_opt(2026, 10, 12).unwrap());
        // 90 M$ in 3 prints: counted as 3 trades' worth, shares untouched.
        let mut block = bar("2026-10-09T21:00:00Z", 90.0, 1_000_000, Some(90.0));
        block.n = Some(3);
        let s = slot_of_bar(&frame, &block).unwrap();
        assert!((s.d - 300_000.0).abs() < 1.0);
        assert_eq!(s.v, 1_000_000);
        // 90 M$ in 20 000 prints is a crowd: counted in full.
        block.n = Some(20_000);
        assert!((slot_of_bar(&frame, &block).unwrap().d - 90_000_000.0).abs() < 10.0);
    }

    #[test]
    fn a_slot_read_again_replaces_the_stored_one() {
        let s = |i: u8, d: f32| Slot { i, o: 1.0, h: 1.0, l: 1.0, c: 1.0, v: 1, d, n: 1 };
        let mut series = HashMap::from([("A".to_string(), vec![s(1, 10.0), s(3, 30.0)])]);
        merge(
            &mut series,
            HashMap::from([("A".to_string(), vec![s(3, 5.0), s(2, 20.0)])]),
        );
        let got: Vec<(u8, f32)> = series["A"].iter().map(|x| (x.i, x.d)).collect();
        // Slot 3 was corrected down, slot 2 arrived late, order is kept.
        assert_eq!(got, vec![(1, 10.0), (2, 20.0), (3, 5.0)]);
    }
}
