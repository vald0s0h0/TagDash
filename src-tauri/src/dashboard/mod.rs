// Dashboard (moodboard) backend: pull the user's trades from TradeTally (source of
// truth), and pick a deterministic daily background image from the user's folder.
// KPI / series math is done on the frontend from `DashboardTrade`s, so this module
// stays a thin fetch + parse + file layer.

use std::path::Path;
use std::process::Command;

use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tradetally::TtClient;

// ─── Types ───────────────────────────────────────────────────────────────────

/// One trade mirrored from TradeTally. Column-mapped fields are convenience; the
/// full upstream object is kept in `raw` so new cards can read anything later.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardTrade {
    pub tt_id:       String,
    pub symbol:      Option<String>,
    pub side:        Option<String>,
    pub quantity:    Option<f64>,
    pub entry_price: Option<f64>,
    pub exit_price:  Option<f64>,
    pub pnl:         Option<f64>,
    pub pnl_percent: Option<f64>,
    pub entry_date:  Option<String>,
    pub exit_date:   Option<String>,
    pub commission:  Option<f64>,
    pub fees:        Option<f64>,
    pub status:      Option<String>,
    pub setup:       Option<String>,
    pub strategy:    Option<String>,
    pub broker:      Option<String>,
    pub tags:        Vec<String>,
    pub raw:         Value,
}

/// The folder where the user drops background photos + the image chosen for today
/// (deterministic per ET day), already encoded as a data-URL so the frontend can
/// set it as a `src` directly. Empty folder → no image, just the folder path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyBackground {
    pub dir:       String,
    pub file_name: Option<String>,
    pub data_url:  Option<String>,
}

/// One random inspiration image, already encoded as a data-URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoodImage {
    pub file_name: String,
    pub data_url:  String,
}

/// A fresh, *random* mood pick (unlike `DailyBackground`, which is stable per day):
/// one image + one short phrase + one long phrase, drawn anew on every call so the
/// dashboard shows something different on each open / refresh. Each field is `None`
/// when its folder / file is empty. The paths are returned so the UI can open them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mood {
    pub images_dir:   String,
    pub short_path:   String,
    pub long_path:    String,
    pub image:        Option<MoodImage>,
    pub short_phrase: Option<String>,
    /// A second short phrase, distinct from `short_phrase` when the file has ≥2
    /// lines — used by the H1 card so it never echoes the Inspiration card.
    pub heading_phrase: Option<String>,
    pub long_phrase:  Option<String>,
}

// ─── TradeTally sync ─────────────────────────────────────────────────────────

/// Page through `GET /api/v1/trades` and return every trade. The caller upserts
/// the result into the `tt_trades` cache. Defensive parsing throughout — missing
/// fields are `None`, and a trade with no id is skipped (it can't be keyed).
pub async fn sync_trades(client: &TtClient) -> Result<Vec<DashboardTrade>, String> {
    const LIMIT: usize = 100;
    const MAX_PAGES: usize = 500; // safety cap (~50k trades)

    let mut all: Vec<DashboardTrade> = Vec::new();
    let mut page = 1usize;

    loop {
        let endpoint = format!("/api/v1/trades?page={page}&limit={LIMIT}");
        let resp = client.get_json(&endpoint).await?;

        // The v1 list endpoint wraps trades in `{ data: [...], pagination: {...} }`.
        // Accept `trades` and a bare array too, just in case the shape varies.
        let trades: Vec<Value> = resp
            .get("data")
            .or_else(|| resp.get("trades"))
            .and_then(|v| v.as_array())
            .cloned()
            .or_else(|| resp.as_array().cloned())
            .unwrap_or_default();

        if trades.is_empty() {
            break;
        }
        let batch = trades.len();
        for t in &trades {
            if let Some(parsed) = parse_trade(t) {
                all.push(parsed);
            }
        }

        // Stop on the API's `pagination.hasMore` flag; fall back to a short batch.
        match resp
            .get("pagination")
            .and_then(|p| p.get("hasMore"))
            .and_then(|v| v.as_bool())
        {
            Some(false) => break,
            Some(true) => {}
            None if batch < LIMIT => break,
            None => {}
        }

        page += 1;
        if page > MAX_PAGES {
            break;
        }
    }

    Ok(all)
}

