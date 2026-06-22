import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "@/lib/tauri";

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

/** Send (create-or-update) today's TradeTally diary entry. */
export function useSaveDiary() {
  return useMutation({
    mutationFn: ({ title, content }: { title: string; content: string }) =>
      api.saveDiaryEntry(title, content),
  });
}
