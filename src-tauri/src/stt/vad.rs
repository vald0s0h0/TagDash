// Lightweight voice-activity gate + resampling. whisper wants 16 kHz mono f32, so
// we (1) linearly resample the captured audio to 16 kHz and (2) run a simple RMS
// energy gate that trims leading/trailing silence and skips clips that are pure
// silence (so an accidental tap never produces an empty note). This is the robust
// fallback path; whisper.cpp's built-in Silero VAD can be layered on later.

/// Linear-resample mono samples to 16 kHz. Cheap and more than enough for speech.
pub fn resample_to_16k(samples: &[f32], src_rate: u32) -> Vec<f32> {
    if samples.is_empty() || src_rate == super::WHISPER_SAMPLE_RATE {
        return samples.to_vec();
    }
    let ratio = super::WHISPER_SAMPLE_RATE as f32 / src_rate as f32;
    let out_len = (samples.len() as f32 * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_pos = i as f32 / ratio;
        let idx = src_pos.floor() as usize;
        let frac = src_pos - idx as f32;
        let a = samples.get(idx).copied().unwrap_or(0.0);
        let b = samples.get(idx + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
}

/// RMS noise gate (0..1). Below this, a 16 kHz clip is treated as silence.
const GATE: f32 = 0.012;
/// Padding kept around detected speech (seconds).
const PAD_SECS: f32 = 0.20;

/// Trim leading/trailing silence from a 16 kHz mono clip. Returns `None` when the
/// whole clip is below the gate (nothing was said).
pub fn trim_silence(samples: &[f32]) -> Option<Vec<f32>> {
    if samples.is_empty() {
        return None;
    }
    let frame = (super::WHISPER_SAMPLE_RATE as usize / 50).max(160); // ~20 ms
    let pad = (PAD_SECS * super::WHISPER_SAMPLE_RATE as f32) as usize;

    let mut first: Option<usize> = None;
    let mut last: usize = 0;
    let mut idx = 0;
    while idx < samples.len() {
        let end = (idx + frame).min(samples.len());
        let mut e = 0.0f32;
        for &s in &samples[idx..end] {
            e += s * s;
        }
        let rms = (e / (end - idx) as f32).sqrt();
        if rms >= GATE {
            if first.is_none() {
                first = Some(idx);
            }
            last = end;
        }
        idx += frame;
    }

    let start = first?;
    let from = start.saturating_sub(pad);
    let to = (last + pad).min(samples.len());
    if to <= from {
        return None;
    }
    Some(samples[from..to].to_vec())
}
