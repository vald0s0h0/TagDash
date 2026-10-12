// Personal data as one thing: where it lives, what it holds, and how it travels.
//
//   `layout`  sorts the data root into `user/` + `cache/` and installs a staged pack
//   `pack`    exports / imports the data pack (one file, carried by hand)
//   here      the overview Réglages ▸ Mes données shows
//
// The separation itself is declared in two places and nowhere else: `crate::paths`
// for files, `local_db::schema::USER_TABLES` for rows.

pub mod layout;
pub mod pack;

use std::path::Path;

use rusqlite::Connection;
use serde::Serialize;

use crate::local_db::split::{self, LegacyLeftovers};
use crate::paths;

/// Total size of the files under `dir` (0 when it does not exist).
pub fn dir_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => dir_bytes(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

fn file_bytes(file: &Path) -> u64 {
    std::fs::metadata(file).map(|m| m.len()).unwrap_or(0)
}

/// Files directly or indirectly under `dir`, OS litter aside.
fn file_count(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => file_count(&entry.path()),
            Ok(kind) if kind.is_file() => {
                let name = entry.file_name();
                u64::from(name != ".DS_Store" && name != "Thumbs.db" && name != "LISEZMOI.txt")
            }
            _ => 0,
        })
        .sum()
}

/// Dictées still waiting for the transcriber: a job keeps its audio until it is done.
fn pending_dictations(root: &Path) -> i64 {
    std::fs::read_dir(paths::stt_jobs_dir(root))
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "wav"))
                .count() as i64
        })
        .unwrap_or(0)
}

/// A SQLite file with its write-ahead log.
fn db_bytes(db: &Path) -> u64 {
    let wal = db.with_file_name(format!(
        "{}-wal",
        db.file_name().unwrap_or_default().to_string_lossy()
    ));
    file_bytes(db) + file_bytes(&wal)
}

/// One line of the overview.
#[derive(Debug, Clone, Serialize)]
pub struct Category {
    /// Stable id — also what `open_target` takes to open the folder.
    pub id: &'static str,
    pub label: &'static str,
    /// What is in there, in words ("182 trades · 122 notes").
    pub detail: String,
    pub bytes: u64,
    /// True when `id` opens a folder of its own in the file manager.
    pub openable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Overview {
    pub machine: String,
    pub user_dir: String,
    pub cache_dir: String,
    pub personal: Vec<Category>,
    pub personal_bytes: u64,
    pub cache: Vec<Category>,
    pub cache_bytes: u64,
    pub counts: pack::Counts,
    /// Local time of the last write to the personal database or the settings.
    pub last_change: Option<String>,
    /// A pack staged for the next launch.
    pub staged: Option<pack::Manifest>,
    pub backups: Vec<pack::Backup>,
    /// Pre-separation originals still kept in the cache database.
    pub legacy: LegacyLeftovers,
    /// Pre-separation files still at the root of the data folder.
    pub legacy_files: Vec<String>,
}

fn plural(n: i64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// What the overview needs from the database, read in one short hold of the
/// connection — the folder sizes are measured after it is released.
#[derive(Debug, Clone)]
pub struct DbFacts {
    counts: pack::Counts,
    reviews_done: i64,
    trade_levels: i64,
    alarms: i64,
    ui_stores: i64,
    option_sets: i64,
    verdicts: i64,
    legacy: LegacyLeftovers,
}

pub fn db_facts(conn: &Connection) -> DbFacts {
    let c = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0);
    DbFacts {
        counts: pack::counts(conn),
        reviews_done: c("SELECT COUNT(*) FROM review_log"),
        trade_levels: c("SELECT COUNT(*) FROM trade_levels"),
        alarms: c("SELECT COUNT(*) FROM price_alarms"),
        ui_stores: c("SELECT COUNT(*) FROM ui_state"),
        option_sets: c("SELECT COUNT(*) FROM user_config"),
        verdicts: c("SELECT COUNT(*) FROM llm_analysis")
            + c("SELECT COUNT(*) FROM panic_llm_scores")
            + c("SELECT COUNT(*) FROM micro_quality_scores")
            + c("SELECT COUNT(*) FROM micro_pullback_quality_scores"),
        legacy: split::leftovers(conn).unwrap_or(LegacyLeftovers {
            tables: Vec::new(),
            rows: 0,
            keys: 0,
            unmoved: Vec::new(),
        }),
    }
}

fn last_change(root: &Path) -> Option<String> {
    let db = paths::user_db(root);
    let wal = db.with_file_name("user.db-wal");
    [db, wal, paths::settings_file(root)]
        .iter()
        .filter_map(|f| std::fs::metadata(f).and_then(|m| m.modified()).ok())
        .max()
        .map(|t| chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d %H:%M").to_string())
}

/// How many secrets are filled in (the derived market-data pair is not one the
/// user typed).
pub fn secrets_set(status: &crate::config::secrets::SecretsStatus) -> usize {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| {
            v.as_object().map(|o| {
                o.iter()
                    .filter(|(k, v)| !matches!(k.as_str(), "alpaca_key" | "alpaca_secret") && v.as_bool() == Some(true))
                    .count()
            })
        })
        .unwrap_or(0)
}

/// Everything Réglages ▸ Mes données shows. Measures folders on disk, so it takes
/// the database facts ready-made (`db_facts`) instead of holding the connection.
pub fn overview(root: &Path, facts: DbFacts, secrets_set: usize) -> Overview {
    let DbFacts { counts, reviews_done, trade_levels, alarms, ui_stores, option_sets, verdicts, legacy } = facts;
    let (mood_images, short, long) = crate::dashboard::mood_paths(root);
    let lines = |file: &Path| -> i64 {
        std::fs::read_to_string(file)
            .map(|t| t.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).count() as i64)
            .unwrap_or(0)
    };
    let user = paths::user(root);

