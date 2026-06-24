import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "@/lib/tauri";
import type { SecretsUpdate } from "@/types";

export function useSecretsStatus() {
  return useQuery({
    queryKey: ["secrets-status"],
    queryFn: api.getSecretsStatus,
    refetchInterval: 15_000,
  });
}

export function useUpdateSecrets() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: (updates: SecretsUpdate) => api.updateSecrets(updates),
    onSuccess: (status) => {
      // The command returns the fresh status — seed the cache immediately.
      qc.setQueryData(["secrets-status"], status);
    },
  });
}