fn parse_trade(v: &Value) -> Option<DashboardTrade> {
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    let f = |k: &str| v.get(k).and_then(json_to_f64);

    // Must have an id to key the row.
    let tt_id = s("id").or_else(|| s("uuid"))?;

    // The v1 trade carries timestamps (`entry_time`/`exit_time`) + a `trade_date`,
    // not `entry_date`/`exit_date`, and has no `status` field. Derive closed/open
    // from the exit timestamp; ISO strings sort chronologically as text.
    let exit_time = s("exit_time");
    let status = s("status").or_else(|| {
        Some(if exit_time.is_some() { "Closed".to_string() } else { "Open".to_string() })
    });

    Some(DashboardTrade {
        tt_id,
        symbol:      s("symbol"),
        side:        s("side"),
        quantity:    f("quantity"),
        entry_price: f("entry_price"),
        exit_price:  f("exit_price"),
        pnl:         f("pnl"),
        pnl_percent: f("pnl_percent"),
        entry_date:  s("entry_time").or_else(|| s("trade_date")),
        exit_date:   exit_time,
        commission:  f("commission"),
        fees:        f("fees"),
        status,
        setup:       s("setup"),
        strategy:    s("strategy"),
        broker:      s("broker"),
        tags:        parse_tags(v.get("tags")),
        raw:         v.clone(),
    })
}

/// Numbers may arrive as JSON numbers or numeric strings — handle both.
fn json_to_f64(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
}

/// Tags can be an array of strings or of `{ name: "…" }` objects.
fn parse_tags(v: Option<&Value>) -> Vec<String> {
    let Some(Value::Array(arr)) = v else { return Vec::new() };
    arr.iter()
        .filter_map(|t| match t {
            Value::String(s) => Some(s.clone()),
            Value::Object(_) => t.get("name").and_then(|n| n.as_str()).map(str::to_string),
            _ => None,
        })
        .collect()
}

// ─── Daily background ────────────────────────────────────────────────────────

/// Ensure `<app_dir>/backgrounds/` exists, then pick one image for today
/// (deterministic per ET calendar day) and return it as a data-URL.
pub fn pick_daily_background(app_dir: &Path) -> DailyBackground {
    let dir = app_dir.join("backgrounds");
    let _ = std::fs::create_dir_all(&dir);
    let dir_str = dir.to_string_lossy().to_string();

    // Collect supported image files, sorted for a stable index.
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_file() && is_image(p))
                .collect()
        })
        .unwrap_or_default();
    files.sort();

    if files.is_empty() {
        return DailyBackground { dir: dir_str, file_name: None, data_url: None };
    }

    // Index = ET day number modulo the number of photos → same image all day,
    // rotates day to day.
    let et = crate::time::et_date(crate::time::now());
    let day_num = chrono::NaiveDate::parse_from_str(&et, "%Y-%m-%d")
        .map(|d| chrono::Datelike::num_days_from_ce(&d))
        .unwrap_or(0);
    let idx = (day_num.rem_euclid(files.len() as i32)) as usize;
    let chosen = &files[idx];

    let file_name = chosen.file_name().map(|n| n.to_string_lossy().to_string());
    let data_url = std::fs::read(chosen).ok().map(|bytes| {
        format!("data:{};base64,{}", mime_for(chosen), base64_encode(&bytes))
    });

    DailyBackground { dir: dir_str, file_name, data_url }
}

fn is_image(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("jpg" | "jpeg" | "png" | "webp" | "gif")
    )
}

fn mime_for(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("png")  => "image/png",
        Some("webp") => "image/webp",
        Some("gif")  => "image/gif",
        _            => "image/jpeg",
    }
}

/// Open a folder in the OS file manager (best effort; errors are ignored).
pub fn open_folder(path: &Path) -> Result<(), String> {
    let _ = std::fs::create_dir_all(path);
    let path_str = path.to_string_lossy().to_string();
    #[cfg(target_os = "windows")]
    let cmd = Command::new("explorer").arg(&path_str).spawn();
    #[cfg(target_os = "macos")]
    let cmd = Command::new("open").arg(&path_str).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = Command::new("xdg-open").arg(&path_str).spawn();
    cmd.map(|_| ()).map_err(|e| e.to_string())
}

