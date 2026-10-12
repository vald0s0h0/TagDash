// The data pack: one file holding everything personal, to carry from one machine to
// another. Nothing is sent anywhere — the user moves the file however they like.
//
// FORMAT. A tar archive under zstd (`.tagdashpack`). First entry `manifest.json`
// (so a pack can be described without unpacking it), then the content of `user/`
// as it sits on disk, with `user.db` replaced by a consistent snapshot of the live
// database. `tar --zstd -tf <pack>` lists one from a terminal.
//
// EXPORT never touches the live data: the database is snapshotted with
// `VACUUM … INTO`, the files are read.
//
// IMPORT never touches it either — not directly. A pack is unpacked and checked
// into `user.incoming/`; the swap happens at the next launch, before anything is
// open (`layout::prepare`), and the data it replaces is kept under `backups/`.
// Replacing a database under a running app is how one gets two half-truths.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use super::layout::READY_MARKER;
use crate::local_db::schema::USER_SCHEMA_VERSION;
use crate::paths;

pub const PACK_EXTENSION: &str = "tagdashpack";
const FORMAT: &str = "tagdash-data-pack";
const FORMAT_VERSION: u32 = 1;
const MANIFEST_NAME: &str = "manifest.json";
/// A pack's manifest is a few hundred bytes; anything larger is not one.
const MANIFEST_MAX: u64 = 256 * 1024;

/// What a pack says about itself.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub format: String,
    pub format_version: u32,
    /// `USER_SCHEMA_VERSION` of the build that wrote it.
    pub schema_version: u32,
    pub app_version: String,
    /// UTC, RFC 3339.
    pub created_at: String,
    pub machine: String,
    pub os: String,
    pub includes_secrets: bool,
    #[serde(default)]
    pub counts: Counts,
    /// Files packed and their size before compression.
    #[serde(default)]
    pub files: u64,
    #[serde(default)]
    pub bytes: u64,
}

/// Headline numbers, so two packs — or a pack and this machine — can be told apart
/// at a glance before one replaces the other.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Counts {
    #[serde(default)]
    pub trades: i64,
    #[serde(default)]
    pub executions: i64,
    #[serde(default)]
    pub journal_notes: i64,
    #[serde(default)]
    pub review_sessions: i64,
    #[serde(default)]
    pub voice_notes: i64,
    #[serde(default)]
    pub screenshots: i64,
    #[serde(default)]
    pub drawings: i64,
    #[serde(default)]
    pub usage_events: i64,
    /// Outbound events not delivered yet (TradeTally queue + review outbox).
    #[serde(default)]
    pub pending_sync: i64,
    /// Most recent trade execution, as stored.
    #[serde(default)]
    pub last_trade_at: Option<String>,
}

/// Count the personal tables through `conn`. Table names are unqualified on
/// purpose: this runs on the app's connection (where they resolve to `user`) and
/// on a standalone snapshot (where they are the main schema).
pub fn counts(conn: &Connection) -> Counts {
    let n = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0);
    Counts {
        trades: n("SELECT COUNT(DISTINCT trade_id) FROM executions"),
        executions: n("SELECT COUNT(*) FROM executions"),
        journal_notes: n("SELECT COUNT(*) FROM journal_entries WHERE TRIM(notes) != ''"),
        review_sessions: n("SELECT COUNT(*) FROM review_sessions"),
        voice_notes: n("SELECT COUNT(*) FROM review_voice"),
        screenshots: n("SELECT COUNT(*) FROM screenshot_files"),
        drawings: n("SELECT COUNT(*) FROM chart_drawings"),
        usage_events: n("SELECT COUNT(*) FROM usage_events"),
        pending_sync: n("SELECT COUNT(*) FROM tradetally_sync_queue WHERE status = 'pending'")
            + n("SELECT COUNT(*) FROM review_outbox"),
        last_trade_at: conn
            .query_row("SELECT MAX(filled_at) FROM executions", [], |r| r.get::<_, Option<String>>(0))
            .ok()
            .flatten(),
    }
}

/// The name the user gave this computer — what tells two packs apart. The network
/// host name is only the fallback: on a Mac without one it is the machine's IP.
pub fn machine_name() -> String {
    #[cfg(target_os = "macos")]
    let given = std::process::Command::new("scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string());
    #[cfg(target_os = "windows")]
    let given = std::env::var("COMPUTERNAME").ok();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let given: Option<String> = None;

    given
        .filter(|name| !name.is_empty())
        .or_else(|| sysinfo::System::host_name().map(|h| h.trim_end_matches(".local").to_string()))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "machine".into())
}