    let personal = vec![
        Category {
            id: "settings",
            label: "Réglages",
            detail: "broker, trading, interface, notifications, dictée, revue".into(),
            bytes: file_bytes(&paths::settings_file(root)),
            openable: false,
        },
        Category {
            id: "secrets",
            label: "Secrets",
            detail: plural(secrets_set as i64, "clé ou identifiant renseigné", "clés et identifiants renseignés"),
            bytes: file_bytes(&paths::secrets_file(root)),
            openable: false,
        },
        Category {
            id: "trades",
            label: "Trades et journal",
            detail: format!(
                "{} · {} · {}",
                plural(counts.trades, "trade", "trades"),
                plural(counts.executions, "exécution", "exécutions"),
                plural(counts.journal_notes, "note", "notes"),
            ),
            bytes: 0,
            openable: false,
        },
        Category {
            id: "reviews",
            label: "Revue",
            detail: format!(
                "{} · {} · {}",
                plural(counts.review_sessions, "session", "sessions"),
                plural(counts.voice_notes, "note vocale", "notes vocales"),
                plural(reviews_done, "revue terminée", "revues terminées"),
            ),
            bytes: dir_bytes(&paths::reviews_dir(root)),
            openable: true,
        },
        Category {
            id: "screenshots",
            label: "Captures",
            detail: plural(counts.screenshots, "capture", "captures"),
            bytes: dir_bytes(&paths::screenshots_dir(root)),
            openable: true,
        },
        Category {
            id: "charts",
            label: "Dessins, niveaux et alarmes",
            detail: format!(
                "{} · {} · {}",
                plural(counts.drawings, "dessin", "dessins"),
                plural(trade_levels, "SL/TP d'origine", "SL/TP d'origine"),
                plural(alarms, "alarme", "alarmes"),
            ),
            bytes: 0,
            openable: false,
        },
        Category {
            id: "backgrounds",
            label: "Fonds d'écran",
            detail: plural(file_count(&paths::backgrounds_dir(root)) as i64, "image", "images"),
            bytes: dir_bytes(&paths::backgrounds_dir(root)),
            openable: true,
        },
        Category {
            id: "mood",
            label: "Mood",
            detail: format!(
                "{} · {} · {}",
                plural(file_count(&mood_images) as i64, "image", "images"),
                plural(lines(&short), "phrase courte", "phrases courtes"),
                plural(lines(&long), "phrase longue", "phrases longues"),
            ),
            bytes: dir_bytes(&paths::mood_dir(root)),
            openable: true,
        },
        Category {
            id: "interface",
            label: "Interface",
            detail: format!(
                "{} (tableau de bord, raccourcis, manette, thème des graphiques…)",
                plural(ui_stores, "réglage d'affichage", "réglages d'affichage"),
            ),
            bytes: 0,
            openable: false,
        },
        Category {
            id: "strategies",
            label: "Stratégies",
            detail: format!(
                "{} · {}",
                plural(option_sets, "jeu de paramètres", "jeux de paramètres"),
                plural(file_count(&paths::prompts_dir(root)) as i64, "barème A+", "barèmes A+"),
            ),
            bytes: dir_bytes(&paths::prompts_dir(root)),
            openable: true,
        },
        Category {
            id: "queues",
            label: "Envois en attente",
            detail: format!(
                "{} · {}",
                plural(counts.pending_sync, "envoi TradeTally", "envois TradeTally"),
                plural(pending_dictations(root), "dictée à transcrire", "dictées à transcrire"),
            ),
            bytes: dir_bytes(&paths::stt_jobs_dir(root)),
            openable: false,
        },
        Category {
            id: "analyses",
            label: "Analyses des modèles",
            detail: plural(verdicts, "verdict", "verdicts"),
            bytes: 0,
            openable: false,
        },
        Category {
            id: "usage",
            label: "Suivi d'usage",
            detail: plural(counts.usage_events, "action enregistrée", "actions enregistrées"),
            bytes: 0,
            openable: false,
        },
        Category {
            id: "database",
            label: "Base personnelle",
            detail: "toutes les lignes ci-dessus (user.db)".into(),
            bytes: db_bytes(&paths::user_db(root)),
            openable: false,
        },
    ];

