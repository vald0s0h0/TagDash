// A prepared day, read back as the replay engine's event stream.
//
// Inside tick coverage: the real SIP trades and quotes. Everywhere else: each
// minute bar split into synthetic 10-second slices (`replay::data::slices_of`).
// Covered minutes are never ALSO sliced — the tape would print twice. Windows are
// minute-aligned by construction (`plan::windows`), so a minute is either fully
// real or fully synthetic.

use std::collections::HashMap;
use std::path::Path;

use super::intervals::Span;
use super::store::{DayStore, TickKind};
use crate::replay::data::{self, DayData, Event, MinBar, TimedEvent};

/// Whether `[lo, hi)` overlaps any of the merged, sorted `spans`.
fn overlaps(spans: &[Span], lo: i64, hi: i64) -> bool {
    let i = spans.partition_point(|(_, e)| *e <= lo);
    spans.get(i).is_some_and(|(s, _)| *s < hi)
}

/// Read `day` from its store. Errors when the day was never prepared.
pub fn read_day(app_dir: &Path, day: &str) -> Result<DayData, String> {
    let store = DayStore::open_existing(app_dir, day)
        .filter(DayStore::has_market)
        .ok_or_else(|| format!("le {day} n'est pas préparé — préparez-le d'abord"))?;
    let coverage = store.all_coverage();
    let no_spans: Vec<Span> = Vec::new();

    let mut events: Vec<TimedEvent> = Vec::new();
    let symbols = store.kept_symbols();
    for sym in &symbols {
        let covered = coverage
            .get(&(sym.clone(), TickKind::Trades))
            .unwrap_or(&no_spans);
        for b in store.minutes(sym) {
            let t = b.time.timestamp_millis();
            if overlaps(covered, t, t + 60_000) {
                continue;
            }
            let bar = MinBar {
                time: b.time,
                open: b.open,
                high: b.high,
                low: b.low,
                close: b.close,
                volume: b.volume,
                trades: b.trade_count.unwrap_or(0),
                vwap: b.vwap,
            };
            for (ts_ms, price, size, prints) in data::slices_of(&bar) {
                events.push(TimedEvent {
                    ts_ms,
                    ev: Event::Trade {
                        symbol: sym.clone(),
                        price,
                        size,
                        prints,
                        synthetic: true,
                    },
                });
            }
        }
    }

    let trades = store.trades();
    let real = !trades.is_empty();
    for (sym, rows) in trades {
        events.extend(rows.into_iter().map(|r| TimedEvent {
            ts_ms: r.ts,
            ev: Event::Trade {
                symbol: sym.clone(),
                price: r.price,
                size: r.size.max(0) as u64,
                prints: r.prints.max(1) as u64,
                synthetic: false,
            },
        }));
    }
    // The replay reads bid/ask only: a quote that changes neither (a size update)
    // is dropped — on a busy open that is most of them, and every event costs the
    // engine a MarketState write.
    for (sym, rows) in store.quotes() {
        let mut last: Option<(f64, f64)> = None;
        for q in rows {
            if last == Some((q.bid, q.ask)) {
                continue;
            }
            last = Some((q.bid, q.ask));
            events.push(TimedEvent {
                ts_ms: q.ts,
                ev: Event::Quote {
                    symbol: sym.clone(),
                    bid: q.bid,
                    ask: q.ask,
                },
            });
        }
    }
    let mut news_ids = std::collections::HashSet::new();
    for n in store.news() {
        news_ids.insert(n.id);
        events.push(TimedEvent {
            ts_ms: n.created_at.timestamp_millis(),
            ev: Event::News(n),
        });
    }
    // Stable: at equal timestamps a quote stays before the trade it was read with.
    events.sort_by_key(|e| e.ts_ms);

    Ok(DayData {
        events,
        prev_closes: store.prev_closes(),
        source: if real { "trades" } else { "minutes" },
        provider: "alpaca_sip",
        symbols: symbols.len(),
        kept: symbols.into_iter().collect(),
        news_ids,
    })
}

/// Tick coverage per symbol (trades), for the toolbar's "real vs synthetic" badge.
pub fn trade_coverage(app_dir: &Path, day: &str) -> HashMap<String, Vec<Span>> {
    DayStore::open_existing(app_dir, day)
        .map(|s| {
            s.all_coverage()
                .into_iter()
                .filter(|((_, k), _)| *k == TickKind::Trades)
                .map(|((sym, _), spans)| (sym, spans))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlap_is_half_open() {
        let spans = vec![(100, 200), (300, 400)];
        assert!(overlaps(&spans, 150, 160));
        assert!(overlaps(&spans, 50, 101));
        assert!(!overlaps(&spans, 200, 300), "touching both ends is not overlapping");
        assert!(!overlaps(&spans, 0, 100));
        assert!(overlaps(&spans, 399, 500));
        assert!(!overlaps(&spans, 400, 500));
        assert!(!overlaps(&[], 0, 10));
    }
}
