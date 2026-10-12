// Compact encoding of a session's tape: prints and quotes as COLUMNS, each value
// stored as a difference with the previous one (varint, zig-zag), strings (exchange,
// conditions, origin) through one dictionary, the whole compressed with zstd.
//
// Lossless for what TagDash records: timestamps to the millisecond, prices to the
// micro-dollar (Alpaca gives at most 4 decimals), sizes and counts exact. About
// 6–8 bytes a print instead of ~60 as SQLite rows with an index.
//
// Layout (before zstd): version u8 · row count · dictionary (count, then length +
// UTF-8 bytes each) · the columns one after the other.

use std::collections::HashMap;

const VERSION: u8 = 1;
const ZSTD_LEVEL: i32 = 12;
const PRICE_SCALE: f64 = 1e6;

#[derive(Debug, Clone, PartialEq)]
pub struct TradeRow {
    pub ts: i64,
    pub price: f64,
    pub size: i64,
    /// 1 = a real print; > 1 an aggregate (see `store`).
    pub prints: i64,
    pub exch: Option<String>,
    pub cond: Option<String>,
    pub origin: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuoteRow {
    pub ts: i64,
    pub bid: f64,
    pub bid_size: Option<i64>,
    pub ask: f64,
    pub ask_size: Option<i64>,
    pub bid_x: Option<String>,
    pub ask_x: Option<String>,
    pub origin: String,
}

// ─── Primitives ─────────────────────────────────────────────────────────────────

fn put(buf: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        buf.push((v as u8) | 0x80);
        v >>= 7;
    }
    buf.push(v as u8);
}

fn put_i(buf: &mut Vec<u8>, v: i64) {
    put(buf, ((v << 1) ^ (v >> 63)) as u64);
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn u(&mut self) -> Result<u64, String> {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let byte = *self.b.get(self.at).ok_or("bloc tronqué")?;
            self.at += 1;
            v |= u64::from(byte & 0x7f) << shift;
            if byte < 0x80 {
                return Ok(v);
            }
            shift += 7;
            if shift > 63 {
                return Err("varint invalide".into());
            }
        }
    }
    fn i(&mut self) -> Result<i64, String> {
        let u = self.u()?;
        Ok(((u >> 1) as i64) ^ -((u & 1) as i64))
    }
    fn bytes(&mut self, n: usize) -> Result<&[u8], String> {
        let s = self.b.get(self.at..self.at + n).ok_or("bloc tronqué")?;
        self.at += n;
        Ok(s)
    }
}

fn ticks(p: f64) -> i64 {
    (p * PRICE_SCALE).round() as i64
}

fn price(t: i64) -> f64 {
    t as f64 / PRICE_SCALE
}

/// Strings of a block → small integers (0 = none).
#[derive(Default)]
struct Dict {
    list: Vec<String>,
    index: HashMap<String, u64>,
}

impl Dict {
    fn id(&mut self, s: Option<&str>) -> u64 {
        let Some(s) = s else { return 0 };
        if let Some(&i) = self.index.get(s) {
            return i;
        }
        self.list.push(s.to_string());
        let i = self.list.len() as u64;
        self.index.insert(s.to_string(), i);
        i
    }
}

fn head(n: usize, dict: &Dict) -> Vec<u8> {
    let mut out = vec![VERSION];
    put(&mut out, n as u64);
    put(&mut out, dict.list.len() as u64);
    for s in &dict.list {
        put(&mut out, s.len() as u64);
        out.extend_from_slice(s.as_bytes());
    }
    out
}

fn open_block(data: &[u8]) -> Result<(Vec<u8>, usize, Vec<String>, usize), String> {
    let raw = zstd::decode_all(data).map_err(|e| e.to_string())?;
    let mut r = Reader { b: &raw, at: 0 };
    let v = *r.bytes(1)?.first().unwrap_or(&0);
    if v != VERSION {
        return Err(format!("version de bloc inconnue ({v})"));
    }
    let n = r.u()? as usize;
    let k = r.u()? as usize;
    let mut dict = Vec::with_capacity(k);
    for _ in 0..k {
        let len = r.u()? as usize;
        dict.push(String::from_utf8_lossy(r.bytes(len)?).into_owned());
    }
    let at = r.at;
    Ok((raw, n, dict, at))
}