    let cache = vec![
        Category {
            id: "cache_db",
            label: "Base de marché",
            detail: "barres journalières, univers, fondamentaux, dépôts SEC, journaux".into(),
            bytes: db_bytes(&paths::cache_db(root)),
            openable: false,
        },
        Category {
            id: "flat_files",
            label: "Flat files",
            detail: "barres minute et intraday téléchargées".into(),
            bytes: dir_bytes(&paths::flat_files_dir(root)),
            openable: true,
        },
        Category {
            id: "tape",
            label: "Tape",
            detail: "prints enregistrés du flux live".into(),
            bytes: dir_bytes(&paths::tape_dir(root)),
            openable: true,
        },
        Category {
            id: "replay",
            label: "Replay",
            detail: "journées préparées".into(),
            bytes: dir_bytes(&paths::replay_dir(root)),
            openable: true,
        },
        Category {
            id: "models",
            label: "Modèles de dictée",
            detail: "whisper".into(),
            bytes: dir_bytes(&paths::models_dir(root)),
            openable: true,
        },
        Category {
            id: "tt_images",
            label: "Images TradeTally",
            detail: "galeries déjà consultées".into(),
            bytes: dir_bytes(&paths::tt_images_dir(root)),
            openable: true,
        },
    ];

    Overview {
        machine: pack::machine_name(),
        user_dir: user.to_string_lossy().to_string(),
        cache_dir: paths::cache(root).to_string_lossy().to_string(),
        // The database line already holds every row-only category.
        personal_bytes: dir_bytes(&user),
        cache_bytes: dir_bytes(&paths::cache(root)),
        personal,
        cache,
        counts,
        last_change: last_change(root),
        staged: pack::staged(root),
        backups: pack::list_backups(root),
        legacy,
        legacy_files: layout::legacy_leftovers(root),
    }
}