/// Files of `user/` that never go in a pack: the live database (a snapshot goes
/// instead), SQLite's write-ahead files (folded into the snapshot of their
/// database — see `snapshot_store`), OS litter, half-written files, the folder note.
pub(super) fn skipped(rel: &str, include_secrets: bool) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    matches!(rel, "user.db" | "LISEZMOI.txt")
        || (rel == "secrets.toml" && !include_secrets)
        || matches!(name, ".DS_Store" | "Thumbs.db" | "desktop.ini")
        || name == READY_MARKER
        || name.ends_with(".db-wal")
        || name.ends_with(".db-shm")
        || name.ends_with(".tmp")
        || name.ends_with(".part")
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// A review session is its own SQLite file in WAL mode, and part of it may still
/// sit in the write-ahead log — always, for a session being recorded right now.
/// Packing the `.db` alone would drop those rows; packing `.db` + `-wal` copies two
/// files that change under the copy. So a store with a live log is packed as a
/// snapshot taken through SQLite: one file, consistent, log folded in.
/// `None` when the store has no pending log (the file on disk is already whole).
fn snapshot_store(store: &Path, into: &Path) -> Option<Result<(), String>> {
    let pending = std::fs::metadata(sibling(store, "-wal")).map(|m| m.len() > 0).unwrap_or(false);
    if !pending {
        return None;
    }
    let take = || -> rusqlite::Result<()> {
        let conn = Connection::open_with_flags(store, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute("VACUUM INTO ?1", rusqlite::params![into.to_string_lossy()])?;
        Ok(())
    };
    Some(take().map_err(|e| e.to_string()))
}

/// Every file under `dir`, as `(absolute, relative with forward slashes)`, sorted.
pub(super) fn walk(dir: &Path) -> Vec<(PathBuf, String)> {
    fn rec(base: &Path, dir: &Path, out: &mut Vec<(PathBuf, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                rec(base, &path, out);
            } else if kind.is_file() {
                if let Ok(rel) = path.strip_prefix(base) {
                    out.push((path.clone(), rel.to_string_lossy().replace('\\', "/")));
                }
            }
        }
    }
    let mut out = Vec::new();
    rec(dir, dir, &mut out);
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

// ─── Export ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct Exported {
    pub path: String,
    /// Size of the pack file on disk.
    pub pack_bytes: u64,
    pub manifest: Manifest,
}

/// Write a pack of this machine's personal data into `dest_dir`.
pub fn export(
    root: &Path,
    db: &Mutex<Connection>,
    dest_dir: &Path,
    include_secrets: bool,
) -> Result<Exported, String> {
    let e = |err: std::io::Error| err.to_string();
    let now = chrono::Utc::now();
    let work = root.join(format!(".export-{}", now.format("%Y%m%d-%H%M%S%3f")));
    std::fs::create_dir_all(&work).map_err(e)?;
    let result = export_into(root, db, dest_dir, include_secrets, &work, now);
    let _ = std::fs::remove_dir_all(&work);
    result
}

