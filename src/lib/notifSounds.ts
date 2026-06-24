// Soft, discreet notification sounds synthesized on the fly with the Web Audio
// API — no asset files to ship, works offline, and the gentle envelopes keep
// every cue light and non-aggressive. Used by the alert-notification hook and
// previewable from Settings → Notifs.

export type NotifSoundId =
  | "soft_chime"
  | "marimba"
  | "droplet"
  | "pop"
  | "bell";

export const NOTIF_SOUNDS: { id: NotifSoundId; label: string }[] = [
  { id: "soft_chime", label: "Carillon doux" },
  { id: "marimba",    label: "Marimba" },
  { id: "droplet",    label: "Goutte d'eau" },
  { id: "pop",        label: "Pop" },
  { id: "bell",       label: "Clochette" },
];

// One shared AudioContext, created lazily on the first (user-gesture-driven) play
// so the browser autoplay policy never blocks it.
let ctx: AudioContext | null = null;
function audioCtx(): AudioContext {
  if (!ctx) ctx = new (window.AudioContext || (window as any).webkitAudioContext)();
  if (ctx.state === "suspended") void ctx.resume();
  return ctx;
}

/** One enveloped sine/triangle tone. `peak` is kept low so nothing is harsh. */
function tone(
  ac: AudioContext,
  opts: {
    freq: number;
    start: number;       // seconds from now
    dur: number;
    peak?: number;
    type?: OscillatorType;
    freqEnd?: number;    // glide target for the droplet
  },
) {
  const { freq, start, dur, peak = 0.12, type = "sine", freqEnd } = opts;
  const t0 = ac.currentTime + start;
  const osc = ac.createOscillator();
  const gain = ac.createGain();
  osc.type = type;
  osc.frequency.setValueAtTime(freq, t0);
  if (freqEnd != null) osc.frequency.exponentialRampToValueAtTime(freqEnd, t0 + dur);

  // Quick soft attack, smooth exponential decay — no clicks, no bite.
  gain.gain.setValueAtTime(0.0001, t0);
  gain.gain.exponentialRampToValueAtTime(peak, t0 + 0.012);
  gain.gain.exponentialRampToValueAtTime(0.0001, t0 + dur);

  osc.connect(gain).connect(ac.destination);
  osc.start(t0);
  osc.stop(t0 + dur + 0.02);
}

/** Play the chosen notification sound once. No-op for an unknown id. */
export function playNotifSound(id: NotifSoundId | string) {
  let ac: AudioContext;
  try {
    ac = audioCtx();
  } catch {
    return; // Web Audio unavailable
  }

  switch (id) {
    case "soft_chime":
      tone(ac, { freq: 880,  start: 0,    dur: 0.45, peak: 0.10 });
      tone(ac, { freq: 1318, start: 0.10, dur: 0.55, peak: 0.08 });
      break;
    case "marimba":
      tone(ac, { freq: 587,  start: 0,    dur: 0.30, peak: 0.13, type: "triangle" });
      tone(ac, { freq: 880,  start: 0.07, dur: 0.35, peak: 0.10, type: "triangle" });
      break;
    case "droplet":
      tone(ac, { freq: 1200, start: 0, dur: 0.28, peak: 0.11, freqEnd: 620 });
      break;
    case "pop":
      tone(ac, { freq: 440, start: 0, dur: 0.14, peak: 0.12, type: "triangle" });
      break;
    case "bell":
      tone(ac, { freq: 988,  start: 0, dur: 0.7,  peak: 0.09 });
      tone(ac, { freq: 1976, start: 0, dur: 0.5,  peak: 0.04 }); // soft octave overtone
      break;
    default:
      break;
  }
}
