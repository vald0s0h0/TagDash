// Whisper model lifecycle: download the ggml model on first use (into
// `<app_dir>/models/`, written atomically), and cache the loaded WhisperContext so
// the ~1 s load happens only once. CPU build everywhere; Metal on macOS (see
// Cargo.toml). The model survives app auto-updates because it lives in app-data.

use std::io::Write;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use whisper_rs::{WhisperContext, WhisperContextParameters};

use super::{model_path, models_dir, SttShared};

/// Hugging Face ggml weights (the public model repo whisper.cpp's own downloader
/// uses). Note: `ggml-org/whisper.cpp` is gated (401) — the `.bin` files live under
/// `ggerganov/whisper.cpp`.
fn model_url(model: &str) -> String {
    format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{model}.bin")
}

/// True once the model file for `model` is on disk.
pub fn is_present(shared: &SttShared, model: &str) -> bool {
    model_path(&shared.app_dir, model).exists()
}

/// Download the model if absent. Idempotent and single-flight (a second call while
/// a download is in progress just returns). Reports progress into
/// `shared.download_progress` (0..1).
pub async fn download_model(shared: &Arc<SttShared>, model: &str) -> Result<(), String> {
    let path = model_path(&shared.app_dir, model);
    if path.exists() {
        return Ok(());
    }
    // Single-flight guard.
    if shared.downloading.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    *shared.download_progress.lock().unwrap() = 0.0;
    *shared.last_error.lock().unwrap() = None;
    let res = download_inner(shared, model).await;
    shared.downloading.store(false, Ordering::SeqCst);
    if let Err(e) = &res {
        *shared.last_error.lock().unwrap() = Some(e.clone());
    }
    res
}

async fn download_inner(shared: &Arc<SttShared>, model: &str) -> Result<(), String> {
    let dir = models_dir(&shared.app_dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = model_path(&shared.app_dir, model);
    let tmp = path.with_extension("bin.tmp");

    let resp = crate::http::client()
        .get(model_url(model))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("download HTTP {}", resp.status()));
    }
    let total = resp.content_length().unwrap_or(0);

    let mut file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    let mut downloaded: u64 = 0;
    let mut resp = resp;
    while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        downloaded += chunk.len() as u64;
        if total > 0 {
            *shared.download_progress.lock().unwrap() = downloaded as f32 / total as f32;
        }
    }
    file.flush().map_err(|e| e.to_string())?;
    drop(file);
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    *shared.download_progress.lock().unwrap() = 1.0;
    Ok(())
}

/// Return the cached WhisperContext, building (and caching) it on first call. Errors
/// when the model file is not present yet.
pub fn load_context(shared: &Arc<SttShared>, model: &str) -> Result<Arc<WhisperContext>, String> {
    if let Some(ctx) = shared.model_ctx.lock().unwrap().clone() {
        return Ok(ctx);
    }
    let path = model_path(&shared.app_dir, model);
    if !path.exists() {
        return Err("modèle whisper non téléchargé".into());
    }
    let ctx = WhisperContext::new_with_params(
        path.to_string_lossy().as_ref(),
        WhisperContextParameters::default(),
    )
    .map_err(|e| format!("whisper load: {e}"))?;
    let arc = Arc::new(ctx);
    *shared.model_ctx.lock().unwrap() = Some(arc.clone());
    Ok(arc)
}
