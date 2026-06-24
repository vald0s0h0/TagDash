// Native microphone capture via cpal. The cpal `Stream` is `!Send`, so it lives on
// its own thread for the whole recording; the calling command keeps only a small
// `Recorder` handle (sample buffer + stop channel). While recording, the thread
// also emits a live FFT spectrum (`stt-spectrum`) so the UI can show "it's hearing
// you" — the capture stays single-source (no second mic tap in the webview).

use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rustfft::{num_complex::Complex, FftPlanner};
use tauri::{AppHandle, Emitter};

use super::JobKind;

/// Number of spectrum bars sent to the UI.
const SPECTRUM_BINS: usize = 32;
/// FFT window (samples) taken from the tail of the capture buffer.
const FFT_WINDOW: usize = 1024;

pub struct Recorder {
    pub kind: JobKind,
    samples: Arc<Mutex<Vec<f32>>>,
    sample_rate: u32,
    stop_tx: mpsc::Sender<()>,
    handle: Option<JoinHandle<()>>,
}

impl Recorder {
    /// Open the input device and start capturing. Blocks only until the audio thread
    /// reports the resolved sample rate (or an error).
    pub fn start(app: AppHandle, device_name: Option<String>, kind: JobKind) -> Result<Recorder, String> {
        let samples = Arc::new(Mutex::new(Vec::<f32>::new()));
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<u32, String>>();
        let samples_thread = samples.clone();

        let handle = std::thread::spawn(move || {
            match build_stream(device_name.as_deref(), samples_thread.clone()) {
                Ok((stream, sample_rate)) => {
                    if let Err(e) = stream.play() {
                        let _ = ready_tx.send(Err(format!("stream play: {e}")));
                        return;
                    }
                    let _ = ready_tx.send(Ok(sample_rate));
                    // Spectrum loop until a stop signal arrives.
                    let mut planner = FftPlanner::<f32>::new();
                    let fft = planner.plan_fft_forward(FFT_WINDOW);
                    loop {
                        match stop_rx.recv_timeout(Duration::from_millis(45)) {
                            Ok(_) | Err(RecvTimeoutError::Disconnected) => break,
                            Err(RecvTimeoutError::Timeout) => {
                                emit_spectrum(&app, &samples_thread, &*fft);
                            }
                        }
                    }
                    drop(stream);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        });

        match ready_rx.recv() {
            Ok(Ok(sample_rate)) => Ok(Recorder {
                kind,
                samples,
                sample_rate,
                stop_tx,
                handle: Some(handle),
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err("recorder thread failed to start".into()),
        }
    }

    /// Stop capturing and return the captured mono samples + their sample rate.
    pub fn stop(mut self) -> (Vec<f32>, u32) {
        let _ = self.stop_tx.send(());
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let samples = std::mem::take(&mut *self.samples.lock().unwrap());
        (samples, self.sample_rate)
    }

    /// Stop and discard (cancel).
    pub fn cancel(mut self) {
        let _ = self.stop_tx.send(());
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Build a mono-accumulating input stream on the chosen (or default) device.
fn build_stream(
    device_name: Option<&str>,
    samples: Arc<Mutex<Vec<f32>>>,
) -> Result<(cpal::Stream, u32), String> {
    let host = cpal::default_host();
    let device = match device_name {
        Some(name) => host
            .input_devices()
            .map_err(|e| e.to_string())?
            .find(|d| d.name().map(|n| n == name).unwrap_or(false))
            .ok_or_else(|| format!("input device '{name}' not found"))?,
        None => host
            .default_input_device()
            .ok_or("no default input device")?,
    };

    let config = device.default_input_config().map_err(|e| e.to_string())?;
    let sample_rate = config.sample_rate().0;
    let channels = config.channels() as usize;
    let err_fn = |e| eprintln!("[stt] input stream error: {e}");
    let stream_config: cpal::StreamConfig = config.clone().into();

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| append_mono(&samples, data, channels, |s| s),
            err_fn,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                append_mono(&samples, data, channels, |s| s as f32 / 32768.0)
            },
            err_fn,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            &stream_config,
            move |data: &[u16], _: &cpal::InputCallbackInfo| {
                append_mono(&samples, data, channels, |s| (s as f32 - 32768.0) / 32768.0)
            },
            err_fn,
            None,
        ),
        other => return Err(format!("unsupported sample format: {other:?}")),
    }
    .map_err(|e| e.to_string())?;

    Ok((stream, sample_rate))
}

/// Down-mix interleaved frames to mono and append to the shared buffer.
fn append_mono<T: Copy>(samples: &Arc<Mutex<Vec<f32>>>, data: &[T], channels: usize, to_f32: impl Fn(T) -> f32) {
    if channels == 0 {
        return;
    }
    let mut buf = samples.lock().unwrap();
    for frame in data.chunks(channels) {
        let sum: f32 = frame.iter().map(|&s| to_f32(s)).sum();
        buf.push(sum / channels as f32);
    }
}

/// Compute a coarse magnitude spectrum from the tail of the buffer and emit it.
fn emit_spectrum(app: &AppHandle, samples: &Arc<Mutex<Vec<f32>>>, fft: &dyn rustfft::Fft<f32>) {
    let window: Vec<f32> = {
        let buf = samples.lock().unwrap();
        if buf.len() < FFT_WINDOW {
            return;
        }
        buf[buf.len() - FFT_WINDOW..].to_vec()
    };

    // Hann window + RMS level.
    let mut rms = 0.0f32;
    let mut fbuf: Vec<Complex<f32>> = Vec::with_capacity(FFT_WINDOW);
    for (i, &s) in window.iter().enumerate() {
        rms += s * s;
        let w = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / (FFT_WINDOW as f32 - 1.0)).cos();
        fbuf.push(Complex { re: s * w, im: 0.0 });
    }
    rms = (rms / FFT_WINDOW as f32).sqrt();
    fft.process(&mut fbuf);

    // Fold the first half into SPECTRUM_BINS bars, log-scaled for a lively meter.
    let half = FFT_WINDOW / 2;
    let per = (half / SPECTRUM_BINS).max(1);
    let mut bins = Vec::with_capacity(SPECTRUM_BINS);
    for b in 0..SPECTRUM_BINS {
        let start = b * per;
        let end = (start + per).min(half);
        let mut mag = 0.0f32;
        for c in &fbuf[start..end] {
            mag += c.norm();
        }
        mag /= (end - start).max(1) as f32;
        // log compress + normalise to a usable 0..1 range for bars.
        let v = ((mag + 1.0).ln() / 6.0).clamp(0.0, 1.0);
        bins.push(v);
    }

    let _ = app.emit(
        super::EV_SPECTRUM,
        SpectrumPayload { bins, level: (rms * 6.0).clamp(0.0, 1.0) },
    );
}

#[derive(serde::Serialize, Clone)]
struct SpectrumPayload {
    bins: Vec<f32>,
    level: f32,
}

/// List input device names (for the mic-check UI).
pub fn list_input_devices() -> Vec<String> {
    let host = cpal::default_host();
    host.input_devices()
        .map(|it| it.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

/// Short blocking probe of the default/chosen device: returns the peak level over
/// ~400 ms so the UI can confirm the mic works.
pub fn test_microphone(device_name: Option<String>) -> super::MicTestResult {
    let samples = Arc::new(Mutex::new(Vec::<f32>::new()));
    let device_label = device_name.clone();
    match build_stream(device_name.as_deref(), samples.clone()) {
        Ok((stream, _rate)) => {
            if let Err(e) = stream.play() {
                return super::MicTestResult { ok: false, level: 0.0, device: device_label, error: Some(e.to_string()) };
            }
            std::thread::sleep(Duration::from_millis(400));
            drop(stream);
            let buf = samples.lock().unwrap();
            let peak = buf.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
            super::MicTestResult { ok: true, level: peak.min(1.0), device: device_label, error: None }
        }
        Err(e) => super::MicTestResult { ok: false, level: 0.0, device: device_label, error: Some(e) },
    }
}
