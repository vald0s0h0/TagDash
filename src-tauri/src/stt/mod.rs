// Speech-to-Text dictée pipeline — offline (whisper.cpp via whisper-rs), no API.
//
// The user presses a mic button, dictates a note, and TagDash transcribes it
// LOCALLY (offline-capable), optionally cleans it through Deepseek when online,
// then routes it to either a trade note (by trade_id) or the day's diary (by ET
// date) and sends it to TradeTally via the existing resilient queue.
//
// Shape: capture is fast and interactive; transcription is slow and must NEVER
// compete with the trading hot path. So a STOP enqueues a persisted job and a
// SINGLE background worker drains the queue serially, pausing when the CPU is busy
// or the cash open is intense, retrying on error, and supporting cancellation.
//
//   recorder.rs — native cpal capture on a dedicated thread (Stream is !Send),
//                 + a live FFT spectrum emitted to the UI while recording.
//   model.rs    — download the ggml model on first use + cache the WhisperContext.
//   queue.rs    — persisted job queue (one WAV + one JSON per job under stt_jobs/).
//   worker.rs   — the single worker loop (VAD → whisper → Deepseek → dispatch).
//   vad.rs      — RMS silence trim/gate + resampling to whisper's 16 kHz mono.
//   cleanup.rs  — Deepseek cleanup (trade → text ; diary → {title, content}).

pub mod cleanup;
pub mod model;
pub mod queue;
pub mod recorder;
pub mod vad;
pub mod worker;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use tokio::sync::Notify;
use whisper_rs::WhisperContext;

use recorder::Recorder;

/// whisper transcribes 16 kHz mono f32 — every WAV we persist is already at this rate.
pub const WHISPER_SAMPLE_RATE: u32 = 16_000;
/// How many finished jobs we keep visible in the queue panel before pruning.
const KEEP_FINISHED: usize = 12;
/// Tauri event names (hyphenated — valid everywhere). The frontend listens to these.
pub const EV_CHANGED: &str = "stt-changed";
pub const EV_SPECTRUM: &str = "stt-spectrum";
pub const EV_DIARY_RESULT: &str = "stt-diary-result";

// ─── Platform guard ──────────────────────────────────────────────────────────
//
// whisper.cpp's BLAS backend crashes (NULL fn-ptr in ggml_backend_blas_graph_compute)
// on macOS ≤ 13 with the old Accelerate framework shipped on Intel Macs.  Disable
// the entire STT pipeline there; Apple-Silicon Macs on macOS ≥ 14 are fine.

/// Returns `true` when the current platform can safely run whisper transcription.
pub fn platform_available() -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        // ProductVersion → "14.5" / "12.7.6" etc.
        let ok = Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|v| v.trim().split('.').next().map(String::from))
            .and_then(|maj| maj.parse::<u32>().ok())
            .map(|maj| maj >= 14)
            .unwrap_or(false);
        return ok;
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

// ─── Paths ────────────────────────────────────────────────────────────────────

pub fn models_dir(app_dir: &Path) -> PathBuf {
    app_dir.join("models")
}

pub fn model_path(app_dir: &Path, model: &str) -> PathBuf {
    models_dir(app_dir).join(format!("ggml-{model}.bin"))
}

pub fn jobs_dir(app_dir: &Path) -> PathBuf {
    app_dir.join("stt_jobs")
}

// ─── Job model ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Trade,
    Diary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Done,
    Error,
    Cancelled,
}

impl Default for JobState {
    fn default() -> Self {
        JobState::Queued
    }
}

/// One queued dictée. Persisted as `<jobs_dir>/<id>.json` next to `<id>.wav`, so the
/// queue survives a restart. `text` is filled once transcribed (kept for the panel
/// preview); the audio WAV is deleted once the job reaches `Done`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttJob {
    pub id: String,
    pub kind: JobKind,
    #[serde(default)]
    pub trade_id: Option<String>,
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub state: JobState,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    pub created_at: String,
}

/// Destination context of the in-progress recording — turned into an `SttJob` when
/// the user stops recording.
#[derive(Debug, Clone)]
pub struct PendingRecording {
    pub kind: JobKind,
    pub trade_id: Option<String>,
    pub symbol: Option<String>,
}

// ─── Worker / status views ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct WorkerState {
    /// "idle" | "running" | "paused"
    pub state: String,
    pub paused_reason: Option<String>,
}

