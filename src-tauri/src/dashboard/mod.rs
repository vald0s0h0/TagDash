// Dashboard (moodboard) backend: pull the user's trades from TradeTally (source of
// truth), and pick a deterministic daily background image from the user's folder.
// KPI / series math is done on the frontend from `DashboardTrade`s, so this module
// stays a thin fetch + parse + file layer.

use std::collections::HashSet;
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
    pub tt_id: String,
    pub symbol: Option<String>,
    pub side: Option<String>,
    pub quantity: Option<f64>,
    pub entry_price: Option<f64>,
    pub exit_price: Option<f64>,
    pub pnl: Option<f64>,
    pub pnl_percent: Option<f64>,
    pub entry_date: Option<String>,
    pub exit_date: Option<String>,
    pub commission: Option<f64>,
    pub fees: Option<f64>,
    pub status: Option<String>,
    pub setup: Option<String>,
    pub strategy: Option<String>,
    pub broker: Option<String>,
    pub tags: Vec<String>,
    pub raw: Value,
}

/// The result of one page-through of `GET /api/v1/trades`.
///
/// `complete` is the whole point of this type. The local cache reconciles by
/// deleting every row the fetch did not return, so it may only do that against a
/// snapshot we can *prove* is the entire upstream journal: the page-through ended
/// on TradeTally's own end-of-list signal, and — when it reported a total — we
/// received exactly that many distinct trades. Anything less (a page that 500s
/// halfway, a proxy that truncates, a server that ignores `page`) yields
/// `complete = false`, and the cache then only refreshes what it saw.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeSnapshot {
    pub trades: Vec<DashboardTrade>,
    /// `pagination.total` as reported upstream, when it reports one.
    pub upstream_total: Option<usize>,
    pub complete: bool,
}

/// One TradeTally diary (journal) entry, as returned by `GET /api/diary`.
/// TradeTally keeps at most one entry per calendar day and updates it in place,
/// so the day is the identity — `id` is carried only for reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiaryEntry {
    pub id: Option<String>,
    pub entry_date: String,
    pub title: String,
    pub content: String,
    pub updated_at: Option<String>,
}

/// The folder where the user drops background photos + the image chosen for today
/// (deterministic per ET day), already encoded as a data-URL so the frontend can
/// set it as a `src` directly. Empty folder → no image, just the folder path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyBackground {
    pub dir: String,
    pub file_name: Option<String>,
    pub data_url: Option<String>,
}

/// One random inspiration image, already encoded as a data-URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoodImage {
    pub file_name: String,
    pub data_url: String,
}

/// A fresh, *random* mood pick (unlike `DailyBackground`, which is stable per day):
/// one image + one short phrase + one long phrase, drawn anew on every call so the
/// dashboard shows something different on each open / refresh. Each field is `None`
/// when its folder / file is empty. The paths are returned so the UI can open them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mood {
    pub images_dir: String,
    pub short_path: String,
    pub long_path: String,
    pub image: Option<MoodImage>,
    pub short_phrase: Option<String>,
    /// A second short phrase, distinct from `short_phrase` when the file has ≥2
    /// lines — used by the H1 card so it never echoes the Inspiration card.
    pub heading_phrase: Option<String>,
    pub long_phrase: Option<String>,
}

// ─── TradeTally sync ─────────────────────────────────────────────────────────