/// The folder a category (or one of the two compartments) opens in the file
/// manager. `None` for an id that has no folder of its own.
pub fn open_target(root: &Path, id: &str) -> Option<std::path::PathBuf> {
    Some(match id {
        "user" => paths::user(root),
        "cache" => paths::cache(root),
        "backups" => paths::backups_dir(root),
        "root" => root.to_path_buf(),
        "reviews" => paths::reviews_dir(root),
        "screenshots" => paths::screenshots_dir(root),
        "backgrounds" => paths::backgrounds_dir(root),
        "mood" => paths::mood_dir(root),
        "strategies" => paths::prompts_dir(root),
        "flat_files" => paths::flat_files_dir(root),
        "tape" => paths::tape_dir(root),
        "replay" => paths::replay_dir(root),
        "models" => paths::models_dir(root),
        "tt_images" => paths::tt_images_dir(root),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_openable_category_has_a_folder() {
        let root = std::env::temp_dir().join(format!("tagdash-overview-{}", std::process::id()));
        layout::prepare(&root).unwrap();
        let conn = crate::local_db::open_and_migrate(&root).unwrap();
        let view = overview(&root, db_facts(&conn), 0);
        for cat in view.personal.iter().chain(&view.cache) {
            assert_eq!(cat.openable, open_target(&root, cat.id).is_some(), "{}", cat.id);
        }
        assert!(view.legacy.tables.is_empty() && view.legacy_files.is_empty());
        assert!(open_target(&root, "../etc").is_none());
        drop(conn);
        let _ = std::fs::remove_dir_all(root);
    }

    /// Dry run of the WHOLE separation on a COPY of a real, pre-separation data root:
    ///   TAGDASH_HOME=/path/to/copy cargo test --lib real_data_dry_run -- --ignored --nocapture
    /// Sorts the folder, splits the database, then checks that every personal row
    /// and every file a row points at came through, and that a pack exported from
    /// the result installs into an empty root with the same content. Refuses to run
    /// without `TAGDASH_HOME` — it would reorganise the real data.
    #[test]
    #[ignore]
    fn real_data_dry_run() {
        use crate::local_db::schema::USER_TABLES;
        assert!(
            std::env::var_os(paths::HOME_ENV).is_some(),
            "TAGDASH_HOME manquant : ce test réorganise le dossier, il lui faut une copie"
        );
        let root = crate::config::app_dir();
        let stored_columns =
            [("screenshot_files", "path"), ("review_sessions", "store_path"), ("review_voice", "audio_path")];

        // ── Before: what the pre-separation database holds.
        let legacy_db = root.join("tagdash.db");
        let mut before: Vec<(String, i64)> = Vec::new();
        let mut files_before = 0usize;
        if legacy_db.exists() {
            let old = Connection::open(&legacy_db).unwrap();
            for t in USER_TABLES {
                if let Ok(n) = old.query_row(&format!("SELECT COUNT(*) FROM \"{t}\""), [], |r| r.get::<_, i64>(0)) {
                    before.push((t.to_string(), n));
                }
            }
            // Rows whose file exists today, at the place the copy was taken FROM.
            for (table, column) in stored_columns {
                let paths: Vec<String> = old
                    .prepare(&format!("SELECT {column} FROM {table} WHERE {column} IS NOT NULL"))
                    .and_then(|mut st| st.query_map([], |r| r.get(0)).map(|it| it.filter_map(|r| r.ok()).collect()))
                    .unwrap_or_default();
                files_before += paths.iter().filter(|p| Path::new(p).exists()).count();
            }
        }

        // ── The migration itself, timed.
        let started = std::time::Instant::now();
        let report = layout::prepare(&root).unwrap();
        println!("dossier trié en {:?} : {:?}", started.elapsed(), report.moved);
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        let started = std::time::Instant::now();
        let conn = crate::local_db::open_and_migrate(&root).unwrap();
        println!("base séparée en {:?}", started.elapsed());

        // ── After: every row arrived, none is shadowed, the originals are kept.
        for (table, n) in &before {
            let now: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM user.\"{table}\""), [], |r| r.get(0)).unwrap();
            println!("  {table:32} {n:>8} → {now:>8}");
            assert_eq!(now, *n, "{table}");
        }
        let left = split::leftovers(&conn).unwrap();
        assert!(left.unmoved.is_empty(), "restées dans le cache : {:?}", left.unmoved);
        println!("originaux conservés : {} tables, {} lignes, {} clés", left.tables.len(), left.rows, left.keys);
        let integrity: String = conn.query_row("PRAGMA user.integrity_check", [], |r| r.get(0)).unwrap();
        assert_eq!(integrity, "ok");

        // ── Settings and secrets are read from their new place, unchanged.
        let (_, cfg) = crate::config::load();
        let secrets = crate::config::secrets::load(&root);
        println!(
            "réglages relus : TradeTally {} · {} secret(s) renseigné(s)",
            if cfg.tradetally.api_base_url.is_empty() { "absent" } else { "présent" },
            secrets_set(&secrets.status()),
        );
        assert!(left.keys > 0 || before.is_empty(), "réglages de stratégie non repris");

        // ── Every stored path is relative now, and designates a file in the copy.
        let mut files_after = 0usize;
        for (table, column) in stored_columns {
            let stored: Vec<String> = conn
                .prepare(&format!("SELECT {column} FROM {table} WHERE {column} IS NOT NULL"))
                .unwrap()
                .query_map([], |r| r.get(0))
                .unwrap()
                .filter_map(|r| r.ok())
                .collect();
            let absolute = stored.iter().filter(|p| Path::new(p).is_absolute()).count();
            assert_eq!(absolute, 0, "{table}.{column} : chemins absolus restants");
            files_after += stored.iter().filter(|p| paths::from_stored(p).exists()).count();
        }
        println!("fichiers désignés par la base : {files_before} avant, {files_after} après");
        // On a root already separated there is no "before" to compare with.
        if legacy_db.exists() || !before.is_empty() {
            assert_eq!(files_after, files_before);
        }

        // ── A pack of the result, installed into an empty root, is the same data.
        let out = root.join("dry-run-out");
        let db = std::sync::Mutex::new(conn);
        let started = std::time::Instant::now();
        let exported = pack::export(&root, &db, &out, false).unwrap();
        println!(
            "pack : {:.1} Mo ({} fichiers, {:.1} Mo avant compression) en {:?}",
            exported.pack_bytes as f64 / 1e6,
            exported.manifest.files,
            exported.manifest.bytes as f64 / 1e6,
            started.elapsed()
        );
        let other = root.join("dry-run-other-machine");
        let _ = std::fs::remove_dir_all(&other);
        layout::prepare(&other).unwrap();
        pack::stage(&other, Path::new(&exported.path)).unwrap();
        assert!(layout::prepare(&other).unwrap().pack_installed.is_some());
        let there = crate::local_db::open_and_migrate(&other).unwrap();
        let here = db.lock().unwrap();
        for table in USER_TABLES {
            let count = |c: &Connection| -> i64 {
                c.query_row(&format!("SELECT COUNT(*) FROM user.\"{table}\""), [], |r| r.get(0)).unwrap()
            };
            assert_eq!(count(&there), count(&here), "{table} après import");
        }
        assert_eq!(pack::counts(&there), pack::counts(&here));
        println!("import vérifié : {:?}", pack::counts(&there));
        // `TAGDASH_E2E_DUMP=<file>`: the overview as the Réglages page receives it.
        if let Ok(file) = std::env::var("TAGDASH_E2E_DUMP") {
            let view = overview(&root, db_facts(&here), secrets_set(&secrets.status()));
            let found = pack::describe(Path::new(&exported.path)).unwrap();
            let json = serde_json::json!({ "overview": view, "packs": [found], "assets": crate::dashboard::assets(&root) });
            std::fs::write(file, serde_json::to_string_pretty(&json).unwrap()).unwrap();
        }
        // Same files on both sides, write-ahead logs aside (folded into their store).
        let packed = |dir: &Path| pack::walk(dir).into_iter().filter(|(_, rel)| !pack::skipped(rel, true)).count();
        for dir in [paths::screenshots_dir, paths::reviews_dir, paths::backgrounds_dir, paths::mood_dir] {
            assert_eq!(packed(&dir(&other)), packed(&dir(&root)));
        }
        // Every session store that travelled opens and is sound.
        for (store, rel) in pack::walk(&paths::reviews_dir(&other)) {
            if rel.ends_with(".db") {
                let conn = Connection::open_with_flags(&store, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
                let check: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0)).unwrap();
                assert_eq!(check, "ok", "{rel}");
            }
        }
        assert!(!paths::secrets_file(&other).exists(), "pack sans secrets");
    }
}
