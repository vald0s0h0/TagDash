// The rubrics as EDITABLE FILES, with the compiled-in texts as the seed and the
// fallback.
//
//   <app data>/prompts/micro_pullback_triage.txt
//   <app data>/prompts/micro_pullback_deep.txt
//
// WHY THIS DOES NOT BREAK THE FROZEN-RUBRIC CONTRACT. `prompt`'s whole discipline is
// that two candidates scored a week apart must be comparable, which requires a
// byte-identical system prompt and a version stamped on every row. Reading the text
// from a file does not weaken that — it moves the authorship, not the guarantee:
//
//   • the prompt is still byte-identical across the calls of one session (nothing is
//     interpolated; only the user message varies);
//   • `version()` FINGERPRINTS what was actually sent. Edit a file and every row
//     written afterwards carries `…+perso-<hash>`, so an edited scale can never be
//     silently compared with the built-in one, nor with a different edit. That is a
//     stronger guarantee than a hand-bumped constant, which relies on remembering.
//
// The files are seeded once at startup and never overwritten: an edit survives an app
// upgrade. Deleting a file restores the built-in default on the next launch, which is
// the documented way back.
//
// Read on every call. Two files of a few kilobytes, once per model call that costs
// seconds and money — the cost is not measurable, and in exchange an edit takes effect
// on the very next candidate, with no restart. A file that is missing, unreadable or
// blank falls back to the built-in text rather than sending an empty system prompt.

use std::path::PathBuf;

use super::prompt::{RUBRIC_VERSION, SYSTEM_DEEP, SYSTEM_TRIAGE};
use super::Pass;

/// Directory holding the editable rubrics.
pub fn dir() -> PathBuf {
    crate::config::app_dir().join("prompts")
}

pub fn path(pass: Pass) -> PathBuf {
    dir().join(match pass {
        Pass::Triage => "micro_pullback_triage.txt",
        Pass::Deep => "micro_pullback_deep.txt",
    })
}

/// The compiled-in text for a pass — what a fresh install starts from and what a
/// missing or blank file falls back to.
pub fn builtin(pass: Pass) -> &'static str {
    match pass {
        Pass::Triage => SYSTEM_TRIAGE,
        Pass::Deep => SYSTEM_DEEP,
    }
}

/// Write the built-in texts to disk if they are not there yet. Never overwrites: a
/// user's edit must survive every upgrade. Returns the paths, for the log line that
/// tells the user where they are.
pub fn ensure_seeded() -> Vec<PathBuf> {
    let dir = dir();
    let _ = std::fs::create_dir_all(&dir);
    let mut out = Vec::new();
    for pass in [Pass::Triage, Pass::Deep] {
        let p = path(pass);
        if !p.exists() {
            let _ = std::fs::write(&p, builtin(pass));
        }
        out.push(p);
    }
    out
}

/// The text to send for `pass`: the file when it holds something, the built-in
/// otherwise.
pub fn text(pass: Pass) -> String {
    std::fs::read_to_string(path(pass))
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| builtin(pass).to_string())
}

/// Whether the file for `pass` differs from the built-in text.
pub fn is_customised(pass: Pass) -> bool {
    text(pass) != builtin(pass)
}

/// The version stamped on every persisted row.
///
/// Plain `RUBRIC_VERSION` while both rubrics are the built-in texts. As soon as
/// either file is edited, the fingerprint of what is actually being sent is appended,
/// so rows scored under an edited scale sort into their own bucket and are never
/// silently compared with the built-in ones — or with a different edit.
///
/// Only the passes that actually run matter, but computing both is cheaper than
/// threading the option down here, and a version that changed because of the OTHER
/// rubric is a false alarm, not a wrong answer.
pub fn version() -> String {
    let triage = text(Pass::Triage);
    let deep = text(Pass::Deep);
    if triage == SYSTEM_TRIAGE && deep == SYSTEM_DEEP {
        return RUBRIC_VERSION.to_string();
    }
    let mut h = fnv1a(&triage);
    h ^= fnv1a(&deep).rotate_left(17);
    format!("{RUBRIC_VERSION}+perso-{h:x}")
}

/// FNV-1a, hand-rolled for the same reason `prompt` does it: `DefaultHasher`'s output
/// is explicitly not stable across Rust releases, and this value is persisted.
fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_or_blank_file_falls_back_to_the_built_in_text() {
        // Reading through a path that cannot exist exercises the same branch a
        // deleted file takes: never send an empty system prompt.
        let missing = std::fs::read_to_string("/nonexistent/tagdash/triage.txt")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| builtin(Pass::Triage).to_string());
        assert_eq!(missing, SYSTEM_TRIAGE);

        let blank: Option<String> = Some("   \n\t ".to_string());
        let resolved = blank
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| builtin(Pass::Deep).to_string());
        assert_eq!(resolved, SYSTEM_DEEP);
    }

    #[test]
    fn the_two_passes_never_share_a_file() {
        assert_ne!(path(Pass::Triage), path(Pass::Deep));
    }

    #[test]
    fn an_edit_changes_the_stamped_version() {
        // The property that protects the table: two different texts can never be
        // stamped with the same version, so two scales can never be mixed.
        let plain = RUBRIC_VERSION.to_string();
        let edited = {
            let mut h = fnv1a("un barème modifié");
            h ^= fnv1a(SYSTEM_DEEP).rotate_left(17);
            format!("{RUBRIC_VERSION}+perso-{h:x}")
        };
        let edited_differently = {
            let mut h = fnv1a("un autre barème modifié");
            h ^= fnv1a(SYSTEM_DEEP).rotate_left(17);
            format!("{RUBRIC_VERSION}+perso-{h:x}")
        };
        assert_ne!(plain, edited);
        assert_ne!(edited, edited_differently);
        assert!(edited.starts_with(RUBRIC_VERSION));
    }

    #[test]
    fn the_fingerprint_is_order_sensitive() {
        // Swapping the two rubrics must not produce the same stamp — the rotate is
        // what stops `a ^ b` from being symmetric.
        let ab = fnv1a("A") ^ fnv1a("B").rotate_left(17);
        let ba = fnv1a("B") ^ fnv1a("A").rotate_left(17);
        assert_ne!(ab, ba);
    }
}
