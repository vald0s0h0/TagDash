// The single STT worker. Drains the persisted job queue one job at a time:
//   VAD/trim → whisper transcription → (online) Deepseek cleanup → dispatch.
// It pauses while the CPU is busy or the cash open is intense (so transcription
// never competes with the trading hot path), retries transient failures with
// backoff, and honours cancellation. Exactly one instance runs for the process.

use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use rusqlite::Connection;
use serde_json::json;
use sysinfo::System;
use tauri::{AppHandle, Emitter};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext};

use crate::config::secrets::Secrets;
use crate::config::AppConfig;
use crate::local_db::{journal_repository, journal_repository::JournalEntry};
use crate::market_state::MarketState;
use crate::tradetally;
use crate::types::LatencyLevel;

use super::{cleanup, model, queue, vad, JobKind, JobState, SttJob, SttShared, EV_CHANGED, EV_DIARY_RESULT};

const MAX_ATTEMPTS: u32 = 3;

pub fn spawn(
    shared: Arc<SttShared>,
    app: AppHandle,
    db: Arc<Mutex<Connection>>,
    secrets: Arc<RwLock<Secrets>>,
    config: Arc<RwLock<AppConfig>>,
    market: Arc<RwLock<MarketState>>,
) {
    tauri::async_runtime::spawn(async move {
        run(shared, app, db, secrets, config, market).await;
    });
}

async fn run(
    shared: Arc<SttShared>,
    app: AppHandle,
    db: Arc<Mutex<Connection>>,
    secrets: Arc<RwLock<Secrets>>,
    config: Arc<RwLock<AppConfig>>,
    market: Arc<RwLock<MarketState>>,
) {
    if !super::platform_available() {
        eprintln!("[tagdash] STT désactivé : macOS < 14 (BLAS crash whisper.cpp)");
        set_worker(&shared, &app, "paused", Some("non disponible sur ce Mac".into()));
        return;
    }

    let mut sys = System::new();
    loop {
        let Some(job_id) = first_queued_id(&shared) else {
            set_worker(&shared, &app, "idle", None);
            shared.wake.notified().await;
            continue;
        };

        if !config.read().unwrap().stt.enabled {
            set_worker(&shared, &app, "paused", Some("désactivé".into()));
            tokio::time::sleep(Duration::from_secs(3)).await;
            continue;
        }

        sys.refresh_cpu_usage();
        let cpu = sys.global_cpu_usage();
        if let Some(reason) = pause_reason(&config, &market, cpu) {
            set_worker(&shared, &app, "paused", Some(reason));
            tokio::time::sleep(Duration::from_secs(3)).await;
            continue;
        }

        let Some(mut job) = claim(&shared, &job_id) else {
            continue; // cancelled / taken meanwhile
        };
        set_worker(&shared, &app, "running", None);
        process_job(&shared, &app, &db, &secrets, &config, &market, &mut job).await;
    }
}

// ─── Queue helpers ──────────────────────────────────────────────────────────────

fn first_queued_id(shared: &SttShared) -> Option<String> {
    shared
        .jobs
        .lock()
        .unwrap()
        .iter()
        .find(|j| j.state == JobState::Queued)
        .map(|j| j.id.clone())
}

/// Mark a still-queued job as Running and return a clone; None if it vanished or was
/// cancelled in the meantime.
fn claim(shared: &SttShared, id: &str) -> Option<SttJob> {
    let mut job = {
        let mut jobs = shared.jobs.lock().unwrap();
        let slot = jobs.iter_mut().find(|j| j.id == id && j.state == JobState::Queued)?;
        slot.state = JobState::Running;
        slot.clone()
    };
    job.state = JobState::Running;
    let _ = queue::save_job(&shared.app_dir, &job);
    Some(job)
}

fn is_cancelled(shared: &SttShared, id: &str) -> bool {
    shared
        .jobs
        .lock()
        .unwrap()
        .iter()
        .find(|j| j.id == id)
        .map(|j| j.state == JobState::Cancelled)
        .unwrap_or(true)
}