fn export_into(
    root: &Path,
    db: &Mutex<Connection>,
    dest_dir: &Path,
    include_secrets: bool,
    work: &Path,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Exported, String> {
    let e = |err: std::io::Error| err.to_string();

    // A consistent copy of the live database, WAL included, in one file.
    let snapshot = work.join("user.db");
    {
        let conn = db.lock().unwrap();
        conn.execute("VACUUM user INTO ?1", rusqlite::params![snapshot.to_string_lossy()])
            .map_err(|err| format!("instantané de la base : {err}"))?;
    }
    let counts = {
        let snap = Connection::open_with_flags(&snapshot, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|err| err.to_string())?;
        counts(&snap)
    };

    let user = paths::user(root);
    let mut files: Vec<(PathBuf, String)> =
        walk(&user).into_iter().filter(|(_, rel)| !skipped(rel, include_secrets)).collect();
    for (n, (path, rel)) in files.iter_mut().enumerate() {
        if !rel.ends_with(".db") {
            continue;
        }
        let copy = work.join(format!("store-{n}.db"));
        match snapshot_store(path, &copy) {
            None => {}
            Some(Ok(())) => *path = copy,
            Some(Err(reason)) => return Err(format!("session {rel} illisible : {reason}")),
        }
    }
    files.insert(0, (snapshot.clone(), "user.db".to_string()));
    let bytes: u64 = files.iter().filter_map(|(p, _)| std::fs::metadata(p).ok()).map(|m| m.len()).sum();

    let manifest = Manifest {
        format: FORMAT.into(),
        format_version: FORMAT_VERSION,
        schema_version: USER_SCHEMA_VERSION,
        app_version: env!("CARGO_PKG_VERSION").into(),
        created_at: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        machine: machine_name(),
        os: std::env::consts::OS.into(),
        includes_secrets: include_secrets && files.iter().any(|(_, rel)| rel == "secrets.toml"),
        counts,
        files: files.len() as u64,
        bytes,
    };

    std::fs::create_dir_all(dest_dir).map_err(e)?;
    let stamp = now.with_timezone(&chrono::Local).format("%Y-%m-%d-%H%M");
    let safe_machine: String = manifest
        .machine
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    let dest = dest_dir.join(format!("TagDash-{safe_machine}-{stamp}.{PACK_EXTENSION}"));
    let part = dest.with_extension(format!("{PACK_EXTENSION}.part"));

    let write = || -> std::io::Result<()> {
        let mut encoder = zstd::Encoder::new(File::create(&part)?, 3)?;
        encoder.include_checksum(true)?;
        let mut tar = tar::Builder::new(encoder);
        let json = serde_json::to_vec_pretty(&manifest).map_err(std::io::Error::other)?;
        let mut header = tar::Header::new_gnu();
        header.set_size(json.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(now.timestamp().max(0) as u64);
        header.set_cksum();
        tar.append_data(&mut header, MANIFEST_NAME, json.as_slice())?;
        for (path, rel) in &files {
            tar.append_path_with_name(path, rel)?;
        }
        tar.into_inner()?.finish()?.sync_all()
    };
    if let Err(err) = write() {
        let _ = std::fs::remove_file(&part);
        return Err(format!("écriture du pack : {err}"));
    }
    std::fs::rename(&part, &dest).map_err(e)?;
    let pack_bytes = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    Ok(Exported { path: dest.to_string_lossy().to_string(), pack_bytes, manifest })
}

// ─── Inspect ────────────────────────────────────────────────────────────────────

fn open_archive(pack: &Path) -> Result<tar::Archive<zstd::Decoder<'static, std::io::BufReader<File>>>, String> {
    let file = File::open(pack).map_err(|e| format!("pack illisible : {e}"))?;
    let decoder = zstd::Decoder::new(file).map_err(|e| format!("pack illisible : {e}"))?;
    Ok(tar::Archive::new(decoder))
}

/// Why this build cannot take the pack, if it cannot.
fn refusal(m: &Manifest) -> Option<String> {
    if m.format != FORMAT {
        return Some("ce fichier n'est pas un pack de données TagDash".into());
    }
    if m.format_version > FORMAT_VERSION || m.schema_version > USER_SCHEMA_VERSION {
        return Some(format!(
            "ce pack vient d'une version plus récente de TagDash ({}) — mets cette machine à jour d'abord",
            m.app_version
        ));
    }
    None
}

/// Read a pack's manifest without unpacking it.
pub fn inspect(pack: &Path) -> Result<Manifest, String> {
    let mut archive = open_archive(pack)?;
    let mut entries = archive.entries().map_err(|e| format!("pack illisible : {e}"))?;
    let not_a_pack = || "ce fichier n'est pas un pack de données TagDash".to_string();
    let mut first = entries.next().ok_or_else(not_a_pack)?.map_err(|_| not_a_pack())?;
    let is_manifest = first.path().map(|p| &*p == Path::new(MANIFEST_NAME)).unwrap_or(false);
    if !is_manifest || first.size() > MANIFEST_MAX {
        return Err(not_a_pack());
    }
    let mut json = String::new();
    first.read_to_string(&mut json).map_err(|_| not_a_pack())?;
    let manifest: Manifest = serde_json::from_str(&json).map_err(|_| not_a_pack())?;
    match refusal(&manifest) {
        Some(reason) => Err(reason),
        None => Ok(manifest),
    }
}

/// A pack found on disk, described.
#[derive(Debug, Clone, Serialize)]
pub struct FoundPack {
    pub path: String,
    pub file_name: String,
    pub pack_bytes: u64,
    pub manifest: Manifest,
}

pub fn describe(pack: &Path) -> Result<FoundPack, String> {
    let manifest = inspect(pack)?;
    Ok(FoundPack {
        path: pack.to_string_lossy().to_string(),
        file_name: pack.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        pack_bytes: std::fs::metadata(pack).map(|m| m.len()).unwrap_or(0),
        manifest,
    })
}

/// Packs sitting where a download or a copy usually lands, newest first.
pub fn find_packs() -> Vec<FoundPack> {
    let mut found: Vec<FoundPack> = [dirs::download_dir(), dirs::desktop_dir()]
        .into_iter()
        .flatten()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flat_map(|entries| entries.flatten())
        .map(|entry| entry.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some(PACK_EXTENSION))
        .filter_map(|p| describe(&p).ok())
        .collect();
    found.sort_by(|a, b| b.manifest.created_at.cmp(&a.manifest.created_at));
    found.truncate(12);
    found
}

// ─── Import (staging) ───────────────────────────────────────────────────────────

/// Unpack `pack` into `user.incoming/` and check it. It replaces the personal data
/// at the next launch; until then nothing of the running app has changed, and
/// `cancel_staged` undoes it.
pub fn stage(root: &Path, pack: &Path) -> Result<Manifest, String> {
    let manifest = inspect(pack)?;
    let incoming = paths::incoming_dir(root);
    let _ = std::fs::remove_dir_all(&incoming);
    std::fs::create_dir_all(&incoming).map_err(|e| e.to_string())?;
    let staged = unpack(pack, &incoming).and_then(|()| verify(&incoming));
    if let Err(reason) = staged {
        let _ = std::fs::remove_dir_all(&incoming);
        return Err(reason);
    }
    // Written last: its presence is what says "complete and verified".
    let json = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    std::fs::write(incoming.join(READY_MARKER), json).map_err(|e| e.to_string())?;
    Ok(manifest)
}

fn unpack(pack: &Path, into: &Path) -> Result<(), String> {
    let e = |err: std::io::Error| format!("pack endommagé : {err}");
    let mut archive = open_archive(pack)?;
    for entry in archive.entries().map_err(e)? {
        let mut entry = entry.map_err(e)?;
        // Plain files and folders only — a pack has no business carrying a link.
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            continue;
        }
        let path = entry.path().map_err(e)?.into_owned();
        if path == Path::new(MANIFEST_NAME) || path.file_name().is_some_and(|n| n == READY_MARKER) {
            continue;
        }
        // `unpack_in` refuses any path that would land outside `into`.
        if !entry.unpack_in(into).map_err(e)? {
            return Err("pack refusé : il contient un chemin hors du dossier de données".into());
        }
    }
    Ok(())
}

