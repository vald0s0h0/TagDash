// Daily price-action classification: "pump & dump" vs "momo former" vs nothing.
// Quality over forcing — returning None is a perfectly valid outcome.
//
//  - pump & dump : at least one day with a true range ≥ PD_TR_MULT × the average
//    TR of the sample AND a dominant wick on that day (wick ≥ body). A single
//    such day in the sample is enough.
//  - momo former : no pump&dump day AND candle bodies dominate (the median
//    body/range across the sample is high → price "held" during the day).

use crate::market_state::aggregators::Bar;
use crate::types::Classification;

// ─── Tunable parameters ───────────────────────────────────────────────────────
/// A wick day's TR must reach this multiple of the average TR.
const PD_TR_MULT: f64 = 3.0;
/// On a wick day, the wick must be at least this multiple of the candle body.
const PD_WICK_DOMINANCE: f64 = 1.0;
/// Median body/range must reach this to call a ticker "momo former".
const MOMO_BODY_DOMINANCE: f64 = 0.6;
/// Minimum number of daily bars required to classify at all.
const MIN_SAMPLE: usize = 20;
/// Number of most-recent daily bars used as the classification sample.
const CLASSIFY_SAMPLE: usize = 100;

pub fn classify(bars: &[Bar]) -> Option<Classification> {
    if bars.len() < MIN_SAMPLE {
        return None;
    }
    let sample = &bars[bars.len().saturating_sub(CLASSIFY_SAMPLE)..];

    // True range per day (needs the previous close → start at index 1).
    let mut trs = Vec::with_capacity(sample.len());
    for i in 1..sample.len() {
        let b = &sample[i];
        let pc = sample[i - 1].close;
        let tr = (b.high - b.low)
            .max((b.high - pc).abs())
            .max((b.low - pc).abs());
        trs.push(tr);
    }
    if trs.is_empty() {
        return None;
    }
    let avg_tr = trs.iter().sum::<f64>() / trs.len() as f64;
    if avg_tr <= 0.0 {
        return None;
    }

    // Scan for pump&dump days and collect body/range ratios for the momo test.
    let mut body_ratios = Vec::with_capacity(sample.len());
    let mut pump = false;
    for i in 1..sample.len() {
        let b = &sample[i];
        let pc = sample[i - 1].close;
        let tr = (b.high - b.low)
            .max((b.high - pc).abs())
            .max((b.low - pc).abs());

        let body  = (b.close - b.open).abs();
        let upper = (b.high - b.open.max(b.close)).max(0.0);
        let lower = (b.open.min(b.close) - b.low).max(0.0);
        let wick  = upper + lower;
        let range = (b.high - b.low).max(1e-9);
        body_ratios.push(body / range);

        if tr >= PD_TR_MULT * avg_tr && wick >= PD_WICK_DOMINANCE * body {
            pump = true;
        }
    }

    if pump {
        return Some(Classification::PumpDump);
    }

    // Momo former: bodies dominate (price held intraday).
    body_ratios.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = body_ratios[body_ratios.len() / 2];
    if median >= MOMO_BODY_DOMINANCE {
        return Some(Classification::MomoFormer);
    }

    None
}
