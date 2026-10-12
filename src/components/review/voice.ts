// Review voice notes: the backend archives each note as 16 kHz mono mu-law with the
// silences cut out (see `review::voice`). Decoded here into a plain PCM WAV so an
// <audio> element can play it — with `preservesPitch`, a 2× replay speeds the voice
// up without the chipmunk effect.

import type { ReviewVoiceNote, VoiceChunk } from "@/types";

export const VOICE_RATE = 16_000;

function ulawDecode(u: number): number {
  const x = ~u & 0xff;
  const sign = x & 0x80;
  const exponent = (x >> 4) & 0x07;
  const mantissa = x & 0x0f;
  let sample = ((mantissa << 3) + 0x84) << exponent;
  sample -= 0x84;
  return sign ? -sample : sample;
}

const TABLE = (() => {
  const t = new Int16Array(256);
  for (let i = 0; i < 256; i++) t[i] = ulawDecode(i);
  return t;
})();

/** mu-law bytes → a 16-bit PCM WAV blob URL. */
export function ulawToWavUrl(bytes: ArrayBuffer): string {
  const u8 = new Uint8Array(bytes);
  const n = u8.length;
  const buf = new ArrayBuffer(44 + n * 2);
  const v = new DataView(buf);
  const str = (o: number, s: string) => {
    for (let i = 0; i < s.length; i++) v.setUint8(o + i, s.charCodeAt(i));
  };
  str(0, "RIFF");
  v.setUint32(4, 36 + n * 2, true);
  str(8, "WAVE");
  str(12, "fmt ");
  v.setUint32(16, 16, true);
  v.setUint16(20, 1, true); // PCM
  v.setUint16(22, 1, true); // mono
  v.setUint32(24, VOICE_RATE, true);
  v.setUint32(28, VOICE_RATE * 2, true);
  v.setUint16(32, 2, true);
  v.setUint16(34, 16, true);
  str(36, "data");
  v.setUint32(40, n * 2, true);
  for (let i = 0; i < n; i++) v.setInt16(44 + i * 2, TABLE[u8[i]], true);
  return URL.createObjectURL(new Blob([buf], { type: "audio/wav" }));
}

/** Market-clock span of one chunk. */
export function chunkEndMs(c: VoiceChunk, speed: number): number {
  return c.start_ms + (c.samples / VOICE_RATE) * 1000 * speed;
}

export interface ActiveChunk {
  voice: ReviewVoiceNote;
  chunk: VoiceChunk;
  /** Position inside the archived audio (seconds) matching the playhead. */
  audioSec: number;
}

/** The voiced chunk under the playhead, if any. */
export function chunkAt(voices: ReviewVoiceNote[], ms: number): ActiveChunk | null {
  for (const v of voices) {
    for (const c of v.chunks) {
      if (ms >= c.start_ms && ms < chunkEndMs(c, v.speed)) {
        return {
          voice: v,
          chunk: c,
          audioSec: c.offset / VOICE_RATE + (ms - c.start_ms) / 1000 / Math.max(0.1, v.speed),
        };
      }
    }
  }
  return null;
}

/** Start of the next voiced chunk strictly after `ms`. */
export function nextChunkStart(voices: ReviewVoiceNote[], ms: number): number | null {
  let best: number | null = null;
  for (const v of voices) {
    for (const c of v.chunks) {
      if (c.start_ms > ms && (best == null || c.start_ms < best)) best = c.start_ms;
    }
  }
  return best;
}