fn verify(incoming: &Path) -> Result<(), String> {
    let db = incoming.join("user.db");
    if !db.is_file() {
        return Err("pack incomplet : base personnelle absente".into());
    }
    let conn = Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("base du pack illisible : {e}"))?;
    let check: String = conn
        .query_row("PRAGMA integrity_check", [], |r| r.get(0))
        .map_err(|e| format!("base du pack illisible : {e}"))?;
    if check != "ok" {
        return Err(format!("base du pack endommagée : {check}"));
    }
    Ok(())
}

/// The pack waiting for the next launch, if any.
pub fn staged(root: &Path) -> Option<Manifest> {
    let json = std::fs::read_to_string(paths::incoming_dir(root).join(READY_MARKER)).ok()?;
    serde_json::from_str(&json).ok()
}

pub fn cancel_staged(root: &Path) -> Result<(), String> {
    let incoming = paths::incoming_dir(root);
    if incoming.exists() {
        std::fs::remove_dir_all(&incoming).map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ─── Backups (data set aside by an import) ──────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct Backup {
    /// Folder name under `backups/` — `user-YYYYMMDD-HHMMSS`.
    pub name: String,
    /// Local time the data was set aside, `YYYY-MM-DD HH:MM`.
    pub set_aside_at: String,
    pub bytes: u64,
    pub counts: Counts,
}

/// `user-YYYYMMDD-HHMMSS` and nothing else: a backup name comes back from the
/// frontend and is joined onto a path.
fn backup_stamp(name: &str) -> Option<chrono::NaiveDateTime> {
    chrono::NaiveDateTime::parse_from_str(name.strip_prefix("user-")?, "%Y%m%d-%H%M%S").ok()
}

fn backup_dir(root: &Path, name: &str) -> Result<PathBuf, String> {
    backup_stamp(name).ok_or("sauvegarde inconnue")?;
    let dir = paths::backups_dir(root).join(name);
    if !dir.is_dir() {
        return Err("sauvegarde introuvable".into());
    }
    Ok(dir)
}

pub fn list_backups(root: &Path) -> Vec<Backup> {
    let mut out: Vec<Backup> = std::fs::read_dir(paths::backups_dir(root))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            let stamp = backup_stamp(&name)?;
            let dir = entry.path();
            let counts = Connection::open_with_flags(
                dir.join("user.db"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .map(|conn| counts(&conn))
            .unwrap_or_default();
            Some(Backup {
                name,
                set_aside_at: stamp.format("%Y-%m-%d %H:%M").to_string(),
                bytes: super::dir_bytes(&dir),
                counts,
            })
        })
        .collect();
    out.sort_by(|a, b| b.name.cmp(&a.name));
    out
}

/// Stage a backup as if it were an imported pack: it becomes the personal data at
/// the next launch, and what it replaces is set aside in turn.
pub fn stage_backup(root: &Path, name: &str) -> Result<(), String> {
    let dir = backup_dir(root, name)?;
    verify(&dir)?;
    let incoming = paths::incoming_dir(root);
    let _ = std::fs::remove_dir_all(&incoming);
    std::fs::rename(&dir, &incoming).map_err(|e| e.to_string())?;
    let conn = Connection::open_with_flags(incoming.join("user.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| e.to_string())?;
    let stamp = backup_stamp(name).map(|s| s.and_local_timezone(chrono::Local).single());
    let manifest = Manifest {
        format: FORMAT.into(),
        format_version: FORMAT_VERSION,
        schema_version: USER_SCHEMA_VERSION,
        app_version: env!("CARGO_PKG_VERSION").into(),
        created_at: stamp
            .flatten()
            .map(|t| t.with_timezone(&chrono::Utc))
            .unwrap_or_else(chrono::Utc::now)
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        machine: format!("{} (sauvegarde)", machine_name()),
        os: std::env::consts::OS.into(),
        includes_secrets: incoming.join("secrets.toml").exists(),
        counts: counts(&conn),
        files: 0,
        bytes: super::dir_bytes(&incoming),
    };
    drop(conn);
    let json = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
    std::fs::write(incoming.join(READY_MARKER), json).map_err(|e| e.to_string())
}

pub fn delete_backup(root: &Path, name: &str) -> Result<(), String> {
    std::fs::remove_dir_all(backup_dir(root, name)?).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::layout;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tagdash-pack-{name}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, content: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// A machine with a little of everything personal on it.
    fn machine(name: &str, note: &str) -> (PathBuf, Mutex<Connection>) {
        let root = scratch(name);
        layout::prepare(&root).unwrap();
        write(&paths::settings_file(&root), format!("# settings of {name}\n").as_bytes());
        write(&paths::secrets_file(&root), b"massive_api_key = \"secret\"\n");
        write(&paths::mood_dir(&root).join("short.txt"), b"less is more\n");
        write(&paths::backgrounds_dir(&root).join("a.jpg"), &[0xFF, 0xD8, 0xFF, 0x00, 0x01]);
        write(&paths::reviews_dir(&root).join("2026-06-12/v1.ulaw"), &[1, 2, 3]);
        write(&paths::screenshots_dir(&root).join("s1.png"), b"png");
        // Cache: must never end up in a pack.
        write(&paths::flat_files_dir(&root).join("daily/daily.db"), b"bars");
        let conn = crate::local_db::open_and_migrate(&root).unwrap();
        conn.execute_batch(&format!(
            "INSERT INTO executions (fill_id, trade_id, symbol, quantity, fill_price, filled_at)
                  VALUES ('f1', 't1', 'AAPL', 100, 1.5, '2026-06-12T13:31:00Z');
             INSERT INTO journal_entries (trade_id, symbol, notes) VALUES ('t1', 'AAPL', '{note}');
             INSERT INTO ui_state (key, value) VALUES ('tagdash-dashboard', '{{\"state\":{{}}}}');
             INSERT INTO daily_cache (symbol, date, close) VALUES ('AAPL', '2026-06-12', 2.0);"
        ))
        .unwrap();
        crate::local_db::cache_repository::set_app_meta(&conn, "strategy_overrides", "{\"apex\":true}").unwrap();
        (root, Mutex::new(conn))
    }

    fn entry_names(pack: &Path) -> Vec<String> {
        let mut archive = open_archive(pack).unwrap();
        archive
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_string_lossy().to_string())
            .collect()
    }

    #[test]
    fn a_pack_carries_the_personal_data_from_one_machine_to_another() {
        let (mac, mac_db) = machine("mac", "belle sortie");
        let (pc, pc_db) = machine("pc", "ancienne note du PC");
        let out = scratch("out");

        let exported = export(&mac, &mac_db, &out, true).unwrap();
        let pack = PathBuf::from(&exported.path);
        assert!(pack.extension().is_some_and(|x| x == PACK_EXTENSION));
        assert_eq!(exported.manifest.counts.trades, 1);
        assert_eq!(exported.manifest.counts.journal_notes, 1);
        assert_eq!(inspect(&pack).unwrap(), exported.manifest);

        // Staging changes nothing the running app can see.
        let staged_manifest = stage(&pc, &pack).unwrap();
        assert_eq!(staged(&pc), Some(staged_manifest));
        let note = |db: &Mutex<Connection>| -> String {
            db.lock().unwrap().query_row("SELECT notes FROM journal_entries", [], |r| r.get(0)).unwrap()
        };
        assert_eq!(note(&pc_db), "ancienne note du PC");

        // Next launch on the PC.
        drop(pc_db);
        let report = layout::prepare(&pc).unwrap();
        let backup = report.pack_installed.expect("installed");
        let pc_db = Mutex::new(crate::local_db::open_and_migrate(&pc).unwrap());
        assert_eq!(note(&pc_db), "belle sortie");
        {
            let conn = pc_db.lock().unwrap();
            assert_eq!(
                crate::local_db::cache_repository::get_app_meta(&conn, "strategy_overrides").as_deref(),
                Some("{\"apex\":true}")
            );
            let ui: String =
                conn.query_row("SELECT value FROM ui_state WHERE key = 'tagdash-dashboard'", [], |r| r.get(0)).unwrap();
            assert!(ui.contains("state"));
            // The PC's own cache was not touched by the import.
            let bars: i64 = conn.query_row("SELECT COUNT(*) FROM daily_cache", [], |r| r.get(0)).unwrap();
            assert_eq!(bars, 1);
        }
        assert!(std::fs::read_to_string(paths::settings_file(&pc)).unwrap().contains("settings of mac"));
        assert_eq!(std::fs::read(paths::reviews_dir(&pc).join("2026-06-12/v1.ulaw")).unwrap(), [1, 2, 3]);
        assert!(paths::backgrounds_dir(&pc).join("a.jpg").exists());

        // What the PC had is whole under backups/, and listed.
        let backups = list_backups(&pc);
        assert_eq!(backups.len(), 1);
        assert_eq!(backups[0].name, backup);
        assert_eq!(backups[0].counts.journal_notes, 1);

        for dir in [mac, pc, out] {
            let _ = std::fs::remove_dir_all(dir);
        }
        drop(mac_db);
    }

    #[test]
    fn a_pack_holds_no_cache_and_no_secrets_unless_asked() {
        let (root, db) = machine("secrets", "note");
        let out = scratch("out2");
        let without = export(&root, &db, &out, false).unwrap();
        assert!(!without.manifest.includes_secrets);
        let names = entry_names(Path::new(&without.path));
        assert_eq!(names[0], MANIFEST_NAME);
        assert!(names.contains(&"user.db".to_string()));
        assert!(names.contains(&"settings.toml".to_string()));
        assert!(names.contains(&"reviews/2026-06-12/v1.ulaw".to_string()));
        assert!(!names.contains(&"secrets.toml".to_string()));
        assert!(!names.iter().any(|n| n.contains("flat_files") || n.contains("cache") || n.ends_with("-wal")));
        std::fs::remove_file(&without.path).unwrap();

        let with = export(&root, &db, &out, true).unwrap();
        assert!(with.manifest.includes_secrets);
        assert!(entry_names(Path::new(&with.path)).contains(&"secrets.toml".to_string()));
        // No work folder left behind at the root.
        let litter: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with(".export-"))
            .collect();
        assert!(litter.is_empty(), "{litter:?}");
        for dir in [root, out] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn a_session_still_in_its_write_ahead_log_is_packed_whole() {
        let (root, db) = machine("wal", "note");
        // A session store as the recorder leaves it mid-session: rows committed,
        // still in the log, the connection open.
        let store = paths::reviews_dir(&root).join("2026-06-12/RV-LIVE.db");
        let live = Connection::open(&store).unwrap();
        live.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA wal_autocheckpoint = 0;
             CREATE TABLE trades (ts INTEGER NOT NULL, price REAL NOT NULL);
             INSERT INTO trades VALUES (1, 2.5), (2, 2.6), (3, 2.7);",
        )
        .unwrap();
        assert!(std::fs::metadata(sibling(&store, "-wal")).unwrap().len() > 0);

        let out = scratch("out-wal");
        let pack = export(&root, &db, &out, false).unwrap().path;
        let names = entry_names(Path::new(&pack));
        assert!(names.contains(&"reviews/2026-06-12/RV-LIVE.db".to_string()));
        assert!(!names.iter().any(|n| n.ends_with("-wal") || n.ends_with("-shm")), "{names:?}");

        // On the other side the session is one file holding every row.
        let other = scratch("other-wal");
        layout::prepare(&other).unwrap();
        stage(&other, Path::new(&pack)).unwrap();
        layout::prepare(&other).unwrap();
        let arrived = Connection::open(paths::reviews_dir(&other).join("2026-06-12/RV-LIVE.db")).unwrap();
        let rows: i64 = arrived.query_row("SELECT COUNT(*) FROM trades", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 3);
        drop(live);
        for dir in [root, out, other] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn a_file_that_is_not_a_pack_is_refused_and_nothing_is_staged() {
        let (root, _db) = machine("garbage", "note");
        let fake = root.join("fake.tagdashpack");
        std::fs::write(&fake, b"definitely not a pack").unwrap();
        assert!(inspect(&fake).is_err());
        assert!(stage(&root, &fake).is_err());
        assert!(staged(&root).is_none());
        assert!(!paths::incoming_dir(&root).exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_pack_from_a_newer_schema_is_refused() {
        let m = Manifest {
            format: FORMAT.into(),
            format_version: FORMAT_VERSION,
            schema_version: USER_SCHEMA_VERSION + 1,
            app_version: "9.9.9".into(),
            created_at: "2026-10-12T12:00:00Z".into(),
            machine: "m".into(),
            os: "macos".into(),
            includes_secrets: false,
            counts: Counts::default(),
            files: 0,
            bytes: 0,
        };
        assert!(refusal(&m).unwrap().contains("plus récente"));
        assert!(refusal(&Manifest { schema_version: USER_SCHEMA_VERSION, ..m }).is_none());
    }

    #[test]
    fn a_backup_can_be_put_back() {
        let (mac, mac_db) = machine("mac2", "note du mac");
        let (pc, pc_db) = machine("pc2", "note du pc");
        let out = scratch("out3");
        let pack = export(&mac, &mac_db, &out, false).unwrap().path;
        stage(&pc, Path::new(&pack)).unwrap();
        drop(pc_db);
        let backup = layout::prepare(&pc).unwrap().pack_installed.unwrap();

        // A name that is not a backup's never reaches the file system.
        assert!(stage_backup(&pc, "../user").is_err());
        assert!(delete_backup(&pc, "user").is_err());

        stage_backup(&pc, &backup).unwrap();
        layout::prepare(&pc).unwrap();
        let conn = crate::local_db::open_and_migrate(&pc).unwrap();
        let note: String = conn.query_row("SELECT notes FROM journal_entries", [], |r| r.get(0)).unwrap();
        assert_eq!(note, "note du pc");
        // The pack without secrets left the PC's own in place both ways.
        assert!(paths::secrets_file(&pc).exists());
        drop(mac_db);
        for dir in [mac, pc, out] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}
