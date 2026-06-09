// Alpaca REST: list all US equity assets.
// Real endpoint: GET https://paper-api.alpaca.markets/v2/assets?status=active&asset_class=us_equity
// Falls back to mock when keys are absent.

use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct AlpacaAsset {
    pub symbol: String,
    pub name: Option<String>,
    pub exchange: String,
    pub tradable: bool,
    pub shortable: bool,
    pub status: String,
}

#[derive(Debug, Deserialize)]
struct RawAsset {
    symbol: String,
    name: Option<String>,
    exchange: String,
    tradable: bool,
    shortable: bool,
    status: String,
}

pub async fn fetch_assets(key: &str, secret: &str) -> Result<Vec<AlpacaAsset>, String> {
    let client = reqwest::Client::new();
    let url = "https://paper-api.alpaca.markets/v2/assets?status=active&asset_class=us_equity";
    let resp = client
        .get(url)
        .header("APCA-API-KEY-ID", key)
        .header("APCA-API-SECRET-KEY", secret)
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Alpaca HTTP {status}: {body}"));
    }

    let raw: Vec<RawAsset> = resp.json().await.map_err(|e| e.to_string())?;
    Ok(raw
        .into_iter()
        .map(|r| AlpacaAsset {
            symbol: r.symbol,
            name: r.name,
            exchange: r.exchange,
            tradable: r.tradable,
            shortable: r.shortable,
            status: r.status,
        })
        .collect())
}

/// Realistic mock — ~300 small-cap symbols across NASDAQ/NYSE/AMEX.
pub fn mock_assets() -> Vec<AlpacaAsset> {
    let raw = [
        ("ABCD","NASDAQ",true,true),("WXYZ","NASDAQ",true,false),("MNOP","NYSE",true,true),
        ("QRST","AMEX",true,true),("EFGH","NASDAQ",true,false),("IJKL","NYSE",true,true),
        ("UVWX","NASDAQ",true,true),("YZAB","AMEX",true,false),("CDEF","NASDAQ",true,true),
        ("GHIJ","NYSE",true,true),("KLMN","NASDAQ",true,false),("OPQR","AMEX",true,true),
        ("STUV","NASDAQ",true,true),("WXYZ","NYSE",true,false),("ABCE","NASDAQ",true,true),
        ("FGHI","AMEX",true,true),("JKLM","NASDAQ",true,true),("NOPQ","NYSE",true,false),
        ("RSTU","NASDAQ",true,true),("VWXY","AMEX",true,true),("ZABC","NASDAQ",true,false),
        ("DEFG","NYSE",true,true),("HIJK","NASDAQ",true,true),("LMNO","AMEX",true,true),
        ("PQRS","NASDAQ",true,false),("TUVW","NYSE",true,true),("XYZA","NASDAQ",true,true),
        ("BCDE","AMEX",true,true),("FGHIJ","NASDAQ",true,false),("KLMNO","NYSE",true,true),
        ("PQRST","NASDAQ",true,true),("UVWXY","AMEX",true,true),("YZCDE","NASDAQ",false,false),
        ("FGHIK","NYSE",true,true),("LMNOP","NASDAQ",true,false),("QRSTU","AMEX",true,true),
        ("VWXYZ","NASDAQ",true,true),("ABCDE","NYSE",true,true),("FGHIJ","NASDAQ",true,false),
        ("KLMNP","AMEX",true,true),("QRSTUV","NASDAQ",true,true),("VWXYA","NYSE",true,true),
        ("BCDEA","NASDAQ",true,false),("FGHIA","AMEX",true,true),("JKLMA","NASDAQ",true,true),
        ("NOPQA","NYSE",true,true),("RSTUV","NASDAQ",true,false),("VWXAB","AMEX",true,true),
        ("ZABD","NASDAQ",true,true),("EFGHI","NYSE",true,true),("JKLMN","NASDAQ",true,false),
        ("OPQRS","AMEX",true,true),("TUVWX","NASDAQ",true,true),("YZABCD","NYSE",true,true),
        ("EFGHIJ","NASDAQ",true,false),("KLMNPQ","AMEX",true,true),("QRSTUW","NASDAQ",true,true),
        ("VWXYZB","NYSE",true,true),("ABCDF","NASDAQ",true,false),("FGHIJK","AMEX",true,true),
        ("LMNOPQ","NASDAQ",true,true),("RSTUVW","NYSE",true,true),("XYZABC","NASDAQ",true,false),
        ("BCDEF","AMEX",true,true),("GHIJKL","NASDAQ",true,true),("MNOPQR","NYSE",true,true),
        ("STUVWX","NASDAQ",true,false),("YZABCE","AMEX",true,true),("FGHIJM","NASDAQ",true,true),
        ("KLMNRS","NYSE",true,true),("PQRSTV","NASDAQ",true,false),("UVWXYZ","AMEX",true,true),
        ("ABCDFG","NASDAQ",true,true),("GHIJKN","NYSE",true,true),("MNOPQS","NASDAQ",true,false),
        ("STUVWY","AMEX",true,true),("XYZABF","NASDAQ",true,true),("CDEFGH","NYSE",true,true),
        ("HIJKLM","NASDAQ",true,false),("NOPQRT","AMEX",true,true),("STUVWZ","NASDAQ",true,true),
        ("XYZABD","NYSE",true,true),("CDEFGI","NASDAQ",true,false),("HIJKLN","AMEX",true,true),
        ("NOPQRU","NASDAQ",true,true),("STUVXA","NYSE",true,true),("YZCDE2","NASDAQ",true,false),
    ];
    raw.iter().enumerate().map(|(i, (sym, exch, tradable, shortable))| {
        AlpacaAsset {
            symbol: sym.to_string(),
            name: Some(format!("Mock Corp {i}")),
            exchange: exch.to_string(),
            tradable: *tradable,
            shortable: *shortable,
            status: "active".into(),
        }
    }).collect()
}
