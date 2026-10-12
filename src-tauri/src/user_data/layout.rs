// What happens to the data root BEFORE anything is opened, on every launch:
//
//   1. a data pack staged by an import becomes `user/` (the data it replaces is
//      set aside under `backups/`, never deleted);
//   2. a root laid out by a build from before the separation — everything side by
//      side in one folder — is sorted into `user/` and `cache/`.
//
// Both are renames inside one folder tree: instant, whatever the size (10 GB of
// flat files move as fast as a settings file), and each one atomic. A launch
// interrupted half-way resumes where it stopped — an item already moved is simply
// no longer at its old place.

use std::path::{Path, PathBuf};

use crate::paths;

/// Marker written last by an import: the staged folder is complete and verified.
pub const READY_MARKER: &str = ".ready";

#[derive(Debug, Default, Clone)]
pub struct LayoutReport {
    /// Items moved out of the pre-separation layout (their old names).
    pub moved: Vec<String>,
    /// (item, reason) for the ones that could not be moved. Retried next launch.
    pub failed: Vec<(String, String)>,
    /// Name of the backup holding the data a staged pack just replaced.
    pub pack_installed: Option<String>,
}

impl LayoutReport {
    /// One line per fact worth keeping in the app log; empty on an ordinary launch.
    pub fn log_lines(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        if let Some(backup) = &self.pack_installed {
            out.push((
                "info",
                format!("Pack de données installé — données précédentes conservées dans backups/{backup}"),
            ));
        }
        if !self.moved.is_empty() {
            out.push((
                "info",
                format!("Dossier de données réorganisé (user/ + cache/) : {}", self.moved.join(", ")),
            ));
        }
        for (item, reason) in &self.failed {
            out.push(("warn", format!("Réorganisation : {item} non déplacé — {reason}")));
        }
        out
    }
}

fn timestamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

const ALREADY_THERE: &str = "la destination existe déjà";

fn is_empty_dir(path: &Path) -> bool {
    path.is_dir() && std::fs::read_dir(path).map(|mut d| d.next().is_none()).unwrap_or(false)
}

/// Rename `src` to `dst`. `Ok(false)` when there is nothing to move. A destination
/// that already holds something is never overwritten.
fn move_item(src: &Path, dst: &Path) -> Result<bool, String> {
    if !src.exists() {
        return Ok(false);
    }
    if is_empty_dir(dst) {
        let _ = std::fs::remove_dir(dst);
    }
    if dst.exists() {
        return Err(ALREADY_THERE.into());
    }
    if let Some(dir) = dst.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::rename(src, dst).map_err(|e| e.to_string())?;
    Ok(true)
}

// ─── 1. Staged pack ─────────────────────────────────────────────────────────────

/// Put a staged pack in place. The current personal data is renamed into
/// `backups/user-<date>/` first — an import is always reversible from Réglages.
fn install_incoming(root: &Path, report: &mut LayoutReport) {
    let incoming = paths::incoming_dir(root);
    if !incoming.exists() {
        return;
    }
    if !incoming.join(READY_MARKER).exists() {
        // An extraction that never finished: not data, just debris.
        let _ = std::fs::remove_dir_all(&incoming);
        return;
    }
    let user = paths::user(root);
    let backup_name = format!("user-{}", timestamp());
    let backup = paths::backups_dir(root).join(&backup_name);
    if user.exists() {
        if let Err(e) = move_item(&user, &backup) {
            report.failed.push(("pack de données".into(), format!("sauvegarde impossible ({e}) — pack laissé en attente")));
            return;
        }
    }
    let _ = std::fs::remove_file(incoming.join(READY_MARKER));
    if let Err(e) = std::fs::rename(&incoming, &user) {
        // Put the data back exactly as it was.
        let _ = std::fs::rename(&backup, &user);
        report.failed.push(("pack de données".into(), e.to_string()));
        return;
    }
    // A pack exported without secrets must not log this machine out of everything.
    let secrets = paths::secrets_file(root);
    let previous = backup.join("secrets.toml");
    if !secrets.exists() && previous.exists() {
        let _ = std::fs::copy(&previous, &secrets);
    }
    report.pack_installed = Some(backup_name);
}

