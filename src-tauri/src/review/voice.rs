// Voice notes of a review session — archived for the replay, timed for subtitles.
//
// A recording is cut into VOICED CHUNKS (RMS gate, short pauses kept inside a
// chunk, long silences dropped). The chunks are archived back to back as 16 kHz
// mono mu-law — one byte per sample, ~1 MB per minute of actual speech, decoded by
// the review player — and each chunk remembers the market instant it started at.
// Whisper transcribes the same concatenated audio, so a subtitle's offset maps back
// through the chunk table to the market clock exactly like the audio does.
//
// Market Replay: the recording runs on the real clock while the market clock may
// run faster. A chunk's market start is `start_ms + real_offset × speed` (the speed
// sampled when recording started).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::stt::WHISPER_SAMPLE_RATE;

/// RMS gate on 20 ms frames (same threshold as the dictée trim).
const GATE: f32 = 0.012;
/// A pause shorter than this stays inside the chunk (breathing, hesitations).
const HANGOVER_SECS: f32 = 0.8;
/// Audio kept around each chunk so words are not clipped.
const PAD_SECS: f32 = 0.2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chunk {
    /// Market instant the chunk starts at (epoch ms).
    pub start_ms: i64,
    /// Byte (= sample) offset of the chunk inside the archive.
    pub offset: usize,
    /// Length in samples (16 kHz).
    pub samples: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subtitle {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

/// Voiced ranges `[from, to)` (sample indices) of a 16 kHz clip.
pub fn voiced_ranges(pcm: &[f32]) -> Vec<(usize, usize)> {
    let rate = WHISPER_SAMPLE_RATE as usize;
    let frame = rate / 50;
    let hang = (HANGOVER_SECS * rate as f32) as usize;
    let pad = (PAD_SECS * rate as f32) as usize;
    let mut raw: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < pcm.len() {
        let end = (i + frame).min(pcm.len());
        let e: f32 = pcm[i..end].iter().map(|s| s * s).sum();
        let rms = (e / (end - i).max(1) as f32).sqrt();
        if rms >= GATE {
            match raw.last_mut() {
                Some(last) if i <= last.1 + hang => last.1 = end,
                _ => raw.push((i, end)),
            }
        }
        i += frame;
    }
    // Pad, then merge anything the padding made overlap.
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (a, b) in raw {
        let a = a.saturating_sub(pad);
        let b = (b + pad).min(pcm.len());
        match out.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// G.711 mu-law encode of one sample in [-1, 1].
pub fn ulaw_encode(sample: f32) -> u8 {
    const BIAS: i32 = 0x84;
    const CLIP: i32 = 32_635;
    let mut pcm = (sample.clamp(-1.0, 1.0) * 32_767.0) as i32;
    let sign = if pcm < 0 {
        pcm = -pcm;
        0x80
    } else {
        0
    };
    pcm = pcm.min(CLIP) + BIAS;
    let mut exponent = 7;
    let mut mask = 0x4000;
    while exponent > 0 && (pcm & mask) == 0 {
        exponent -= 1;
        mask >>= 1;
    }
    let mantissa = (pcm >> (exponent + 3)) & 0x0F;
    !((sign | (exponent << 4) | mantissa) as u8)
}

pub struct Archived {
    /// The voiced audio, concatenated — what whisper transcribes.
    pub pcm: Vec<f32>,
    pub chunks: Vec<Chunk>,
    pub path: PathBuf,
    pub end_ms: i64,
}

pub fn audio_path(app_dir: &Path, day: &str, voice_id: &str) -> PathBuf {
    super::reviews_dir(app_dir).join(day).join(format!("{voice_id}.ulaw"))
}

/// Cut the silences out of `pcm` (16 kHz), write the mu-law archive and return the
/// chunk table. `None` when nothing was said.
pub fn archive(
    app_dir: &Path,
    day: &str,
    voice_id: &str,
    pcm: &[f32],
    start_ms: i64,
    speed: f64,
) -> Result<Option<Archived>, String> {
    let ranges = voiced_ranges(pcm);
    if ranges.is_empty() {
        return Ok(None);
    }
    let rate = WHISPER_SAMPLE_RATE as f64;
    let mut concat: Vec<f32> = Vec::new();
    let mut chunks = Vec::with_capacity(ranges.len());
    for (a, b) in &ranges {
        let real_offset_ms = (*a as f64 / rate) * 1000.0;
        chunks.push(Chunk {
            start_ms: start_ms + (real_offset_ms * speed.max(0.1)).round() as i64,
            offset: concat.len(),
            samples: b - a,
        });
        concat.extend_from_slice(&pcm[*a..*b]);
    }
    let path = audio_path(app_dir, day, voice_id);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let bytes: Vec<u8> = concat.iter().map(|s| ulaw_encode(*s)).collect();
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    let end_ms = start_ms + ((pcm.len() as f64 / rate) * 1000.0 * speed.max(0.1)).round() as i64;
    Ok(Some(Archived {
        pcm: concat,
        chunks,
        path,
        end_ms,
    }))
}

/// Map an offset inside the concatenated audio (ms) to the market clock.
pub fn to_market_ms(chunks: &[Chunk], concat_ms: i64, speed: f64) -> i64 {
    let rate = WHISPER_SAMPLE_RATE as f64;
    let sample = ((concat_ms.max(0) as f64 / 1000.0) * rate) as usize;
    let chunk = chunks
        .iter()
        .rev()
        .find(|c| c.offset <= sample)
        .or_else(|| chunks.first());
    match chunk {
        Some(c) => {
            let inside_ms = ((sample.saturating_sub(c.offset)) as f64 / rate) * 1000.0;
            c.start_ms + (inside_ms * speed.max(0.1)).round() as i64
        }
        None => concat_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_between_two_phrases_is_cut() {
        let rate = WHISPER_SAMPLE_RATE as usize;
        let tone = |n: usize| (0..n).map(|i| ((i as f32) * 0.3).sin() * 0.2).collect::<Vec<_>>();
        let mut pcm = tone(rate); // 1 s speech
        pcm.extend(vec![0.0; rate * 3]); // 3 s silence
        pcm.extend(tone(rate)); // 1 s speech
        let r = voiced_ranges(&pcm);
        assert_eq!(r.len(), 2);
        let kept: usize = r.iter().map(|(a, b)| b - a).sum();
        assert!(kept < rate * 3, "silence must be dropped, kept {kept}");
    }

    #[test]
    fn subtitle_offsets_map_back_to_the_market_clock() {
        let chunks = vec![
            Chunk { start_ms: 1_000_000, offset: 0, samples: 16_000 },
            Chunk { start_ms: 1_010_000, offset: 16_000, samples: 16_000 },
        ];
        // 1.5 s into the concatenated audio = 0.5 s into the second chunk.
        assert_eq!(to_market_ms(&chunks, 1_500, 1.0), 1_010_500);
        // A 2× replay stretches the market clock.
        assert_eq!(to_market_ms(&chunks, 1_500, 2.0), 1_011_000);
    }

    #[test]
    fn ulaw_is_monotonic_and_symmetric() {
        assert_eq!(ulaw_encode(0.0), 0xFF);
        assert_ne!(ulaw_encode(0.5), ulaw_encode(-0.5));
        assert_eq!(ulaw_encode(0.5) & 0x7F, ulaw_encode(-0.5) & 0x7F);
    }
}
