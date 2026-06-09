import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "@/lib/tauri";
import type { AppConfig } from "@/types";

export function useLocalConfig() {
  return useQuery({ queryKey: ["local-config"], queryFn: api.getLocalConfig });
}

export function useUpdateLocalConfig() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (config: AppConfig) => api.updateLocalConfig(config),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["local-config"] }),
  });
}
