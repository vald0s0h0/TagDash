// Soft, discreet notification sounds synthesized on the fly with the Web Audio
// API — no asset files to ship, works offline, and the gentle envelopes keep
// every cue light and non-aggressive. Used by the alert-notification hook and
// previewable from Settings → Notifs.

export type NotifSoundId =
  | "soft_chime"
  | "marimba"
  | "droplet"
  | "pop"
  | "bell"
  | "ring_classic"
  | "ring_mobile"
  | "alarm_two_tone";

export interface NotifSound {
  id: NotifSoundId;
  label: string;
  /** Approximate length in seconds — shown in Settings so a 9-second ring is
   *  never picked by surprise. */
  secs: number;
  /** A long, repeating ring meant to be heard from the next room. Louder, and
   *  it never overlaps itself — see `playNotifSound`. */
  sustained?: boolean;
}

/** Short cues first, then the ringtones — the list reads as "discreet → insistent". */
export const NOTIF_SOUNDS: NotifSound[] = [
  { id: "soft_chime",    label: "Carillon doux",       secs: 0.7 },
  { id: "marimba",       label: "Marimba",             secs: 0.5 },
  { id: "droplet",       label: "Goutte d'eau",        secs: 0.3 },
  { id: "pop",           label: "Pop",                 secs: 0.2 },
  { id: "bell",          label: "Clochette",           secs: 0.7 },
  { id: "ring_classic",  label: "Sonnerie classique",  secs: 9, sustained: true },
  { id: "ring_mobile",   label: "Sonnerie mobile",     secs: 9, sustained: true },
  { id: "alarm_two_tone", label: "Alarme deux tons",   secs: 9, sustained: true },
];

const BY_ID = new Map<string, NotifSound>(NOTIF_SOUNDS.map((s) => [s.id, s]));

// One shared AudioContext. Created lazily, then kept for the app's lifetime.
let ctx: AudioContext | null = null;
function audioCtx(): AudioContext {
  if (!ctx) ctx = new (window.AudioContext || (window as any).webkitAudioContext)();
  if (ctx.state === "suspended") void ctx.resume();
  return ctx;
}

// An AudioContext only leaves "suspended" under user activation, and the alert path
// has no gesture of its own: it fires while the trader is WATCHING, not clicking.
// Waiting for the first alert to create the context is therefore a coin flip — the
// one session where it loses is an app launched and left untouched until the
// premarket, which is exactly the session an audible cue exists for. So the context
// is opened on the first interaction anywhere in the app (activation is sticky: it
// stays running afterwards, alert after alert).
//
// Unconditional, not gated on the sound being enabled: switching the cue on later
// must not depend on the user happening to click again afterwards.
function primeAudio() {
  window.removeEventListener("pointerdown", primeAudio, true);
  window.removeEventListener("keydown", primeAudio, true);
  try {
    audioCtx();
  } catch {
    /* Web Audio unavailable — playNotifSound stays a no-op */
  }
}
if (typeof window !== "undefined") {
  window.addEventListener("pointerdown", primeAudio, true);
  window.addEventListener("keydown", primeAudio, true);
}

/** One enveloped sine/triangle tone. `peak` is kept low so nothing is harsh.
 *  `dest` lets a ringtone route through its own bus so it can be cut short. */
function tone(
  ac: AudioContext,
  opts: {
    freq: number;
    start: number;       // seconds from now
    dur: number;
    peak?: number;
    type?: OscillatorType;
    freqEnd?: number;    // glide target for the droplet
    dest?: AudioNode;
  },
): OscillatorNode {
  const { freq, start, dur, peak = 0.12, type = "sine", freqEnd, dest } = opts;
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

  osc.connect(gain).connect(dest ?? ac.destination);
  osc.start(t0);
  osc.stop(t0 + dur + 0.02);
  return osc;
}

/** One ring burst: a single oscillator trilling between two pitches, the way an
 *  electromechanical bell alternates as the clapper swings. Cheaper and steadier
 *  than scheduling one oscillator per alternation. */
function ringBurst(
  ac: AudioContext,
  opts: {
    start: number;
    dur: number;
    low: number;
    high: number;
    /** Alternations per second (~20 on a real bell). */
    rate: number;
    peak: number;
    type?: OscillatorType;
    dest?: AudioNode;
  },
): OscillatorNode {
  const { start, dur, low, high, rate, peak, type = "triangle", dest } = opts;
  const t0 = ac.currentTime + start;
  const osc = ac.createOscillator();
  const gain = ac.createGain();
  osc.type = type;

  const step = 1 / rate;
  for (let i = 0, t = t0; t < t0 + dur; i++, t += step) {
    osc.frequency.setValueAtTime(i % 2 === 0 ? high : low, t);
  }

  // Held flat through the burst, then released — a bell rings, it does not fade
  // from the first instant.
  gain.gain.setValueAtTime(0.0001, t0);
  gain.gain.exponentialRampToValueAtTime(peak, t0 + 0.02);
  gain.gain.setValueAtTime(peak, t0 + Math.max(dur - 0.08, 0.03));
  gain.gain.exponentialRampToValueAtTime(0.0001, t0 + dur);

  osc.connect(gain).connect(dest ?? ac.destination);
  osc.start(t0);
  osc.stop(t0 + dur + 0.02);
  return osc;
}

