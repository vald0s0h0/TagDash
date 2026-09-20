import { create } from "zustand";
import { persist } from "zustand/middleware";

// The "journal du jour". TradeTally holds the entry (one per calendar day,
// updated in place); this store is the editing buffer in front of it.
//
// Two things have to be true at once: what TradeTally holds must show up on the
// card (otherwise the entry you wrote yesterday, or from another machine, looks
// lost), and text you are in the middle of typing must never be overwritten by a
// fetch landing a moment later. `dirty` is what separates the two — it is set by
// every keystroke and cleared by a successful send, and `hydrate` only writes into
// the buffer while it is false.
//
// The buffer persists to localStorage so it survives the dashboard tab unmounting
// (it re-mounts on every visit) and app restarts, and auto-resets the next day at
// midnight America/New_York.

/** Calendar day key in America/New_York — flips at ET midnight. */
export function journalDayKey(now: Date = new Date()): string {
  // en-CA gives an ISO-ish YYYY-MM-DD; the timeZone option does the ET conversion.
  return now.toLocaleDateString("en-CA", { timeZone: "America/New_York" });
}

interface JournalState {
  day: string;
  title: string;
  content: string;
  /** Edited since the last successful send / hydrate — see the note above. */
  dirty: boolean;
  setTitle: (title: string) => void;
  setContent: (content: string) => void;
  /** Adopt TradeTally's entry for `day`. Ignored while the buffer is dirty, so it
   *  can never clobber text being typed. */
  hydrate: (day: string, title: string, content: string) => void;
  /** Mark the buffer as matching what TradeTally now holds (after a send). */
  markSent: () => void;
  /** Reset the draft if the journal day has rolled over since the last edit. */
  rollover: (today?: string) => void;
}

export const useJournalStore = create<JournalState>()(
  persist(
    (set, get) => ({
      day: journalDayKey(),
      title: "",
      content: "",
      dirty: false,
      setTitle: (title) => set({ title, day: journalDayKey(), dirty: true }),
      setContent: (content) => set({ content, day: journalDayKey(), dirty: true }),
      hydrate: (day, title, content) => {
        const s = get();
        if (s.dirty && s.day === day) return; // unsent local edits win
        set({ day, title, content, dirty: false });
      },
      markSent: () => set({ dirty: false }),
      rollover: (today = journalDayKey()) => {
        if (get().day !== today) set({ day: today, title: "", content: "", dirty: false });
      },
    }),
    {
      name: "tagdash-journal",
      version: 2,
      // v1 had no `dirty` flag. Without a migration zustand discards the stored
      // state on a version bump, which would drop an unsent draft; carry the text
      // forward and treat it as dirty so the first hydrate can't overwrite it.
      migrate: (persisted, version) => {
        const s = (persisted ?? {}) as Partial<JournalState>;
        if (version < 2) {
          return {
            day: s.day ?? journalDayKey(),
            title: s.title ?? "",
            content: s.content ?? "",
            dirty: Boolean((s.title ?? "") || (s.content ?? "")),
          } as JournalState;
        }
        return persisted as JournalState;
      },
    }
  )
);
