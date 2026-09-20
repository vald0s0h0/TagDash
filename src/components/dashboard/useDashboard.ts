import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "@/lib/tauri";
import { journalDayKey, useJournalStore } from "@/stores/journalStore";

/** Cached trades that feed the KPI cards. */
export function useDashboardTrades() {
  return useQuery({
    queryKey: ["dashboard_trades"],
    queryFn: () => api.getDashboardTrades(),
  });
}

/** Today's background photo (deterministic per day) + the folder path. */
export function useDailyBackground() {
  return useQuery({
    queryKey: ["daily_background"],
    queryFn: () => api.getDailyBackground(),
    staleTime: 1000 * 60 * 60, // a full hour — the image is stable for the day
  });
}

/** A fresh random mood pick (image + short/long phrase). Unlike the daily
 *  background it re-randomises on every mount — the dashboard tab unmounts when
 *  you leave it, so this yields a new pick on each open and on page refresh. The
 *  ⟳ action in the dropdown invalidates ["mood"] for a manual reshuffle. */
export function useMood() {
  return useQuery({
    queryKey: ["mood"],
    queryFn: () => api.getMood(),
    staleTime: 0,
    refetchOnMount: "always",
    refetchOnWindowFocus: false,
  });
}

/** Re-sync trades from TradeTally (source of truth) and refresh the cards. */
export function useSyncTrades() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: () => api.syncTradetallyTrades(),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["dashboard_trades"] }),
  });
}

/** TradeTally's own journal entry for today (ET). The Journal card hydrates from
 *  this, so the entry shows up whether it was written here, on another machine, or
 *  straight in TradeTally. Refetched on mount — the dashboard tab unmounts when you
 *  leave it, so every open re-reads the source of truth.
 *
 *  `retry: false`: a missing entry comes back as `null` (not an error), so the only
 *  errors here are server/auth ones, and the card just keeps showing the draft. */
export function useDiaryEntry() {
  return useQuery({
    queryKey: ["diary_entry", journalDayKey()],
    queryFn: () => api.getDiaryEntry(),
    refetchOnMount: "always",
    refetchOnWindowFocus: false,
    retry: false,
  });
}

/** Send (create-or-update) today's TradeTally diary entry. The send is queued and
 *  retried in the backend, so a success here means "handed to the queue". */
export function useSaveDiary() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: ({ title, content }: { title: string; content: string }) =>
      api.saveDiaryEntry(title, content),
    onSuccess: () => {
      useJournalStore.getState().markSent();
      // Don't re-read immediately: the worker drains the queue on its own interval,
      // so the server would still answer with the pre-send text. The next open picks
      // the new version up.
      qc.invalidateQueries({ queryKey: ["diary_entry"], refetchType: "none" });
    },
  });
}
