// FMP (Financial Modeling Prep) client.
// Endpoint: GET https://financialmodelingprep.com/stable/shares-float-all?apikey={key}
// Returns bulk float data for all US stocks.

use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct FmpFloat {
    pub symbol: String,
    pub float_shares: f64,
    pub outstanding_shares: f64,
    pub free_float: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFmpFloat {
    symbol: String,
    float_shares: Option<f64>,
    outstanding_shares: Option<f64>,
    free_float: Option<f64>,
}

pub async fn fetch_shares_float_all(api_key: &str) -> Result<Vec<FmpFloat>, String> {
    let client = crate::http::client();
    let url = format!(
        "https://financialmodelingprep.com/stable/shares-float-all?apikey={api_key}"
    );
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if status.as_u16() == 429 {
            return Err("FMP rate limited — free tier quota exhausted".into());
        }
        return Err(format!("FMP HTTP {status}: {body}"));
    }

    let raw: Vec<RawFmpFloat> = resp.json().await.map_err(|e| e.to_string())?;
    Ok(raw
        .into_iter()
        .filter_map(|r| {
            Some(FmpFloat {
                symbol: r.symbol,
                float_shares: r.float_shares?,
                outstanding_shares: r.outstanding_shares.unwrap_or(0.0),
                free_float: r.free_float.unwrap_or(0.0),
            })
        })
        .collect())
}

/// Mock float data for a realistic set of small-cap symbols.
pub fn mock_shares_float_all() -> Vec<FmpFloat> {
    // Symbol, float_shares (M), outstanding (M), free_float %
    let data = [
        ("ABCD", 8.5, 10.2, 83.3),
        ("WXYZ", 12.1, 14.0, 86.4),
        ("MNOP", 6.2, 7.8, 79.5),
        ("QRST", 22.0, 25.0, 88.0),
        ("EFGH", 4.8, 6.0, 80.0),
        ("IJKL", 18.5, 20.0, 92.5),
        ("UVWX", 9.7, 11.3, 85.8),
        ("YZAB", 35.0, 38.0, 92.1),
        ("CDEF", 7.3, 9.0, 81.1),
        ("GHIJ", 14.6, 16.5, 88.5),
        ("KLMN", 3.2, 4.1, 78.0),
        ("OPQR", 28.0, 30.0, 93.3),
        ("STUV", 11.4, 13.0, 87.7),
        ("ABCE", 5.5, 6.8, 80.9),
        ("FGHI", 42.0, 45.0, 93.3),
        ("JKLM", 7.9, 9.5, 83.2),
        ("NOPQ", 19.0, 21.0, 90.5),
        ("RSTU", 6.1, 7.4, 82.4),
        ("VWXY", 31.0, 34.0, 91.2),
        ("ZABC", 8.8, 10.5, 83.8),
        ("DEFG", 15.3, 17.0, 90.0),
        ("HIJK", 4.4, 5.6, 78.6),
        ("LMNO", 24.0, 26.5, 90.6),
        ("PQRS", 9.2, 11.0, 83.6),
        ("TUVW", 13.7, 15.5, 88.4),
        ("XYZA", 6.8, 8.2, 82.9),
        ("BCDE", 47.0, 50.0, 94.0),
        ("KLMNO", 5.1, 6.3, 81.0),
        ("PQRST", 10.6, 12.4, 85.5),
        ("UVWXY", 7.4, 9.0, 82.2),
        ("FGHIK", 16.8, 18.5, 90.8),
        ("LMNOP", 8.3, 10.0, 83.0),
        ("QRSTU", 22.5, 25.0, 90.0),
        ("VWXYZ", 5.9, 7.2, 81.9),
        ("ABCDE", 12.0, 14.0, 85.7),
        ("KLMNP", 9.5, 11.2, 84.8),
        ("QRSTUV", 17.2, 19.0, 90.5),
        ("VWXYA", 6.4, 7.8, 82.1),
        ("BCDEA", 38.0, 41.0, 92.7),
        ("FGHIA", 7.7, 9.3, 82.8),
        ("JKLMA", 11.9, 13.8, 86.2),
        ("NOPQA", 4.6, 5.9, 78.0),
        ("RSTUV", 26.0, 28.5, 91.2),
        ("VWXAB", 8.1, 9.8, 82.7),
        ("ZABD", 14.0, 16.0, 87.5),
        ("EFGHI", 6.6, 8.0, 82.5),
        ("JKLMN", 19.5, 21.5, 90.7),
        ("OPQRS", 9.0, 10.8, 83.3),
        ("TUVWX", 33.0, 36.0, 91.7),
        ("EFGHIJ", 7.1, 8.7, 81.6),
        ("KLMNPQ", 11.5, 13.3, 86.5),
        ("QRSTUW", 5.3, 6.6, 80.3),
        ("VWXYZB", 23.0, 25.5, 90.2),
        ("ABCDF", 8.6, 10.3, 83.5),
        ("FGHIJK", 16.1, 18.0, 89.4),
        ("LMNOPQ", 6.9, 8.4, 82.1),
        ("RSTUVW", 29.0, 32.0, 90.6),
        ("XYZABC", 7.5, 9.1, 82.4),
        ("BCDEF", 12.8, 14.7, 87.1),
        ("GHIJKL", 4.9, 6.2, 79.0),
        ("MNOPQR", 21.0, 23.5, 89.4),
        ("STUVWX", 9.3, 11.1, 83.8),
        ("FGHIJM", 15.7, 17.5, 89.7),
        ("KLMNRS", 7.2, 8.8, 81.8),
        ("PQRSTV", 11.1, 13.0, 85.4),
        ("UVWXYZ", 5.7, 7.0, 81.4),
        ("ABCDFG", 18.3, 20.5, 89.3),
        ("GHIJKN", 8.4, 10.1, 83.2),
        ("MNOPQS", 13.4, 15.2, 88.2),
        ("STUVWY", 6.5, 7.9, 82.3),
        ("XYZABF", 24.5, 27.0, 90.7),
        ("CDEFGH", 9.8, 11.6, 84.5),
        ("HIJKLM", 7.6, 9.2, 82.6),
        ("NOPQRT", 14.9, 16.8, 88.7),
        ("STUVWZ", 5.6, 6.9, 81.2),
        ("XYZABD", 20.5, 23.0, 89.1),
        ("CDEFGI", 8.0, 9.7, 82.5),
        ("HIJKLN", 12.3, 14.2, 86.6),
        ("NOPQRU", 6.3, 7.7, 81.8),
        ("STUVXA", 16.5, 18.3, 90.2),
        ("YZCDE2", 9.4, 11.2, 83.9),
    ];
    data.iter()
        .map(|(sym, float_m, out_m, free)| FmpFloat {
            symbol: sym.to_string(),
            float_shares: float_m * 1_000_000.0,
            outstanding_shares: out_m * 1_000_000.0,
            free_float: *free,
        })
        .collect()
}