// ── Sustained ringtones ────────────────────────────────────────────────────
// A 9-second ring is not a cue, it is a summons: it has to survive the walk back
// from another room. Two consequences the short cues never had.
//
// It must not STACK. Alerts arrive in bursts (the poll is ~800 ms and several
// tickers can fire in the same second), and three overlapping rings are noise, not
// urgency — so a ring already sounding simply keeps going, and the later alert
// rides it. Explicitly stopping and restarting is what the Settings preview does.
let ringBus: GainNode | null = null;
let ringNodes: OscillatorNode[] = [];
/** `ac.currentTime` at which the current ring finishes; 0 when nothing is ringing. */
let ringUntil = 0;

function stopRing(ac: AudioContext) {
  const now = ac.currentTime;
  if (ringBus) {
    try {
      ringBus.gain.cancelScheduledValues(now);
      ringBus.gain.setValueAtTime(ringBus.gain.value, now);
      // A 40 ms release rather than a hard cut: stopping an oscillator mid-cycle
      // clicks.
      ringBus.gain.linearRampToValueAtTime(0.0001, now + 0.04);
    } catch { /* node already torn down */ }
  }
  for (const osc of ringNodes) {
    try { osc.stop(now + 0.05); } catch { /* already stopped */ }
  }
  ringNodes = [];
  ringBus = null;
  ringUntil = 0;
}

/** Fresh output bus for one ring, so `stopRing` can silence exactly that ring. */
function newRingBus(ac: AudioContext): GainNode {
  const bus = ac.createGain();
  bus.gain.setValueAtTime(1, ac.currentTime);
  bus.connect(ac.destination);
  return bus;
}

/** Play the chosen notification sound once. No-op for an unknown id.
 *
 *  `restart` is for the Settings preview: it cuts a ringtone already playing so
 *  auditioning three of them in a row works. The alert path leaves it off, so a
 *  ring in progress is never interrupted or doubled by the next alert. */
export function playNotifSound(id: NotifSoundId | string, opts?: { restart?: boolean }) {
  let ac: AudioContext;
  try {
    ac = audioCtx();
  } catch {
    return; // Web Audio unavailable
  }

  const spec = BY_ID.get(id);
  let dest: AudioNode | undefined;
  if (spec?.sustained) {
    if (opts?.restart) {
      stopRing(ac);
    } else if (ac.currentTime < ringUntil) {
      return; // already ringing — let it finish rather than layering a second one
    }
    ringBus = newRingBus(ac);
    ringUntil = ac.currentTime + spec.secs;
    dest = ringBus;
  }
  /** Register a ringtone's oscillators so `stopRing` can cut them. */
  const ring = (osc: OscillatorNode) => { if (dest) ringNodes.push(osc); };

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

    // ── Ringtones: ~9 s, loud enough to carry, and unmistakably "come back". ──

    // Electromechanical bell: a 20 Hz trill in 1.4 s bursts with a breath between,
    // four times over — the cadence of an old desk phone.
    case "ring_classic":
      for (let i = 0; i < 4; i++) {
        ring(ringBurst(ac, {
          start: i * 2.3,
          dur: 1.4,
          low: 660,
          high: 880,
          rate: 20,
          peak: 0.22,
          dest,
        }));
      }
      break;

    // Mobile ringtone: a rising arpeggio (A5 · C#6 · E6 · C#6 · A5) repeated six
    // times. Melodic rather than shrill, so it reads as a phone from a distance
    // instead of as a fault alarm.
    case "ring_mobile":
      for (let i = 0; i < 6; i++) {
        const at = i * 1.6;
        [880, 1109, 1319, 1109, 880].forEach((freq, k) => {
          ring(tone(ac, {
            freq,
            start: at + k * 0.16,
            dur: 0.22,
            peak: 0.17,
            type: "triangle",
            dest,
          }));
        });
      }
      break;

    // Two-tone alarm: four high/low pairs, a pause, four times over. The most
    // insistent of the three — for when nothing quieter would get you back.
    case "alarm_two_tone":
      for (let i = 0; i < 4; i++) {
        const at = i * 2.36;
        for (let k = 0; k < 4; k++) {
          ring(tone(ac, {
            freq: 988,
            start: at + k * 0.44,
            dur: 0.2,
            peak: 0.2,
            type: "triangle",
            dest,
          }));
          ring(tone(ac, {
            freq: 740,
            start: at + k * 0.44 + 0.22,
            dur: 0.2,
            peak: 0.2,
            type: "triangle",
            dest,
          }));
        }
      }
      break;

    default:
      // An id in the catalog with no case arm here (an entry added without its
      // sound): nothing plays, so hand back the ring slot this call reserved
      // instead of muting the next nine seconds of real alerts.
      if (dest) stopRing(ac);
      break;
  }
}
