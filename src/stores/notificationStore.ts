import { create } from "zustand";

/** Transient, non-blocking notices — the order path's way of saying "that didn't
 *  happen" without stealing focus.
 *
 *  Deliberately NOT a dialog. These fire while the user is watching price, often
 *  mid-setup; a modal would cover the chart and demand a click at the worst moment.
 *  They stack in a corner, fade on their own, and can be dismissed with a click.
 *
 *  RAM only: a notice that outlived the session it described would be noise. */
export type NoticeLevel = "error" | "warn" | "info";

export interface Notice {
  id: number;
  level: NoticeLevel;
  message: string;
}

/** How long each level stays up. Errors linger: they usually mean an order did not
 *  reach the venue, and that is worth reading twice. */
const TTL_MS: Record<NoticeLevel, number> = {
  error: 8000,
  warn: 6000,
  info: 4000,
};

interface NotificationState {
  notices: Notice[];
  notify: (message: string, level?: NoticeLevel) => void;
  dismiss: (id: number) => void;
}

let nextId = 1;

export const useNotificationStore = create<NotificationState>((set, get) => ({
  notices: [],

  notify: (message, level = "error") => {
    const text = String(message ?? "").trim();
    if (!text) return;
    // Collapse a repeat of the notice already on top. Hammering a button that keeps
    // failing should not build a wall of identical toasts.
    const top = get().notices[get().notices.length - 1];
    if (top && top.message === text && top.level === level) return;

    const id = nextId++;
    set((s) => ({ notices: [...s.notices, { id, level, message: text }].slice(-4) }));
    setTimeout(() => get().dismiss(id), TTL_MS[level]);
  },

  dismiss: (id) => set((s) => ({ notices: s.notices.filter((n) => n.id !== id) })),
}));

/** Turn whatever a rejected Tauri command threw into something readable. Rust
 *  commands reject with a plain string; anything else is a bug on our side and is
 *  shown as-is rather than swallowed. */
export function notifyError(e: unknown, prefix?: string) {
  const detail = typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
  useNotificationStore.getState().notify(prefix ? `${prefix} — ${detail}` : detail, "error");
}
