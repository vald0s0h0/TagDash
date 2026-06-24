import { useEffect, useRef } from "react";
import {
  isPermissionGranted,
  requestPermission,
  sendNotification,
} from "@tauri-apps/plugin-notification";
import { useActiveAlerts } from "@/queries/useScanner";
import { useLocalConfig } from "@/queries/useLocalConfig";
import { playNotifSound } from "@/lib/notifSounds";
import type { AlertSignal, AttentionMode, Session } from "@/types";

/** True when a sound/attention mode is active for the alert's session. Mirrors
 *  the Rust `mode_matches` gate (notify.rs): pre-open / afterhours never fire. */
function modeMatches(mode: AttentionMode, session: Session): boolean {
  switch (mode) {
    case "premarket": return session === "premarket";
    case "open":      return session === "open";
    case "both":      return session === "premarket" || session === "open";
    default:          return false; // "off"
  }
}

/** One-line body for a notification: reason + price when known. */
function notifBody(a: AlertSignal): string {
  const price = a.price != null ? `$${a.price.toFixed(2)}` : null;
  return [price, a.reason].filter(Boolean).join(" · ") || a.strategy_name;
}

/** Mount once, app-level: fire a native OS notification (Windows toast / macOS
 *  Notification Center) whenever a NEW scanner alert appears — any strategy, any
 *  session, regardless of the active tab. Driven by the active-alerts poll (newest
 *  first, one entry per symbol+strategy, ~800 ms) so it never depends on what's on
 *  screen and fires with low latency; each genuine trigger carries a fresh
 *  `alert_id`, which is how "new" is detected.
 *
 *  Gated by `ui.desktop_alerts` (Settings → Notifications). The first poll after
 *  mount only seeds the baseline (so the existing backlog never replays); the
 *  baseline is also kept current while the toggle is OFF, so enabling it later
 *  starts from "now", not from the backlog. */
export function useAlertNotifications() {
  const { data: config } = useLocalConfig();
  const { data: alerts } = useActiveAlerts();
  const enabled = config?.ui?.desktop_alerts ?? false;
  const soundMode = (config?.ui?.alert_sound_mode ?? "off") as AttentionMode;
  const soundId   = config?.ui?.alert_sound ?? "soft_chime";

  // Alert ids seen on the previous poll — anything new this poll is a fresh alert.
  const prevIds     = useRef<Set<string>>(new Set());
  const initialized = useRef(false);
  const granted     = useRef(false);

  // Request OS permission when the feature is switched on (no-op once granted).
  useEffect(() => {
    if (!enabled) { granted.current = false; return; }
    let cancelled = false;
    (async () => {
      try {
        let ok = await isPermissionGranted();
        if (!ok) ok = (await requestPermission()) === "granted";
        if (!cancelled) granted.current = ok;
      } catch {
        if (!cancelled) granted.current = false;
      }
    })();
    return () => { cancelled = true; };
  }, [enabled]);

  useEffect(() => {
    if (!alerts) return;
    const currentIds = alerts.map((a) => a.alert_id);

    // First batch after mount: baseline only, never notify for the backlog.
    if (!initialized.current) {
      prevIds.current = new Set(currentIds);
      initialized.current = true;
      return;
    }

    const fresh = alerts.filter((a) => !prevIds.current.has(a.alert_id));
    prevIds.current = new Set(currentIds);

    if (fresh.length === 0) return;

    // Sound cue — independent of the desktop toast, gated by its own session mode.
    if (soundMode !== "off" && fresh.some((a) => modeMatches(soundMode, a.session))) {
      playNotifSound(soundId);
    }

    if (!enabled || !granted.current) return;

    // `alerts` is newest-first; notify oldest-first so the most recent ends up on top.
    for (const a of [...fresh].reverse()) {
      try {
        sendNotification({
          title: `🔔 ${a.symbol} — ${a.strategy_name}`,
          body:  notifBody(a),
        });
      } catch { /* notification backend unavailable — ignore */ }
    }
  }, [alerts, enabled, soundMode, soundId]);
}
