import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/tauri";

export function useSecretsStatus() {
  return useQuery({
    queryKey: ["secrets-status"],
    queryFn: api.getSecretsStatus,
    refetchInterval: 15_000,
  });
}