fn lookup(dict: &[String], id: u64) -> Result<Option<String>, String> {
    if id == 0 {
        return Ok(None);
    }
    dict.get(id as usize - 1).cloned().map(Some).ok_or_else(|| "dictionnaire invalide".into())
}

fn compress(parts: Vec<u8>) -> Result<Vec<u8>, String> {
    zstd::encode_all(parts.as_slice(), ZSTD_LEVEL).map_err(|e| e.to_string())
}

fn opt(v: Option<i64>) -> u64 {
    v.map_or(0, |x| x.max(0) as u64 + 1)
}

fn unopt(u: u64) -> Option<i64> {
    (u > 0).then(|| (u - 1) as i64)
}

// ─── Trades ─────────────────────────────────────────────────────────────────────

pub fn encode_trades(rows: &[TradeRow]) -> Result<Vec<u8>, String> {
    let mut dict = Dict::default();
    let mut cols = Vec::with_capacity(rows.len() * 8);
    let (mut prev_ts, mut prev_px) = (0i64, 0i64);
    for r in rows {
        put_i(&mut cols, r.ts - prev_ts);
        prev_ts = r.ts;
    }
    for r in rows {
        let t = ticks(r.price);
        put_i(&mut cols, t - prev_px);
        prev_px = t;
    }
    for r in rows {
        put_i(&mut cols, r.size);
    }
    for r in rows {
        put_i(&mut cols, r.prints);
    }
    for r in rows {
        let id = dict.id(r.exch.as_deref());
        put(&mut cols, id);
    }
    for r in rows {
        let id = dict.id(r.cond.as_deref());
        put(&mut cols, id);
    }
    for r in rows {
        let id = dict.id(Some(&r.origin));
        put(&mut cols, id);
    }
    let mut out = head(rows.len(), &dict);
    out.extend_from_slice(&cols);
    compress(out)
}

pub fn decode_trades(data: &[u8]) -> Result<Vec<TradeRow>, String> {
    let (raw, n, dict, at) = open_block(data)?;
    let mut r = Reader { b: &raw, at };
    let mut ts = Vec::with_capacity(n);
    let mut acc = 0i64;
    for _ in 0..n {
        acc += r.i()?;
        ts.push(acc);
    }
    let mut px = Vec::with_capacity(n);
    acc = 0;
    for _ in 0..n {
        acc += r.i()?;
        px.push(acc);
    }
    let size: Vec<i64> = (0..n).map(|_| r.i()).collect::<Result<_, _>>()?;
    let prints: Vec<i64> = (0..n).map(|_| r.i()).collect::<Result<_, _>>()?;
    let exch: Vec<u64> = (0..n).map(|_| r.u()).collect::<Result<_, _>>()?;
    let cond: Vec<u64> = (0..n).map(|_| r.u()).collect::<Result<_, _>>()?;
    let origin: Vec<u64> = (0..n).map(|_| r.u()).collect::<Result<_, _>>()?;
    (0..n)
        .map(|i| {
            Ok(TradeRow {
                ts: ts[i],
                price: price(px[i]),
                size: size[i],
                prints: prints[i],
                exch: lookup(&dict, exch[i])?,
                cond: lookup(&dict, cond[i])?,
                origin: lookup(&dict, origin[i])?.unwrap_or_default(),
            })
        })
        .collect()
}

// ─── Quotes ─────────────────────────────────────────────────────────────────────

pub fn encode_quotes(rows: &[QuoteRow]) -> Result<Vec<u8>, String> {
    let mut dict = Dict::default();
    let mut cols = Vec::with_capacity(rows.len() * 10);
    let mut prev = 0i64;
    for r in rows {
        put_i(&mut cols, r.ts - prev);
        prev = r.ts;
    }
    prev = 0;
    for r in rows {
        let t = ticks(r.bid);
        put_i(&mut cols, t - prev);
        prev = t;
    }
    // The ask as its distance to the bid: the spread, small and repetitive.
    for r in rows {
        put_i(&mut cols, ticks(r.ask) - ticks(r.bid));
    }
    for r in rows {
        put(&mut cols, opt(r.bid_size));
    }
    for r in rows {
        put(&mut cols, opt(r.ask_size));
    }
    for r in rows {
        let id = dict.id(r.bid_x.as_deref());
        put(&mut cols, id);
    }
    for r in rows {
        let id = dict.id(r.ask_x.as_deref());
        put(&mut cols, id);
    }
    for r in rows {
        let id = dict.id(Some(&r.origin));
        put(&mut cols, id);
    }
    let mut out = head(rows.len(), &dict);
    out.extend_from_slice(&cols);
    compress(out)
}

