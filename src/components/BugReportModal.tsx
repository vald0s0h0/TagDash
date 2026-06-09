import { useState } from "react";
import { Copy, Trash2 } from "lucide-react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";
import type { BugPriority, BugReport } from "@/types";

interface Props {
  open: boolean;
  onClose: () => void;
}

// ─── Priority metadata ─────────────────────────────────────────────────────────

const PRIORITIES: { value: BugPriority; label: string; badge: string; dot: string }[] = [
  { value: 3, label: "Haute",   badge: "bg-red-900/70 text-red-300",     dot: "bg-red-400" },
  { value: 2, label: "Moyenne", badge: "bg-amber-900/60 text-amber-300", dot: "bg-amber-400" },
  { value: 1, label: "Basse",   badge: "bg-zinc-700 text-zinc-300",      dot: "bg-zinc-400" },
];

function priorityMeta(p: number) {
  return PRIORITIES.find((x) => x.value === p) ?? PRIORITIES[1];
}

export function BugReportModal({ open, onClose }: Props) {
  const [text, setText] = useState("");
  const [priority, setPriority] = useState<BugPriority>(2);

  const queryClient = useQueryClient();
  const { data: reports = [] } = useQuery({
    queryKey: ["bug_reports"],
    queryFn:  () => api.getBugReports(),
    enabled:  open,
  });

  function setReports(next: BugReport[]) {
    queryClient.setQueryData(["bug_reports"], next);
  }

  async function submit() {
    const trimmed = text.trim();
    if (!trimmed) return;
    const next = await api.addBugReport(crypto.randomUUID(), trimmed, priority);
    setReports(next);
    setText("");
  }

  async function remove(id: string) {
    setReports(await api.deleteBugReport(id));
  }

  async function clearAll() {
    setReports(await api.clearBugReports());
  }

  function copyAll() {
    const blob = reports
      .map((r) => `[${priorityMeta(r.priority).label}] [${r.created_at}]\n${r.text}`)
      .join("\n\n---\n\n");
    navigator.clipboard.writeText(blob).catch(() => {});
  }

  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-w-xl">
        <DialogHeader>
          <DialogTitle>Signaler un bug</DialogTitle>
        </DialogHeader>

        {/* Input area */}
        <textarea
          className="h-24 w-full resize-none rounded-md border border-border bg-background px-3 py-2 text-sm text-foreground placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-ring"
          placeholder="Décris le bug observé…"
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) submit();
          }}
        />

        <div className="flex items-center justify-between">
          {/* Priority selector */}
          <div className="flex items-center gap-1">
            <span className="mr-1 text-xs text-muted-foreground">Priorité :</span>
            {PRIORITIES.map((p) => (
              <button
                key={p.value}
                onClick={() => setPriority(p.value)}
                className={cn(
                  "rounded px-2 py-1 text-[11px] font-medium transition-colors",
                  priority === p.value
                    ? p.badge
                    : "text-muted-foreground hover:bg-accent hover:text-foreground"
                )}
              >
                {p.label}
              </button>
            ))}
          </div>
          <Button size="sm" onClick={submit} disabled={!text.trim()}>
            Envoyer
          </Button>
        </div>

        {/* Reports list */}
        {reports.length > 0 && (
          <>
            <div className="flex items-center justify-between border-t border-border pt-3">
              <span className="text-xs text-muted-foreground">
                {reports.length} rapport{reports.length > 1 ? "s" : ""}
              </span>
              <div className="flex gap-2">
                <Button variant="ghost" size="sm" onClick={copyAll}>
                  <Copy className="mr-1.5 h-3.5 w-3.5" />
                  Copier tout
                </Button>
                <Button variant="ghost" size="sm" onClick={clearAll}>
                  <Trash2 className="mr-1.5 h-3.5 w-3.5" />
                  Tout effacer
                </Button>
              </div>
            </div>

            <div className="max-h-52 overflow-y-auto rounded-md border border-border">
              <table className="w-full text-xs">
                <thead className="sticky top-0 bg-card">
                  <tr className="border-b border-border text-left">
                    <th className="w-20 px-2 py-1.5 font-medium text-muted-foreground">
                      Priorité
                    </th>
                    <th className="px-2 py-1.5 font-medium text-muted-foreground">
                      Description
                    </th>
                    <th className="w-8 px-2 py-1.5" />
                  </tr>
                </thead>
                <tbody>
                  {reports.map((r) => {
                    const meta = priorityMeta(r.priority);
                    return (
                      <tr key={r.id} className="border-b border-border last:border-0">
                        <td className="px-2 py-1.5">
                          <span className={cn("rounded px-1.5 py-0.5 text-[10px] font-semibold", meta.badge)}>
                            {meta.label}
                          </span>
                        </td>
                        <td className="break-words px-2 py-1.5 text-foreground">
                          {r.text}
                        </td>
                        <td className="px-2 py-1.5 text-right">
                          <button
                            onClick={() => remove(r.id)}
                            title="Supprimer"
                            className="text-muted-foreground/60 hover:text-red-400"
                          >
                            <Trash2 className="h-3.5 w-3.5" />
                          </button>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}
