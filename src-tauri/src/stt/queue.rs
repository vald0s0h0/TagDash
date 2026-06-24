// Persisted STT job queue. Each queued dictée is two files under `<app_dir>/stt_jobs/`:
//   <id>.wav   — the captured audio, already resampled to 16 kHz mono f32.
//   <id>.json  — the SttJob metadata (kind, destination, state, attempts…).
// The queue is reloaded at startup so a dictée left mid-flight (e.g. the worker was
// paused over the cash open when the app closed) is never lost. The WAV is removed
// once the job reaches a terminal state; the JSON lingers briefly for the panel.

use std::path::Path;

use hound::{SampleFormat, WavReader, WavSpec, WavWriter};

use super::{jobs_dir, JobState, SttJob, WHISPER_SAMPLE_RATE};

fn job_json(app_dir: &Path, id: &str) -> std::path::PathBuf {
    jobs_dir(app_dir).join(format!("{id}.json"))
}

fn job_wav(app_dir: &Path, id: &str) -> std::path::PathBuf {
    jobs_dir(app_dir).join(format!("{id}.wav"))
}

/// Write the 16 kHz mono clip for a job (atomically: `.tmp` then rename).
pub fn write_wav(app_dir: &Path, id: &str, samples: &[f32]) -> Result<(), String> {
    std::fs::create_dir_all(jobs_dir(app_dir)).map_err(|e| e.to_string())?;
    let path = job_wav(app_dir, id);
    let tmp = path.with_extension("wav.tmp");
    let spec = WavSpec {
        channels: 1,
        sample_rate: WHISPER_SAMPLE_RATE,
        bits_per_sample: 32,
        sample_format: SampleFormat::Float,
    };
    let mut writer = WavWriter::create(&tmp, spec).map_err(|e| e.to_string())?;
    for &s in samples {
        writer.write_sample(s).map_err(|e| e.to_string())?;
    }
    writer.finalize().map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

/// Read a job's 16 kHz mono clip back as f32 samples.
pub fn read_wav(app_dir: &Path, id: &str) -> Result<Vec<f32>, String> {
    let mut reader = WavReader::open(job_wav(app_dir, id)).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    let samples: Vec<f32> = match spec.sample_format {
        SampleFormat::Float => reader.samples::<f32>().filter_map(Result::ok).collect(),
        SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .filter_map(Result::ok)
                .map(|s| s as f32 / max)
                .collect()
        }
    };
    Ok(samples)
}

/// Persist a job's metadata JSON.
pub fn save_job(app_dir: &Path, job: &SttJob) -> Result<(), String> {
    std::fs::create_dir_all(jobs_dir(app_dir)).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(job).map_err(|e| e.to_string())?;
    std::fs::write(job_json(app_dir, &job.id), json).map_err(|e| e.to_string())
}

/// Remove both files for a job (best effort).
pub fn delete_job_files(app_dir: &Path, id: &str) {
    let _ = std::fs::remove_file(job_json(app_dir, id));
    let _ = std::fs::remove_file(job_wav(app_dir, id));
}

/// Delete only the audio WAV (kept the JSON for the panel after a job is done).
pub fn delete_wav(app_dir: &Path, id: &str) {
    let _ = std::fs::remove_file(job_wav(app_dir, id));
}

/// Reload jobs at startup. Jobs that were `Running` when the app stopped are reset
/// to `Queued` so the worker retries them. Terminal jobs whose WAV is gone are kept
/// for the panel; queued jobs whose WAV vanished are dropped.
pub fn load_persisted(app_dir: &Path) -> Vec<SttJob> {
    let dir = jobs_dir(app_dir);
    let _ = std::fs::create_dir_all(&dir);
    let mut jobs: Vec<SttJob> = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return jobs;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&p) else { continue };
        let Ok(mut job) = serde_json::from_str::<SttJob>(&content) else { continue };
        match job.state {
            JobState::Running => {
                job.state = JobState::Queued;
                let _ = save_job(app_dir, &job);
            }
            JobState::Queued if !job_wav(app_dir, &job.id).exists() => {
                // Lost audio — drop it.
                delete_job_files(app_dir, &job.id);
                continue;
            }
            _ => {}
        }
        jobs.push(job);
    }
    // Oldest first so the worker drains in creation order.
    jobs.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    jobs
}