/// Snapshot polled by the frontend (`stt_status`).
#[derive(Debug, Clone, Serialize)]
pub struct SttStatus {
    pub enabled: bool,
    /// `false` on macOS ≤ 13 where whisper's BLAS backend crashes.
    pub platform_available: bool,
    pub model: String,
    pub model_present: bool,
    pub downloading: bool,
    pub download_progress: f32,
    pub recording: bool,
    pub recording_kind: Option<JobKind>,
    pub worker_state: String,
    pub paused_reason: Option<String>,
    pub jobs: Vec<SttJob>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MicTestResult {
    pub ok: bool,
    /// Peak level over the short probe (0..1) — drives the "I can hear you" meter.
    pub level: f32,
    pub device: Option<String>,
    pub error: Option<String>,
}

// ─── Shared state (lives in AppState) ─────────────────────────────────────────────

pub struct SttShared {
    pub app_dir: PathBuf,
    /// FIFO of pending + recently-finished jobs (capped; finished pruned to KEEP_FINISHED).
    pub jobs: Mutex<VecDeque<SttJob>>,
    /// Live recorder session (None when idle).
    pub recorder: Mutex<Option<Recorder>>,
    /// Destination of the in-progress recording.
    pub pending: Mutex<Option<PendingRecording>>,
    /// Loaded WhisperContext, built lazily once the model file is present.
    pub model_ctx: Mutex<Option<Arc<WhisperContext>>>,
    pub downloading: AtomicBool,
    pub download_progress: Mutex<f32>,
    pub worker_state: Mutex<WorkerState>,
    pub last_error: Mutex<Option<String>>,
    /// Wakes the worker when a new job is enqueued.
    pub wake: Notify,
}

impl SttShared {
    pub fn new(app_dir: PathBuf) -> Self {
        // Reload any jobs left over from a previous run (running → re-queued).
        let persisted = queue::load_persisted(&app_dir);
        Self {
            app_dir,
            jobs: Mutex::new(persisted.into_iter().collect()),
            recorder: Mutex::new(None),
            pending: Mutex::new(None),
            model_ctx: Mutex::new(None),
            downloading: AtomicBool::new(false),
            download_progress: Mutex::new(0.0),
            worker_state: Mutex::new(WorkerState { state: "idle".into(), paused_reason: None }),
            last_error: Mutex::new(None),
            wake: Notify::new(),
        }
    }

    pub fn is_recording(&self) -> bool {
        self.recorder.lock().unwrap().is_some()
    }

    /// Append a job and wake the worker.
    pub fn enqueue(&self, job: SttJob) {
        let _ = queue::save_job(&self.app_dir, &job);
        self.jobs.lock().unwrap().push_back(job);
        self.wake.notify_one();
    }

    /// Persist + mirror a job's new state into the in-RAM deque.
    pub fn update_job(&self, job: &SttJob) {
        let _ = queue::save_job(&self.app_dir, job);
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(slot) = jobs.iter_mut().find(|j| j.id == job.id) {
            *slot = job.clone();
        }
        prune_finished(&mut jobs, &self.app_dir);
    }

