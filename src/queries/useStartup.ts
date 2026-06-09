import { useEffect, useRef } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api } from "@/lib/tauri";

const STARTUP_KEY = ["startup-status"];
const UNIVERSE_KEY = ["streamable-universe"];

export function useStartupStatus() {
  return useQuery({
    queryKey: STARTUP_KEY,
    queryFn: () => api.getStartupStatus(),
    refetchInterval: (query) => {
      // Poll every 500 ms while pipeline is running, stop when done
      const data = query.state.data;
      if (!data || !data.completed) return 500;
      return false;
    },
  });
}

export function useRunStartupPipeline() {
  const qc = useQueryClient();
  return useMutation({
    mutationFn: () => api.runStartupPipeline(),
    onSuccess: () => {
      // Start polling startup status. The streamable universe is intentionally
      // NOT invalidated here: the pipeline has only just *started* and hasn't
      // written floats / company metadata / bars yet. It is refetched on
      // completion instead (see useRefetchUniverseOnComplete).
      qc.invalidateQueries({ queryKey: STARTUP_KEY });
    },
  });
}

/// Refetch the streamable universe once the pipeline transitions to completed,
/// so the table reflects the freshly written floats, country/industry and
/// average volume (rather than the empty snapshot fetched while it was running).
export function useRefetchUniverseOnComplete(completed: boolean | undefined) {
  const qc = useQueryClient();
  const prev = useRef(false);
  useEffect(() => {
    if (completed && !prev.current) {
      qc.invalidateQueries({ queryKey: UNIVERSE_KEY });
    }
    prev.current = !!completed;
  }, [completed, qc]);
}

export function useStreamableUniverse() {
  return useQuery({
    queryKey: UNIVERSE_KEY,
    queryFn: () => api.getStreamableUniverse(),
    staleTime: 60_000,
  });
}
