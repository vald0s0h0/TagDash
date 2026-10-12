// On-disk layout. ONE data root, two compartments — the code (the app bundle and
// its bundled defaults) lives elsewhere and is never written to.
//
//   <root>/user/    PERSONAL — what belongs to the person using this install and
//                   that no provider can hand back: settings, secrets, trades and
//                   their notes, review sessions, drawings, dashboard, usage log,
//                   outbound queues. This folder IS the data pack (`crate::user_data`).
//   <root>/cache/   RE-DOWNLOADABLE — market data, reference data, models, mirrors
//                   of remote sources. Deleting it costs bandwidth, nothing else.
//
// Everything that builds a path goes through here, so "is this personal?" is
// answered in one file rather than by whichever module happened to join a name
// onto the app directory.
//
// The root is `<system config dir>/tagdash`, or `TAGDASH_HOME` when set — the way to
// run a build against a sandbox copy instead of the real data.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Overrides the data root (dev / tests against a copy of the data).
pub const HOME_ENV: &str = "TAGDASH_HOME";

/// The data root of this process. Resolved once.
pub fn root() -> PathBuf {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        if let Some(home) = std::env::var_os(HOME_ENV).filter(|v| !v.is_empty()) {
            return PathBuf::from(home);
        }
        dirs::config_dir()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
            .join("tagdash")
    })
    .clone()
}

// ─── Compartments ───────────────────────────────────────────────────────────

pub const USER_DIR: &str = "user";
pub const CACHE_DIR: &str = "cache";

pub fn user(root: &Path) -> PathBuf {
    root.join(USER_DIR)
}

pub fn cache(root: &Path) -> PathBuf {
    root.join(CACHE_DIR)
}

// ─── Personal ───────────────────────────────────────────────────────────────

pub fn settings_file(root: &Path) -> PathBuf {
    user(root).join("settings.toml")
}

pub fn secrets_file(root: &Path) -> PathBuf {
    user(root).join("secrets.toml")
}

/// Personal tables. Attached to the main connection as schema `user` — see
/// `crate::local_db`.
pub fn user_db(root: &Path) -> PathBuf {
    user(root).join("user.db")
}

/// Editable A+ rubrics.
pub fn prompts_dir(root: &Path) -> PathBuf {
    user(root).join("prompts")
}

/// Mood images + the short / long phrase files.
pub fn mood_dir(root: &Path) -> PathBuf {
    user(root).join("mood")
}

pub fn backgrounds_dir(root: &Path) -> PathBuf {
    user(root).join("backgrounds")
}

pub fn screenshots_dir(root: &Path) -> PathBuf {
    user(root).join("screenshots")
}

/// Review sessions: per-ticker tape copies and voice notes.
pub fn reviews_dir(root: &Path) -> PathBuf {
    user(root).join("reviews")
}

/// Dictées not transcribed yet (an outbound queue: audio + job metadata).
pub fn stt_jobs_dir(root: &Path) -> PathBuf {
    user(root).join("stt_jobs")
}

// ─── Cache ──────────────────────────────────────────────────────────────────

/// Market / reference data and everything else a provider can give back.
pub fn cache_db(root: &Path) -> PathBuf {
    cache(root).join("cache.db")
}

pub fn flat_files_dir(root: &Path) -> PathBuf {
    cache(root).join("flat_files")
}

pub fn tape_dir(root: &Path) -> PathBuf {
    cache(root).join("tape")
}

pub fn replay_dir(root: &Path) -> PathBuf {
    cache(root).join("replay")
}

/// Whisper models (downloaded on first use).
pub fn models_dir(root: &Path) -> PathBuf {
    cache(root).join("models")
}

/// TradeTally gallery images fetched for the review (the server keeps them).
pub fn tt_images_dir(root: &Path) -> PathBuf {
    cache(root).join("tt_images")
}

/// The embedded TradeTally webview's own storage (its login session).
pub fn tradetally_webview_dir(root: &Path) -> PathBuf {
    cache(root).join("tradetally-webview")
}

// ─── Pack staging + backups ─────────────────────────────────────────────────

/// A validated, unpacked data pack waiting for the next launch to become `user/`.
pub fn incoming_dir(root: &Path) -> PathBuf {
    root.join("user.incoming")
}

/// Personal data set aside each time a pack replaced it.
pub fn backups_dir(root: &Path) -> PathBuf {
    root.join("backups")
}

// ─── Stored paths ───────────────────────────────────────────────────────────
//
// A file path written into the personal database (a screenshot, a session store,
// a voice note) is kept RELATIVE to `user/`, with forward slashes. An absolute
// path would name this machine and this OS, and the row would point nowhere the
// moment the data moves — which is the whole point of a data pack.