    pub fn status(&self, cfg: &crate::config::SttConfig) -> SttStatus {
        let model_present = model_path(&self.app_dir, &cfg.model).exists();
        let ws = self.worker_state.lock().unwrap().clone();
        let recording_kind = self.pending.lock().unwrap().as_ref().map(|p| p.kind);
        SttStatus {
            enabled: cfg.enabled,
            platform_available: platform_available(),
            model: cfg.model.clone(),
            model_present,
            downloading: self.downloading.load(std::sync::atomic::Ordering::Relaxed),
            download_progress: *self.download_progress.lock().unwrap(),
            recording: self.is_recording(),
            recording_kind,
            worker_state: ws.state,
            paused_reason: ws.paused_reason,
            jobs: self.jobs.lock().unwrap().iter().cloned().collect(),
            error: self.last_error.lock().unwrap().clone(),
        }
    }
}

/// Keep at most `KEEP_FINISHED` terminal jobs (done/error/cancelled) in the deque,
/// deleting the on-disk files of the ones we drop.
fn prune_finished(jobs: &mut VecDeque<SttJob>, app_dir: &Path) {
    let finished: Vec<String> = jobs
        .iter()
        .filter(|j| matches!(j.state, JobState::Done | JobState::Error | JobState::Cancelled))
        .map(|j| j.id.clone())
        .collect();
    if finished.len() <= KEEP_FINISHED {
        return;
    }
    let drop_n = finished.len() - KEEP_FINISHED;
    for id in finished.into_iter().take(drop_n) {
        queue::delete_job_files(app_dir, &id);
        jobs.retain(|j| j.id != id);
    }
}

// ─── Recording orchestration (called by the Tauri commands) ──────────────────────

/// Open the mic and start capturing for `kind`, remembering the destination.
pub fn start_recording(
    shared: &Arc<SttShared>,
    app: AppHandle,
    kind: JobKind,
    trade_id: Option<String>,
    symbol: Option<String>,
    device: Option<String>,
) -> Result<(), String> {
    // Drop any recorder still held — the UI enforces a single mic, so one lingering
    // here is stale (e.g. a component unmounted mid-recording). Self-heals the
    // pipeline instead of wedging it with "déjà en cours". Take outside the lock so
    // the join in cancel() never holds it.
    let stale = shared.recorder.lock().unwrap().take();
    if let Some(old) = stale {
        old.cancel();
    }
    *shared.pending.lock().unwrap() = None;

    let rec = Recorder::start(app.clone(), device, kind)?;
    *shared.recorder.lock().unwrap() = Some(rec);
    *shared.pending.lock().unwrap() = Some(PendingRecording { kind, trade_id, symbol });
    let _ = app.emit(EV_CHANGED, ());
    Ok(())
}

/// Stop capturing, resample to 16 kHz, persist the WAV and enqueue the job. Returns
/// the new job id.
pub fn stop_recording(shared: &Arc<SttShared>, app: AppHandle) -> Result<String, String> {
    let rec = shared
        .recorder
        .lock()
        .unwrap()
        .take()
        .ok_or("aucun enregistrement en cours")?;
    let pending = shared
        .pending
        .lock()
        .unwrap()
        .take()
        .ok_or("contexte d'enregistrement manquant")?;

    let (samples, rate) = rec.stop();
    let pcm = vad::resample_to_16k(&samples, rate);
    let id = format!("stt-{}", Utc::now().timestamp_millis());
    queue::write_wav(&shared.app_dir, &id, &pcm)?;

    shared.enqueue(SttJob {
        id: id.clone(),
        kind: pending.kind,
        trade_id: pending.trade_id,
        symbol: pending.symbol,
        state: JobState::Queued,
        attempts: 0,
        error: None,
        text: None,
        created_at: Utc::now().to_rfc3339(),
    });
    let _ = app.emit(EV_CHANGED, ());
    Ok(id)
}

/// Stop and discard the in-progress recording (cancel button).
pub fn cancel_recording(shared: &Arc<SttShared>, app: AppHandle) {
    if let Some(rec) = shared.recorder.lock().unwrap().take() {
        rec.cancel();
    }
    *shared.pending.lock().unwrap() = None;
    let _ = app.emit(EV_CHANGED, ());
}

/// Cancel a queued/running job (running ones finish in the worker but won't send).
pub fn cancel_job(shared: &Arc<SttShared>, id: &str, app: &AppHandle) {
    {
        let mut jobs = shared.jobs.lock().unwrap();
        if let Some(j) = jobs.iter_mut().find(|j| j.id == id) {
            if matches!(j.state, JobState::Queued | JobState::Running) {
                j.state = JobState::Cancelled;
                let _ = queue::save_job(&shared.app_dir, j);
                queue::delete_wav(&shared.app_dir, id);
            }
        }
    }
    let _ = app.emit(EV_CHANGED, ());
}

/// Re-queue a failed job for another attempt (only if its audio is still on disk).
pub fn retry_job(shared: &Arc<SttShared>, id: &str, app: &AppHandle) {
    let mut requeued = false;
    {
        let mut jobs = shared.jobs.lock().unwrap();
        if let Some(j) = jobs.iter_mut().find(|j| j.id == id) {
            if j.state == JobState::Error
                && jobs_dir(&shared.app_dir).join(format!("{id}.wav")).exists()
            {
                j.state = JobState::Queued;
                j.attempts = 0;
                j.error = None;
                let _ = queue::save_job(&shared.app_dir, j);
                requeued = true;
            }
        }
    }
    if requeued {
        shared.wake.notify_one();
        let _ = app.emit(EV_CHANGED, ());
    }
}