// ─── Worker-state / events ──────────────────────────────────────────────────────

fn set_worker(shared: &SttShared, app: &AppHandle, state: &str, reason: Option<String>) {
    {
        let mut ws = shared.worker_state.lock().unwrap();
        ws.state = state.to_string();
        ws.paused_reason = reason;
    }
    let _ = app.emit(EV_CHANGED, ());
}

fn emit_changed(app: &AppHandle) {
    let _ = app.emit(EV_CHANGED, ());
}

// ─── Pause heuristic ────────────────────────────────────────────────────────────

fn pause_reason(config: &Arc<RwLock<AppConfig>>, market: &Arc<RwLock<MarketState>>, cpu: f32) -> Option<String> {
    let cfg = config.read().unwrap().stt.clone();
    if cpu > cfg.pause_cpu_pct {
        return Some(format!("CPU {cpu:.0}%"));
    }
    let now = crate::time::now();
    if crate::time::is_regular_session(now) {
        let mins = crate::time::et_minutes(now);
        if mins < 570 + cfg.pause_market_open_minutes {
            return Some("ouverture du marché".into());
        }
        let elevated = {
            let m = market.read().unwrap();
            !matches!(&m.latency.level, LatencyLevel::Normal)
        };
        if elevated {
            return Some("latence élevée".into());
        }
    }
    None
}

// ─── Per-job pipeline ───────────────────────────────────────────────────────────

async fn process_job(
    shared: &Arc<SttShared>,
    app: &AppHandle,
    db: &Arc<Mutex<Connection>>,
    secrets: &Arc<RwLock<Secrets>>,
    config: &Arc<RwLock<AppConfig>>,
    _market: &Arc<RwLock<MarketState>>,
    job: &mut SttJob,
) {
    let model_name = config.read().unwrap().stt.model.clone();

    // 1. Ensure the model is present (downloads on first job).
    if !model::is_present(shared, &model_name) {
        if let Err(e) = model::download_model(shared, &model_name).await {
            return retry(shared, app, job, format!("téléchargement modèle: {e}")).await;
        }
    }

    // 2. Load audio.
    let samples = match queue::read_wav(&shared.app_dir, &job.id) {
        Ok(s) => s,
        Err(e) => return terminal_error(shared, app, job, format!("lecture audio: {e}")),
    };

    // 3. VAD / trim. Pure silence → done (empty), nothing to send.
    let Some(trimmed) = vad::trim_silence(&samples) else {
        return finish_empty(shared, app, job);
    };

    // 4. Transcribe (blocking, off the async runtime).
    let (lang, jargon) = {
        let c = config.read().unwrap();
        (c.stt.language.clone(), c.stt.jargon.clone())
    };
    let prompt = jargon.join(", ");
    let ctx = match model::load_context(shared, &model_name) {
        Ok(c) => c,
        Err(e) => return retry(shared, app, job, e).await,
    };
    let text = match tokio::task::spawn_blocking(move || transcribe(ctx, trimmed, lang, prompt)).await {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => return retry(shared, app, job, e).await,
        Err(e) => return retry(shared, app, job, format!("transcription: {e}")).await,
    };
    if text.trim().is_empty() {
        return finish_empty(shared, app, job);
    }

    // Cancelled while transcribing? Drop without sending.
    if is_cancelled(shared, &job.id) {
        queue::delete_wav(&shared.app_dir, &job.id);
        return;
    }

    // 5. Online cleanup (offline → passthrough).
    let cleaned = cleanup::clean(secrets, job.kind, &text).await;
    job.text = Some(cleaned.text.clone());

    // 6. Dispatch by destination.
    match job.kind {
        JobKind::Trade => {
            let trade_id = job.trade_id.clone().unwrap_or_default();
            if trade_id.is_empty() {
                return terminal_error(shared, app, job, "trade_id manquant".into());
            }
            if let Err(e) = append_trade_note(db, &trade_id, job.symbol.as_deref(), &cleaned.text) {
                return retry(shared, app, job, e).await;
            }
            let _ = app.emit("stt-trade-note-saved", json!({ "trade_id": trade_id }));
        }
        JobKind::Diary => {
            // The dashboard card owns the day's running content; we emit the cleaned
            // block (+ optional title) and the frontend appends it + sends to TradeTally.
            let _ = app.emit(
                EV_DIARY_RESULT,
                json!({ "block": cleaned.text, "title": cleaned.title }),
            );
        }
    }

    // 7. Done.
    queue::delete_wav(&shared.app_dir, &job.id);
    job.state = JobState::Done;
    job.error = None;
    shared.update_job(job);
    emit_changed(app);
}

