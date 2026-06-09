import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/tauri";

export function useMockAlerts() {
  return useQuery({
    queryKey: ["mock-alerts"],
    queryFn: api.getMockAlerts,
  });
}