// ─── 2. Pre-separation layout ───────────────────────────────────────────────────

/// Fold the write-ahead log into the database file so it can move as ONE file.
/// Refuses when another process holds the database: moving it then would split a
/// live database from the instance writing to it.
fn checkpoint(db: &Path) -> Result<(), String> {
    let conn = rusqlite::Connection::open(db).map_err(|e| e.to_string())?;
    let busy: i64 = conn
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    if busy != 0 {
        return Err("base utilisée par une autre instance de TagDash".into());
    }
    Ok(())
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

fn move_legacy_db(root: &Path) -> Result<bool, String> {
    let src = root.join("tagdash.db");
    let dst = paths::cache_db(root);
    if !src.exists() {
        return Ok(false);
    }
    if dst.exists() {
        return Err(ALREADY_THERE.into());
    }
    checkpoint(&src)?;
    move_item(&src, &dst)?;
    // Empty after the checkpoint when they still exist at all.
    let _ = move_item(&with_suffix(&src, "-wal"), &with_suffix(&dst, "-wal"));
    let _ = std::fs::remove_file(with_suffix(&src, "-shm"));
    Ok(true)
}

/// (old name under the root, new place). Order matters for `reviews/tt_images`,
/// which leaves `reviews/` for the cache before `reviews/` itself moves.
fn legacy_items(root: &Path) -> Vec<(&'static str, PathBuf)> {
    vec![
        ("tagdash.toml", paths::settings_file(root)),
        ("tagdash.secrets.toml", paths::secrets_file(root)),
        ("dashboard-default.json", paths::user(root).join("dashboard-default.json")),
        ("prompts", paths::prompts_dir(root)),
        ("mood", paths::mood_dir(root)),
        ("backgrounds", paths::backgrounds_dir(root)),
        ("screenshots", paths::screenshots_dir(root)),
        ("reviews/tt_images", paths::tt_images_dir(root)),
        ("reviews", paths::reviews_dir(root)),
        ("stt_jobs", paths::stt_jobs_dir(root)),
        ("flat_files", paths::flat_files_dir(root)),
        ("tape", paths::tape_dir(root)),
        ("replay", paths::replay_dir(root)),
        ("models", paths::models_dir(root)),
        ("tradetally-webview", paths::tradetally_webview_dir(root)),
    ]
}

/// Without these three the app would start as a blank install next to the user's
/// real data. Failing to move one stops the launch instead.
const CRITICAL: [&str; 3] = ["tagdash.db", "tagdash.toml", "tagdash.secrets.toml"];

fn migrate_legacy(root: &Path, report: &mut LayoutReport) -> Result<(), String> {
    fn record(name: &'static str, result: Result<bool, String>, report: &mut LayoutReport) -> Result<(), String> {
        match result {
            Ok(true) => report.moved.push(name.to_string()),
            Ok(false) => {}
            // Already there in the new layout: the new layout wins, the old item
            // stays where it is and Réglages ▸ Mes données lists it.
            Err(reason) if reason == ALREADY_THERE => report.failed.push((name.to_string(), reason)),
            Err(reason) if CRITICAL.contains(&name) => {
                return Err(format!(
                    "impossible de déplacer {name} vers la nouvelle organisation : {reason}"
                ));
            }
            Err(reason) => report.failed.push((name.to_string(), reason)),
        }
        Ok(())
    }
    record("tagdash.db", move_legacy_db(root), report)?;
    for (name, dst) in legacy_items(root) {
        record(name, move_item(&root.join(name), &dst), report)?;
    }
    Ok(())
}

/// Pre-separation items still sitting at the root (a move that failed, or files an
/// older build wrote there after the reorganisation).
pub fn legacy_leftovers(root: &Path) -> Vec<String> {
    let mut names: Vec<&str> = vec!["tagdash.db"];
    names.extend(legacy_items(root).into_iter().map(|(n, _)| n));
    names
        .into_iter()
        .filter(|n| *n != "reviews/tt_images" && root.join(n).exists())
        .map(str::to_string)
        .collect()
}

// ─── Folder notes ───────────────────────────────────────────────────────────────

const USER_NOTE: &str = "TagDash — données personnelles\r\n\
\r\n\
Ce dossier contient tout ce qui t'appartient et qu'aucun fournisseur ne peut te\r\n\
rendre : réglages, secrets, trades et notes, revues (sessions, audio), captures,\r\n\
dessins, tableau de bord, files d'attente.\r\n\
\r\n\
Pour passer sur une autre machine : Réglages > Mes données > Exporter, puis\r\n\
Importer sur l'autre machine. Ne copie pas ce dossier à la main pendant que\r\n\
TagDash tourne.\r\n";

const CACHE_NOTE: &str = "TagDash — cache\r\n\
\r\n\
Données de marché et de référence, modèles, miroirs de sources distantes.\r\n\
Tout ce qui est ici se retélécharge : supprimer ce dossier (TagDash fermé) ne\r\n\
fait perdre que du temps et de la bande passante.\r\n";

fn write_note(dir: &Path, text: &str) {
    let file = dir.join("LISEZMOI.txt");
    if !file.exists() {
        let _ = std::fs::write(file, text);
    }
}

// ─── Entry point ────────────────────────────────────────────────────────────────

/// Bring the data root to the current layout. Called first thing at launch, before
/// the settings are read or a database is opened. An `Err` means the app must not
/// start (it would come up empty beside the user's data).
pub fn prepare(root: &Path) -> Result<LayoutReport, String> {
    let mut report = LayoutReport::default();
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    // Legacy first: a root that is BOTH pre-separation and carrying a staged pack
    // must back up the user's sorted data, not an empty folder.
    migrate_legacy(root, &mut report)?;
    install_incoming(root, &mut report);
    for (dir, note) in [(paths::user(root), USER_NOTE), (paths::cache(root), CACHE_NOTE)] {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        write_note(&dir, note);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tagdash-layout-{name}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// A root as a pre-separation build leaves it.
    fn legacy_root(name: &str) -> PathBuf {
        let root = scratch(name);
        write(&root.join("tagdash.toml"), "[trading]\n");
        write(&root.join("tagdash.secrets.toml"), "massive_api_key = \"k\"\n");
        write(&root.join("mood/short.txt"), "less is more\n");
        write(&root.join("backgrounds/a.jpg"), "jpg");
        write(&root.join("screenshots/s1.png"), "png");
        write(&root.join("reviews/2026-06-12/RV-1.db"), "db");
        write(&root.join("reviews/tt_images/img.webp"), "webp");
        write(&root.join("flat_files/daily/daily.db"), "bars");
        write(&root.join("tape/tape-2026-06-12.db"), "prints");
        let conn = rusqlite::Connection::open(root.join("tagdash.db")).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE executions (fill_id TEXT PRIMARY KEY NOT NULL, trade_id TEXT NOT NULL,
                                      symbol TEXT NOT NULL, quantity INTEGER NOT NULL,
                                      fill_price REAL NOT NULL, filled_at TEXT NOT NULL);
             INSERT INTO executions VALUES ('f1', 't1', 'AAPL', 100, 1.5, '2026-06-12T13:31:00Z');",
        )
        .unwrap();
        root
    }

    #[test]
    fn a_pre_separation_root_is_sorted_into_user_and_cache() {
        let root = legacy_root("sort");
        let report = prepare(&root).unwrap();
        assert!(report.failed.is_empty(), "{:?}", report.failed);

        assert!(paths::settings_file(&root).exists());
        assert!(paths::secrets_file(&root).exists());
        assert!(paths::mood_dir(&root).join("short.txt").exists());
        assert!(paths::backgrounds_dir(&root).join("a.jpg").exists());
        assert!(paths::screenshots_dir(&root).join("s1.png").exists());
        assert!(paths::reviews_dir(&root).join("2026-06-12/RV-1.db").exists());
        // Re-downloadable: the cache side.
        assert!(paths::cache_db(&root).exists());
        assert!(paths::tt_images_dir(&root).join("img.webp").exists());
        assert!(!paths::reviews_dir(&root).join("tt_images").exists());
        assert!(paths::flat_files_dir(&root).join("daily/daily.db").exists());
        assert!(paths::tape_dir(&root).join("tape-2026-06-12.db").exists());
        // Nothing of the old layout is left at the root.
        assert!(legacy_leftovers(&root).is_empty(), "{:?}", legacy_leftovers(&root));

        // The database survived the move whole, and the split finishes the job.
        let conn = crate::local_db::open_and_migrate(&root).unwrap();
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM user.executions", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn preparing_twice_changes_nothing() {
        let root = legacy_root("twice");
        prepare(&root).unwrap();
        let again = prepare(&root).unwrap();
        assert!(again.moved.is_empty() && again.failed.is_empty() && again.pack_installed.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_fresh_install_just_gets_its_two_folders() {
        let root = scratch("fresh");
        let report = prepare(&root).unwrap();
        assert!(report.moved.is_empty());
        assert!(paths::user(&root).join("LISEZMOI.txt").exists());
        assert!(paths::cache(&root).join("LISEZMOI.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn files_an_old_build_wrote_later_never_overwrite_the_new_layout() {
        let root = legacy_root("later");
        prepare(&root).unwrap();
        std::fs::write(paths::settings_file(&root), "[trading]\n# mine\n").unwrap();
        // An older build is launched afterwards and recreates its own files.
        write(&root.join("tagdash.toml"), "[trading]\n# stale\n");
        let report = prepare(&root).unwrap();
        assert_eq!(report.failed.len(), 1);
        assert!(std::fs::read_to_string(paths::settings_file(&root)).unwrap().contains("# mine"));
        assert_eq!(legacy_leftovers(&root), ["tagdash.toml"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_staged_pack_replaces_the_data_and_keeps_a_backup() {
        let root = scratch("pack");
        prepare(&root).unwrap();
        write(&paths::settings_file(&root), "# before\n");
        write(&paths::secrets_file(&root), "massive_api_key = \"local\"\n");
        let incoming = paths::incoming_dir(&root);
        write(&incoming.join("settings.toml"), "# from the pack\n");
        write(&incoming.join(READY_MARKER), "");

        let report = prepare(&root).unwrap();
        let backup = report.pack_installed.expect("pack installed");
        assert!(std::fs::read_to_string(paths::settings_file(&root)).unwrap().contains("from the pack"));
        assert!(!paths::user(&root).join(READY_MARKER).exists());
        assert!(!incoming.exists());
        // The replaced data is whole, under backups/.
        let kept = paths::backups_dir(&root).join(&backup).join("settings.toml");
        assert!(std::fs::read_to_string(kept).unwrap().contains("before"));
        // The pack carried no secrets: this machine keeps its own.
        assert!(std::fs::read_to_string(paths::secrets_file(&root)).unwrap().contains("local"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_unfinished_extraction_is_discarded() {
        let root = scratch("partial");
        prepare(&root).unwrap();
        write(&paths::settings_file(&root), "# mine\n");
        write(&paths::incoming_dir(&root).join("settings.toml"), "# half a pack\n");
        let report = prepare(&root).unwrap();
        assert!(report.pack_installed.is_none());
        assert!(!paths::incoming_dir(&root).exists());
        assert!(std::fs::read_to_string(paths::settings_file(&root)).unwrap().contains("# mine"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
