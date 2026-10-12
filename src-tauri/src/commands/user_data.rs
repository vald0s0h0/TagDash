// Commands behind Réglages ▸ Mes données and ▸ Tableau de bord, and the storage the
// frontend's persisted stores read and write — see `crate::user_data`.

use std::collections::HashMap;
use std::path::PathBuf;

use tauri::State;

use crate::dashboard::{self, DashboardAssets};
use crate::local_db::{split, ui_state_repository};
use crate::state::AppState;
use crate::user_data::{self, pack, Overview};

// ─── Interface state (the frontend's persisted stores) ──────────────────────────

/// Every persisted store, `key → JSON`. Read once per window, before the stores
/// are created.
#[tauri::command(rename_all = "snake_case", async)]
pub fn get_ui_state(state: State<'_, AppState>) -> Result<HashMap<String, String>, String> {
    let conn = state.db.lock().unwrap();
    ui_state_repository::all(&conn)
        .map(|rows| rows.into_iter().collect())
        .map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case", async)]
pub fn set_ui_state(key: String, value: String, state: State<'_, AppState>) -> Result<(), String> {
    let conn = state.db.lock().unwrap();
    ui_state_repository::set(&conn, &key, &value).map_err(|e| e.to_string())
}

#[tauri::command(rename_all = "snake_case", async)]
pub fn remove_ui_state(key: String, state: State<'_, AppState>) -> Result<(), String> {
    let conn = state.db.lock().unwrap();
    ui_state_repository::remove(&conn, &key).map_err(|e| e.to_string())
}

// ─── Overview ───────────────────────────────────────────────────────────────────

#[tauri::command(rename_all = "snake_case", async)]
pub fn user_data_overview(state: State<'_, AppState>) -> Overview {
    let facts = {
        let conn = state.db.lock().unwrap();
        user_data::db_facts(&conn)
    };
    let secrets = user_data::secrets_set(&state.secrets.read().unwrap().status());
    user_data::overview(&state.app_dir, facts, secrets)
}

/// Open a category's folder (or `user` / `cache` / `backups`) in the file manager.
#[tauri::command(rename_all = "snake_case", async)]
pub fn user_data_open(target: String, state: State<'_, AppState>) -> Result<(), String> {
    let dir = user_data::open_target(&state.app_dir, &target).ok_or("dossier inconnu")?;
    dashboard::open_folder(&dir)
}

// ─── Data pack ──────────────────────────────────────────────────────────────────

/// Where a pack is written: the user's Downloads folder (the data root when the OS
/// reports none).
fn export_dir(state: &AppState) -> PathBuf {
    dirs::download_dir().unwrap_or_else(|| state.app_dir.clone())
}

/// Write a pack of this machine's personal data into Downloads and show it there.
#[tauri::command(rename_all = "snake_case")]
pub async fn user_data_export(
    include_secrets: bool,
    state: State<'_, AppState>,
) -> Result<pack::Exported, String> {
    let (root, db, dest) = (state.app_dir.clone(), state.db.clone(), export_dir(&state));
    tokio::task::spawn_blocking(move || {
        let exported = pack::export(&root, &db, &dest, include_secrets)?;
        let _ = dashboard::open_folder(&dest);
        Ok(exported)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Packs found in Downloads and on the Desktop, newest first.
#[tauri::command(rename_all = "snake_case")]
pub async fn user_data_find_packs() -> Result<Vec<pack::FoundPack>, String> {
    tokio::task::spawn_blocking(pack::find_packs).await.map_err(|e| e.to_string())
}

/// Describe the pack at `path` (dropped on the window, or typed) without unpacking it.
#[tauri::command(rename_all = "snake_case")]
pub async fn user_data_inspect(path: String) -> Result<pack::FoundPack, String> {
    tokio::task::spawn_blocking(move || pack::describe(&PathBuf::from(clean_path(&path))))
        .await
        .map_err(|e| e.to_string())?
}

/// A path as a person pastes it: quoted by the shell or the file manager, with a
/// leading `~`.
fn clean_path(raw: &str) -> String {
    let trimmed = raw.trim().trim_matches(|c| c == '"' || c == '\'');
    match (trimmed.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest).to_string_lossy().to_string(),
        _ => trimmed.to_string(),
    }
}

/// Unpack and check a pack. It replaces the personal data at the NEXT launch — the
/// frontend then offers to restart.
#[tauri::command(rename_all = "snake_case")]
pub async fn user_data_stage_import(
    path: String,
    state: State<'_, AppState>,
) -> Result<pack::Manifest, String> {
    let root = state.app_dir.clone();
    tokio::task::spawn_blocking(move || pack::stage(&root, &PathBuf::from(clean_path(&path))))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command(rename_all = "snake_case", async)]
pub fn user_data_cancel_import(state: State<'_, AppState>) -> Result<(), String> {
    pack::cancel_staged(&state.app_dir)
}

/// Stage a backup (data an earlier import set aside) to come back at the next launch.
#[tauri::command(rename_all = "snake_case", async)]
pub fn user_data_restore_backup(name: String, state: State<'_, AppState>) -> Result<(), String> {
    pack::stage_backup(&state.app_dir, &name)
}

#[tauri::command(rename_all = "snake_case", async)]
pub fn user_data_delete_backup(name: String, state: State<'_, AppState>) -> Result<(), String> {
    pack::delete_backup(&state.app_dir, &name)
}

/// Drop the pre-separation originals kept in the cache database. Irreversible, and
/// only ever run from the button that says so.
#[tauri::command(rename_all = "snake_case", async)]
pub fn user_data_purge_legacy(state: State<'_, AppState>) -> Result<usize, String> {
    let conn = state.db.lock().unwrap();
    split::purge_legacy(&conn).map_err(|e| e.to_string())
}

// ─── Dashboard assets ───────────────────────────────────────────────────────────

#[tauri::command(rename_all = "snake_case", async)]
pub fn get_dashboard_assets(state: State<'_, AppState>) -> DashboardAssets {
    dashboard::assets(&state.app_dir)
}

/// Replace a mood phrase file (`"short"` | `"long"`) with `content`.
#[tauri::command(rename_all = "snake_case", async)]
pub fn save_mood_phrases(kind: String, content: String, state: State<'_, AppState>) -> Result<(), String> {
    dashboard::save_phrases(&state.app_dir, &kind, &content)
}

#[cfg(test)]
mod tests {
    use super::clean_path;

    #[test]
    fn a_pasted_path_loses_its_quotes() {
        assert_eq!(clean_path("  '/tmp/a b.tagdashpack' "), "/tmp/a b.tagdashpack");
        assert_eq!(clean_path("\"C:\\packs\\x.tagdashpack\""), "C:\\packs\\x.tagdashpack");
    }
}
