import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/tauri";

export function useSyncStatus() {
  return useQuery({
    queryKey: ["sync-status"],
    queryFn: api.getSyncQueueStatus,
    refetchInterval: 10_000,
  });
}