/// Page through `GET /api/v1/trades` and return the whole upstream journal, plus
/// whether the page-through can be trusted as complete (see `TradeSnapshot`).
/// Defensive parsing throughout — missing fields are `None`, and a trade with no
/// id is skipped (it can't be keyed).
pub async fn sync_trades(client: &TtClient) -> Result<TradeSnapshot, String> {
    const LIMIT: usize = 100;
    const MAX_PAGES: usize = 500; // safety cap (~50k trades)

    let mut all: Vec<DashboardTrade> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut upstream_total: Option<usize> = None;
    let mut reached_end = false;
    let mut page = 1usize;

    loop {
        let endpoint = format!("/api/v1/trades?page={page}&limit={LIMIT}");
        let resp = client.get_json(&endpoint).await?;

        if upstream_total.is_none() {
            upstream_total = resp
                .pointer("/pagination/total")
                .and_then(|v| v.as_u64())
                .map(|n| n as usize);
        }

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
            reached_end = true;
            break;
        }
        let batch = trades.len();

        // Keep only ids we have not already collected. A server that silently
        // ignores `page` hands back the same rows forever; that shows up here as a
        // page that adds nothing new, and we stop rather than loop to MAX_PAGES
        // and inflate the snapshot with 500 copies of page 1.
        let mut fresh = 0usize;
        for t in &trades {
            if let Some(parsed) = parse_trade(t) {
                if seen.insert(parsed.tt_id.clone()) {
                    all.push(parsed);
                    fresh += 1;
                }
            }
        }
        if fresh == 0 {
            break; // not `reached_end`: pagination is not behaving, don't prune on it
        }

        // Stop on the API's `pagination.hasMore` flag; fall back to a short batch.
        match resp
            .pointer("/pagination/hasMore")
            .and_then(|v| v.as_bool())
        {
            Some(false) => {
                reached_end = true;
                break;
            }
            Some(true) => {}
            None if batch < LIMIT => {
                reached_end = true;
                break;
            }
            None => {}
        }

        page += 1;
        if page > MAX_PAGES {
            break;
        }
    }

    // A reported total we did not match means rows went missing somewhere between
    // TradeTally and here — exactly the case where pruning the cache would delete
    // trades that still exist upstream.
    let complete = reached_end && upstream_total.is_none_or(|total| seen.len() == total);

    Ok(TradeSnapshot {
        trades: all,
        upstream_total,
        complete,
    })
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
        Some(if exit_time.is_some() {
            "Closed".to_string()
        } else {
            "Open".to_string()
        })
    });

    Some(DashboardTrade {
        tt_id,
        symbol: s("symbol"),
        side: s("side"),
        quantity: f("quantity"),
        entry_price: f("entry_price"),
        exit_price: f("exit_price"),
        pnl: f("pnl"),
        pnl_percent: f("pnl_percent"),
        entry_date: s("entry_time").or_else(|| s("trade_date")),
        exit_date: exit_time,
        commission: f("commission"),
        fees: f("fees"),
        status,
        setup: s("setup"),
        strategy: s("strategy"),
        broker: s("broker"),
        tags: parse_tags(v.get("tags")),
        raw: v.clone(),
    })
}

/// Numbers may arrive as JSON numbers or numeric strings — handle both.
fn json_to_f64(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<f64>().ok()))
}

/// Tags can be an array of strings or of `{ name: "…" }` objects.
fn parse_tags(v: Option<&Value>) -> Vec<String> {
    let Some(Value::Array(arr)) = v else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|t| match t {
            Value::String(s) => Some(s.clone()),
            Value::Object(_) => t.get("name").and_then(|n| n.as_str()).map(str::to_string),
            _ => None,
        })
        .collect()
}

// ─── Daily background ────────────────────────────────────────────────────────

/// Ensure `<root>/user/backgrounds/` exists, then pick one image for today
/// (deterministic per ET calendar day) and return it as a data-URL.
pub fn pick_daily_background(app_dir: &Path) -> DailyBackground {
    let dir = crate::paths::backgrounds_dir(app_dir);
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
        return DailyBackground {
            dir: dir_str,
            file_name: None,
            data_url: None,
        };
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
    let data_url = std::fs::read(chosen)
        .ok()
        .map(|bytes| format!("data:{};base64,{}", mime_for(chosen), base64_encode(&bytes)));

    DailyBackground {
        dir: dir_str,
        file_name,
        data_url,
    }
}

fn is_image(p: &Path) -> bool {
    matches!(
        p.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("jpg" | "jpeg" | "png" | "webp" | "gif" | "avif")
    )
}