/// Read the trade's existing note, append the new block (never overwrite), persist,
/// and queue the TradeTally `note_updated` event.
fn append_trade_note(
    db: &Arc<Mutex<Connection>>,
    trade_id: &str,
    symbol: Option<&str>,
    block: &str,
) -> Result<(), String> {
    let conn = db.lock().unwrap();
    let existing = journal_repository::get(&conn, trade_id).ok().flatten();
    let (prev, confidence, tags, sym) = match existing {
        Some(e) => {
            let sym = symbol.filter(|s| !s.is_empty()).map(str::to_string).unwrap_or(e.symbol);
            (e.notes, e.confidence, e.tags, sym)
        }
        None => (String::new(), None, Vec::new(), symbol.unwrap_or("").to_string()),
    };
    let next = if prev.trim().is_empty() {
        block.to_string()
    } else {
        format!("{prev}\n\n{block}")
    };
    let entry = JournalEntry {
        trade_id: trade_id.to_string(),
        symbol: sym.clone(),
        notes: next.clone(),
        confidence,
        tags: tags.clone(),
        updated_at: String::new(),
    };
    journal_repository::save(&conn, &entry).map_err(|e| e.to_string())?;
    tradetally::enqueue_note_updated(&conn, trade_id, &sym, &next, confidence, &tags);
    Ok(())
}

fn finish_empty(shared: &SttShared, app: &AppHandle, job: &mut SttJob) {
    queue::delete_wav(&shared.app_dir, &job.id);
    job.state = JobState::Done;
    job.text = Some(String::new());
    job.error = Some("aucune parole détectée".into());
    shared.update_job(job);
    emit_changed(app);
}

async fn retry(shared: &SttShared, app: &AppHandle, job: &mut SttJob, err: String) {
    job.attempts += 1;
    job.error = Some(err);
    if job.attempts >= MAX_ATTEMPTS {
        job.state = JobState::Error;
        shared.update_job(job);
        emit_changed(app);
        return;
    }
    job.state = JobState::Queued;
    shared.update_job(job);
    emit_changed(app);
    let backoff = Duration::from_secs(2u64.pow(job.attempts.min(4)));
    tokio::time::sleep(backoff).await;
    shared.wake.notify_one();
}

fn terminal_error(shared: &SttShared, app: &AppHandle, job: &mut SttJob, err: String) {
    job.state = JobState::Error;
    job.error = Some(err);
    shared.update_job(job);
    emit_changed(app);
}

/// Run whisper on a 16 kHz mono clip. French forced, jargon fed as the initial
/// prompt to bias recognition. Returns the joined segment text.
fn transcribe(
    ctx: Arc<WhisperContext>,
    samples: Vec<f32>,
    lang: String,
    prompt: String,
) -> Result<String, String> {
    let mut state = ctx.create_state().map_err(|e| e.to_string())?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some(&lang));
    params.set_translate(false);
    if !prompt.is_empty() {
        params.set_initial_prompt(&prompt);
    }
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_print_special(false);

    state.full(params, &samples).map_err(|e| e.to_string())?;
    let n = state.full_n_segments();
    let mut out = String::new();
    for i in 0..n {
        if let Some(seg) = state.get_segment(i) {
            if let Ok(text) = seg.to_str_lossy() {
                out.push_str(text.trim());
                out.push(' ');
            }
        }
    }
    Ok(out.trim().to_string())
}
