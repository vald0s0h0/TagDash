// Single source of truth for displaying timestamps in New York time across the
// whole UI (charts, journal/screenshots, panels). Underlying data stays in real
// UTC; only the *display* is converted, via Intl with timeZone America/New_York
// — which handles EST/EDT (DST) correctly, unlike a fixed −4h offset.

export const NY_TZ = "America/New_York";

type TimeInput = string | number | Date;

/** A `number` input is treated as a Unix **seconds** timestamp (chart time);
 *  strings/Dates are parsed as-is. */
function toDate(input: TimeInput): Date {
  if (input instanceof Date) return input;
  if (typeof input === "number") return new Date(input * 1000);
  return new Date(input);
}

/** NY wall-clock parts (zero-padded, 24h) for an instant. */
function nyParts(d: Date): Record<string, string> {
  const fmt = new Intl.DateTimeFormat("en-US", {
    timeZone:   NY_TZ,
    hourCycle:  "h23", // 00–23, avoids the "24:00" en-US midnight quirk
    year:   "numeric", month: "2-digit", day:    "2-digit",
    hour:   "2-digit", minute: "2-digit", second: "2-digit",
  });
  const out: Record<string, string> = {};
  for (const p of fmt.formatToParts(d)) out[p.type] = p.value;
  return out;
}

/** "HH:mm" (or "HH:mm:ss") in NY time. */
export function nyTime(input: TimeInput, withSeconds = false): string {
  const p = nyParts(toDate(input));
  return withSeconds ? `${p.hour}:${p.minute}:${p.second}` : `${p.hour}:${p.minute}`;
}

/** "Mon DD · HH:mm" in NY time (crosshair / compact datetime). */
export function nyDateTime(input: TimeInput, withSeconds = false): string {
  const d  = toDate(input);
  const md = new Intl.DateTimeFormat("en-US", { timeZone: NY_TZ, month: "short", day: "2-digit" }).format(d);
  return `${md} · ${nyTime(d, withSeconds)}`;
}

/** Short NY month, e.g. "Jun". */
export function nyMonth(input: TimeInput): string {
  return new Intl.DateTimeFormat("en-US", { timeZone: NY_TZ, month: "short" }).format(toDate(input));
}

/** NY year, e.g. "2026". */
export function nyYear(input: TimeInput): string {
  return new Intl.DateTimeFormat("en-US", { timeZone: NY_TZ, year: "numeric" }).format(toDate(input));
}

/** NY day of month + month, e.g. "04 Jun". */
export function nyDayMonth(input: TimeInput): string {
  const d = toDate(input);
  return `${nyParts(d).day} ${nyMonth(d)}`;
}

/** Filename-safe NY stamp "YYYY-MM-DD-HH-mm-ss" (screenshots). */
export function nyFilenameStamp(input: TimeInput = new Date()): string {
  const p = nyParts(toDate(input));
  return `${p.year}-${p.month}-${p.day}-${p.hour}-${p.minute}-${p.second}`;
}

/** Minutes since NY midnight, for session (pre/regular/post-market) tests. */
export function nyMinutesOfDay(input: TimeInput): number {
  const p = nyParts(toDate(input));
  return Number(p.hour) * 60 + Number(p.minute);
}

// Regular cash session in NY: 09:30 (570) → 16:00 (960). Anything outside is
// pre-market / post-market (extended hours).
export const NY_REGULAR_OPEN_MIN  = 9 * 60 + 30;
export const NY_REGULAR_CLOSE_MIN = 16 * 60;

/** True when the instant falls outside the 09:30–16:00 NY cash session. */
export function isExtendedHours(input: TimeInput): boolean {
  const m = nyMinutesOfDay(input);
  return m < NY_REGULAR_OPEN_MIN || m >= NY_REGULAR_CLOSE_MIN;
}
