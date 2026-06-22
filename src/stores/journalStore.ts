import { create } from "zustand";
import { persist } from "zustand/middleware";

// The "journal du jour" draft. It is the working text the user keeps editing all
// day; the Send button pushes it to TradeTally without clearing the fields. The
// draft persists to localStorage so it survives the dashboard tab unmounting (it
// re-mounts on every visit) and app restarts. It auto-resets the next day at
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
  setTitle: (title: string) => void;
  setContent: (content: string) => void;
  /** Reset the draft if the journal day has rolled over since the last edit. */
  rollover: (today?: string) => void;
}

export const useJournalStore = create<JournalState>()(
  persist(
    (set, get) => ({
      day: journalDayKey(),
      title: "",
      content: "",
      setTitle: (title) => set({ title, day: journalDayKey() }),
      setContent: (content) => set({ content, day: journalDayKey() }),
      rollover: (today = journalDayKey()) => {
        if (get().day !== today) set({ day: today, title: "", content: "" });
      },
    }),
    { name: "tagdash-journal", version: 1 }
  )
);
