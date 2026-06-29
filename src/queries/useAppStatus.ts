import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/tauri";

export function useAppStatus() {
  return useQuery({
    queryKey:        ["app-status"],
    queryFn:         api.getAppStatus,
    refetchInterval: 2_000,
  });
}