pub fn decode_quotes(data: &[u8]) -> Result<Vec<QuoteRow>, String> {
    let (raw, n, dict, at) = open_block(data)?;
    let mut r = Reader { b: &raw, at };
    let mut ts = Vec::with_capacity(n);
    let mut acc = 0i64;
    for _ in 0..n {
        acc += r.i()?;
        ts.push(acc);
    }
    let mut bid = Vec::with_capacity(n);
    acc = 0;
    for _ in 0..n {
        acc += r.i()?;
        bid.push(acc);
    }
    let spread: Vec<i64> = (0..n).map(|_| r.i()).collect::<Result<_, _>>()?;
    let bs: Vec<u64> = (0..n).map(|_| r.u()).collect::<Result<_, _>>()?;
    let as_: Vec<u64> = (0..n).map(|_| r.u()).collect::<Result<_, _>>()?;
    let bx: Vec<u64> = (0..n).map(|_| r.u()).collect::<Result<_, _>>()?;
    let ax: Vec<u64> = (0..n).map(|_| r.u()).collect::<Result<_, _>>()?;
    let origin: Vec<u64> = (0..n).map(|_| r.u()).collect::<Result<_, _>>()?;
    (0..n)
        .map(|i| {
            Ok(QuoteRow {
                ts: ts[i],
                bid: price(bid[i]),
                ask: price(bid[i] + spread[i]),
                bid_size: unopt(bs[i]),
                ask_size: unopt(as_[i]),
                bid_x: lookup(&dict, bx[i])?,
                ask_x: lookup(&dict, ax[i])?,
                origin: lookup(&dict, origin[i])?.unwrap_or_default(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trades_round_trip_exactly() {
        let rows = vec![
            TradeRow { ts: 1_750_000_000_123, price: 5.02, size: 100, prints: 1, exch: Some("V".into()), cond: Some("@,I".into()), origin: "sip_rest".into() },
            TradeRow { ts: 1_750_000_000_123, price: 0.8123, size: 7, prints: 1, exch: None, cond: None, origin: "sip_live".into() },
            TradeRow { ts: 1_750_000_001_000, price: 12.345678, size: 2_000, prints: 14, exch: Some("V".into()), cond: None, origin: "massive_live".into() },
            TradeRow { ts: 1_749_999_999_999, price: 5.01, size: 0, prints: 0, exch: None, cond: Some("@,I".into()), origin: "sip_rest".into() },
        ];
        let blob = encode_trades(&rows).unwrap();
        assert_eq!(decode_trades(&blob).unwrap(), rows);
        assert!(decode_trades(&encode_trades(&[]).unwrap()).unwrap().is_empty());
    }

    #[test]
    fn quotes_round_trip_exactly() {
        let rows = vec![
            QuoteRow { ts: 1_750_000_000_000, bid: 5.0, bid_size: Some(3), ask: 5.02, ask_size: None, bid_x: Some("Q".into()), ask_x: Some("P".into()), origin: "sip_rest".into() },
            QuoteRow { ts: 1_750_000_000_004, bid: 5.03, bid_size: Some(0), ask: 5.01, ask_size: Some(12), bid_x: None, ask_x: None, origin: "tape_sip".into() },
        ];
        let blob = encode_quotes(&rows).unwrap();
        assert_eq!(decode_quotes(&blob).unwrap(), rows);
    }

    #[test]
    fn a_busy_tape_packs_small() {
        let mut rows = Vec::new();
        let mut ts = 1_750_000_000_000i64;
        for i in 0..100_000i64 {
            ts += (i * 7919) % 13;
            rows.push(TradeRow {
                ts,
                price: 5.0 + ((i * 31) % 17) as f64 / 100.0,
                size: [100, 1, 50, 200, 7][(i % 5) as usize],
                prints: 1,
                exch: Some(["V", "Q", "P", "K"][(i % 4) as usize].into()),
                cond: Some(if i % 3 == 0 { "@,I" } else { "@" }.into()),
                origin: "sip_rest".into(),
            });
        }
        let blob = encode_trades(&rows).unwrap();
        assert!(blob.len() < 100_000 * 8, "{} bytes", blob.len());
        assert_eq!(decode_trades(&blob).unwrap().len(), 100_000);
    }
}
