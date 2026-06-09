import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/tauri";

export function useAppStatus() {
  return useQuery({
    queryKey:        ["app-status"],
    queryFn:         api.getAppStatus,
    refetchInterval: 500, // real latency from market state, needs fast refresh
  });
}