/// Sub-folders of `user/` a stored path may point into.
const STORED_ROOTS: [&str; 2] = ["screenshots", "reviews"];

/// What to write in the database for `path`: relative to `user/` when it lives
/// there, unchanged otherwise.
pub fn to_stored(path: &Path) -> String {
    match path.strip_prefix(user(&root())) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => path.to_string_lossy().to_string(),
    }
}

/// The file a stored value designates on this machine.
pub fn from_stored(stored: &str) -> PathBuf {
    let p = Path::new(stored);
    if p.is_absolute() || is_windows_absolute(stored) {
        return p.to_path_buf();
    }
    let mut out = user(&root());
    for part in stored.split('/').filter(|s| !s.is_empty()) {
        out.push(part);
    }
    out
}

fn is_windows_absolute(s: &str) -> bool {
    let b = s.as_bytes();
    (b.len() > 2 && b[0].is_ascii_alphabetic() && b[1] == b':') || s.starts_with("\\\\")
}

/// Rewrite a value stored by an older build (an absolute path, on whichever OS
/// wrote it) into the relative form. Recognised by shape — `…/tagdash/<folder>/…`
/// or `…/tagdash/user/<folder>/…` — so a path written on another machine converts
/// too. Anything else is returned unchanged.
pub fn normalize_stored(stored: &str) -> String {
    let unified = stored.replace('\\', "/");
    if !unified.starts_with('/') && !is_windows_absolute(stored) {
        return unified; // already relative
    }
    // Under this machine's own user folder (covers a custom TAGDASH_HOME).
    let here = user(&root()).to_string_lossy().replace('\\', "/");
    if let Some(rest) = unified.strip_prefix(&format!("{}/", here.trim_end_matches('/'))) {
        return rest.to_string();
    }
    for folder in STORED_ROOTS {
        let marker = format!("/{folder}/");
        let Some(at) = unified.rfind(&marker) else {
            continue;
        };
        let owner = unified[..at].trim_end_matches("/user");
        let is_data_root = owner
            .rsplit('/')
            .next()
            .is_some_and(|last| last.eq_ignore_ascii_case("tagdash"));
        if is_data_root {
            return unified[at + 1..].to_string();
        }
    }
    stored.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_legacy_mac_path_becomes_relative() {
        assert_eq!(
            normalize_stored(
                "/Users/x/Library/Application Support/tagdash/reviews/2026-06-12/RV-20260612-CLIK-B.db"
            ),
            "reviews/2026-06-12/RV-20260612-CLIK-B.db"
        );
        assert_eq!(
            normalize_stored("/Users/x/Library/Application Support/tagdash/screenshots/a b.png"),
            "screenshots/a b.png"
        );
    }

    #[test]
    fn a_windows_path_converts_too() {
        assert_eq!(
            normalize_stored(r"C:\Users\x\AppData\Roaming\tagdash\screenshots\shot.png"),
            "screenshots/shot.png"
        );
        assert_eq!(
            normalize_stored(r"C:\Users\x\AppData\Roaming\tagdash\user\reviews\2026-06-12\v.ulaw"),
            "reviews/2026-06-12/v.ulaw"
        );
    }

    #[test]
    fn a_relative_value_is_left_alone() {
        assert_eq!(normalize_stored("screenshots/shot.png"), "screenshots/shot.png");
        assert_eq!(normalize_stored(r"reviews\2026-06-12\v.ulaw"), "reviews/2026-06-12/v.ulaw");
    }

    #[test]
    fn a_path_outside_a_data_root_is_not_guessed() {
        let elsewhere = "/Users/x/Desktop/screenshots/shot.png";
        assert_eq!(normalize_stored(elsewhere), elsewhere);
        let tmp = "/tmp/tagdash-review-123/reviews/2026-06-12/s.db";
        assert_eq!(normalize_stored(tmp), tmp);
    }

    #[test]
    fn stored_form_round_trips_under_the_user_folder() {
        let file = screenshots_dir(&root()).join("shot.png");
        let stored = to_stored(&file);
        assert_eq!(stored, "screenshots/shot.png");
        assert_eq!(from_stored(&stored), file);
    }

    #[test]
    fn a_path_elsewhere_is_stored_and_read_back_as_is() {
        let file = std::env::temp_dir().join("tagdash-elsewhere").join("s.db");
        let stored = to_stored(&file);
        assert_eq!(Path::new(&stored), file);
        assert_eq!(from_stored(&stored), file);
    }
}