/// Open a file or folder with the OS default handler (folders → file manager,
/// `.txt` → default editor). Works for both, unlike a bare `explorer <file>`.
fn os_open(path: &Path) -> Result<(), String> {
    let path_str = path.to_string_lossy().to_string();
    #[cfg(target_os = "windows")]
    // `start` is a cmd builtin; the empty "" is the (mandatory) window title.
    let cmd = Command::new("cmd").args(["/C", "start", "", &path_str]).spawn();
    #[cfg(target_os = "macos")]
    let cmd = Command::new("open").arg(&path_str).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = Command::new("xdg-open").arg(&path_str).spawn();
    cmd.map(|_| ()).map_err(|e| e.to_string())
}

// ─── Mood (random image + short / long phrase) ─────────────────────────────────

/// The mood drop targets, created on first access so the "open" actions always
/// have something to open and the expected format is self-documenting. Returns
/// `(images_dir, short_file, long_file)`.
fn mood_paths(app_dir: &Path) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let root = app_dir.join("mood");
    let images = root.join("images");
    let short = root.join("short.txt");
    let long = root.join("long.txt");
    let _ = std::fs::create_dir_all(&images);
    // Seed the txt files with a commented hint so the one-phrase-per-line format is
    // obvious. Lines starting with `#` are ignored when picking.
    if !short.exists() {
        let _ = std::fs::write(
            &short,
            "# Une phrase courte par ligne (ex : less is more)\n",
        );
    }
    if !long.exists() {
        let _ = std::fs::write(
            &long,
            "# Une citation longue par ligne (~50 mots)\n",
        );
    }
    (images, short, long)
}

/// Pick a fresh, random image + short + long phrase from the user's `mood/` folder.
/// Re-randomises on every call (each dashboard open / refresh).
pub fn pick_mood(app_dir: &Path) -> Mood {
    let (images_dir, short_path, long_path) = mood_paths(app_dir);

    // Random image → data-URL.
    let files: Vec<std::path::PathBuf> = std::fs::read_dir(&images_dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_file() && is_image(p))
                .collect()
        })
        .unwrap_or_default();
    let image = files.choose(&mut rand::thread_rng()).and_then(|chosen| {
        let file_name = chosen.file_name()?.to_string_lossy().to_string();
        let bytes = std::fs::read(chosen).ok()?;
        Some(MoodImage {
            file_name,
            data_url: format!("data:{};base64,{}", mime_for(chosen), base64_encode(&bytes)),
        })
    });

    // Pick two distinct short phrases in one go so the Inspiration (03) and H1 (19)
    // cards never show the same line.
    let (short_phrase, heading_phrase) = pick_two_random_lines(&short_path);

    Mood {
        images_dir:   images_dir.to_string_lossy().to_string(),
        short_path:   short_path.to_string_lossy().to_string(),
        long_path:    long_path.to_string_lossy().to_string(),
        image,
        short_phrase,
        heading_phrase,
        long_phrase:  pick_random_line(&long_path),
    }
}

/// Non-empty, non-comment lines from a text file (trimmed).
fn clean_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|content| {
            content
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// One random clean line from a text file.
fn pick_random_line(path: &Path) -> Option<String> {
    clean_lines(path).choose(&mut rand::thread_rng()).cloned()
}

/// Two distinct random clean lines: `(first, second)`. The second is `None` when
/// there is only one line; otherwise it is a different line from the first.
fn pick_two_random_lines(path: &Path) -> (Option<String>, Option<String>) {
    let mut rng = rand::thread_rng();
    let lines = clean_lines(path);
    let mut idxs: Vec<usize> = (0..lines.len()).collect();
    idxs.shuffle(&mut rng);
    let first = idxs.first().map(|&i| lines[i].clone());
    let second = idxs.get(1).map(|&i| lines[i].clone());
    (first, second)
}

/// Open one of the mood drop targets: the images folder, or a phrases `.txt` file.
pub fn open_mood_target(app_dir: &Path, target: &str) -> Result<(), String> {
    let (images_dir, short_path, long_path) = mood_paths(app_dir);
    match target {
        "images" => os_open(&images_dir),
        "short" => os_open(&short_path),
        "long" => os_open(&long_path),
        _ => Err(format!("unknown mood target: {target}")),
    }
}

// ─── Minimal base64 encoder (no crate dependency, matching `screenshot`'s
//     hand-rolled decoder style) ───────────────────────────────────────────────

fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}