fn mime_for(p: &Path) -> &'static str {
    match p
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("avif") => "image/avif",
        _ => "image/jpeg",
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
    let cmd = Command::new("cmd")
        .args(["/C", "start", "", &path_str])
        .spawn();
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
pub(crate) fn mood_paths(app_dir: &Path) -> (std::path::PathBuf, std::path::PathBuf, std::path::PathBuf) {
    let root = crate::paths::mood_dir(app_dir);
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
        let _ = std::fs::write(&long, "# Une citation longue par ligne (~50 mots)\n");
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
        images_dir: images_dir.to_string_lossy().to_string(),
        short_path: short_path.to_string_lossy().to_string(),
        long_path: long_path.to_string_lossy().to_string(),
        image,
        short_phrase,
        heading_phrase,
        long_phrase: pick_random_line(&long_path),
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

// ─── Dashboard assets, as Réglages ▸ Tableau de bord shows and edits them ───────

/// What feeds the dashboard: how many backgrounds and mood images there are, and
/// the two phrase files as text.
#[derive(Debug, Clone, Serialize)]
pub struct DashboardAssets {
    pub backgrounds: usize,
    pub mood_images: usize,
    /// Content of `short.txt` / `long.txt` (one phrase per line, `#` = comment).
    pub short_phrases: String,
    pub long_phrases: String,
}

fn image_count(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_file() && is_image(p)).count())
        .unwrap_or(0)
}

pub fn assets(app_dir: &Path) -> DashboardAssets {
    let (images_dir, short_path, long_path) = mood_paths(app_dir);
    DashboardAssets {
        backgrounds: image_count(&crate::paths::backgrounds_dir(app_dir)),
        mood_images: image_count(&images_dir),
        short_phrases: std::fs::read_to_string(short_path).unwrap_or_default(),
        long_phrases: std::fs::read_to_string(long_path).unwrap_or_default(),
    }
}

/// Replace a phrase file (`"short"` | `"long"`) with `content`.
pub fn save_phrases(app_dir: &Path, kind: &str, content: &str) -> Result<(), String> {
    let (_, short_path, long_path) = mood_paths(app_dir);
    let path = match kind {
        "short" => short_path,
        "long" => long_path,
        _ => return Err(format!("unknown phrase file: {kind}")),
    };
    let mut text = content.replace("\r\n", "\n");
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    std::fs::write(path, text).map_err(|e| e.to_string())
}

// ─── Bundled default assets (shipped with the app, seeded once on first run) ───

/// Copy the bundled default mood images / phrase files / backgrounds into the
/// user's app-data, WITHOUT overwriting anything they already have. New users get
/// the maintainer's curated set out of the box; existing users keep their own
/// files (and any later edit/deletion sticks — this is gated by a one-time marker
/// in the caller). `resources` is the resolved `resources/defaults` directory.
pub fn seed_default_assets(resources: &Path, app_dir: &Path) {
    let mood = crate::paths::mood_dir(app_dir);
    copy_dir_if_absent(&resources.join("mood").join("images"), &mood.join("images"));
    copy_dir_if_absent(
        &resources.join("backgrounds"),
        &crate::paths::backgrounds_dir(app_dir),
    );
    for name in ["short.txt", "long.txt"] {
        copy_file_if_absent(&resources.join("mood").join(name), &mood.join(name));
    }
}

fn copy_dir_if_absent(src: &Path, dst: &Path) {
    let Ok(entries) = std::fs::read_dir(src) else {
        return;
    };
    let _ = std::fs::create_dir_all(dst);
    for e in entries.flatten() {
        let p = e.path();
        if p.is_file() {
            if let Some(name) = p.file_name() {
                copy_file_if_absent(&p, &dst.join(name));
            }
        }
    }
}

fn copy_file_if_absent(src: &Path, dst: &Path) {
    if dst.exists() || !src.exists() {
        return;
    }
    if let Some(parent) = dst.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::copy(src, dst);
}

/// The bundled default dashboard layout JSON (shipped resource), or None if absent.
/// Used by the frontend to seed a brand-new user's board with the maintainer's
/// arrangement; users with a saved layout keep theirs.
pub fn read_default_dashboard(resources: &Path) -> Option<String> {
    std::fs::read_to_string(resources.join("dashboard.json")).ok()
}

/// Persist the current dashboard layout to `<root>/user/dashboard-default.json` so
/// the maintainer can capture it and bundle it as the new shipped default. Returns
/// the file written.
pub fn write_dashboard_export(app_dir: &Path, json: &str) -> Result<std::path::PathBuf, String> {
    let path = crate::paths::user(app_dir).join("dashboard-default.json");
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(path)
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
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
