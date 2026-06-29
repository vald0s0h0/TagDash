import { useEffect, useMemo, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ArrowDown,
  ArrowUp,
  Camera,
  Check,
  ChevronsUpDown,
  Database,
  NotebookPen,
  RefreshCw,
  Search,
  Trash2,
  X,
} from "lucide-react";
import { Dialog, DialogContent } from "@/components/ui/dialog";
import { api } from "@/lib/tauri";
import type { TradeDbRow } from "@/types";
import { cn } from "@/lib/utils";

interface Props {
  open: boolean;
  onClose: () => void;
}

type Kind = "text" | "pnl" | "int" | "bool" | "date";

interface Col {
  key: keyof TradeDbRow;
  header: string;
  w: number;
  kind: Kind;
}

const COLS: Col[] = [
  { key: "symbol",               header: "Ticker",    w: 80,  kind: "text" },
  { key: "side",                 header: "Side",      w: 66,  kind: "text" },
  { key: "open",                 header: "Statut",    w: 70,  kind: "bool" },
  { key: "pnl",                  header: "P&L",       w: 90,  kind: "pnl" },
  { key: "fills",                header: "Fills",     w: 54,  kind: "int" },
  { key: "first_fill_at",        header: "Ouverture", w: 140, kind: "date" },
  { key: "last_fill_at",         header: "Fermeture", w: 140, kind: "date" },
  { key: "has_note",             header: "Note",      w: 52,  kind: "bool" },
  { key: "has_screenshot",       header: "Screen",    w: 58,  kind: "bool" },
  { key: "sent_to_tradetally",   header: "Envoyé TT", w: 80, kind: "bool" },
  { key: "synced_on_tradetally", header: "Sync TT",   w: 72,  kind: "bool" },
];

function formatCell(kind: Kind, value: unknown): React.ReactNode {
  if (kind === "bool") {
    return value ? (
      <Check className="mx-auto h-3 w-3 text-emerald-400" />
    ) : (
      <span className="text-muted-foreground/30">·</span>
    );
  }
  if (value === null || value === undefined || value === "") {
    return <span className="text-muted-foreground/30">-</span>;
  }
  switch (kind) {
    case "text": {
      const s = String(value);
      if (s === "long") return <span className="text-emerald-400">Long</span>;
      if (s === "short") return <span className="text-red-400">Short</span>;
      if (s === "closed") return <span className="text-muted-foreground">Closed</span>;
      return <span className="font-semibold">{s}</span>;
    }
    case "pnl": {
      const n = value as number;
      return (
        <span className={n > 0 ? "text-emerald-400" : n < 0 ? "text-red-400" : undefined}>
          {n > 0 ? "+" : ""}${n.toFixed(2)}
        </span>
      );
    }
    case "int":
      return String(value);
    case "date":
      return String(value).replace("T", " ").slice(0, 16);
    default:
      return String(value);
  }
}

type SortState = { key: keyof TradeDbRow; dir: "asc" | "desc" } | null;

export function TradesDBModal({ open, onClose }: Props) {
  const queryClient = useQueryClient();
  const [filter, setFilter] = useState("");
  const [sort, setSort] = useState<SortState>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);

  useEffect(() => {
    if (open) {
      setFilter("");
      setSort(null);
      setSelected(new Set());
      setConfirmDelete(null);
    }
  }, [open]);

  const { data = [], isLoading, isFetching, refetch } = useQuery({
    queryKey: ["all_trades_db"],
    queryFn: () => api.getAllTradesDb(),
    enabled: open,
    staleTime: 10_000,
  });

  const rows = useMemo(() => {
    let out = data;
    const q = filter.trim().toLowerCase();
    if (q) {
      out = out.filter(
        (row) =>
          row.symbol.toLowerCase().includes(q) ||
          row.trade_id.toLowerCase().includes(q) ||
          row.side.toLowerCase().includes(q),
      );
    }
    if (sort) {
      const { key, dir } = sort;
      out = [...out].sort((a, b) => {
        const av = a[key];
        const bv = b[key];
        if (av === null || av === undefined) return 1;
        if (bv === null || bv === undefined) return -1;
        let cmp: number;
        if (typeof av === "number" && typeof bv === "number") cmp = av - bv;
        else if (typeof av === "boolean" && typeof bv === "boolean")
          cmp = av === bv ? 0 : av ? 1 : -1;
        else cmp = String(av).localeCompare(String(bv));
        return dir === "asc" ? cmp : -cmp;
      });
    }
    return out;
  }, [data, filter, sort]);

  const toggleSort = (key: keyof TradeDbRow) =>
    setSort((s) =>
      s?.key === key ? (s.dir === "asc" ? { key, dir: "desc" } : null) : { key, dir: "asc" },
    );

  const toggleSelect = (id: string) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const toggleSelectAll = () => {
    if (selected.size === rows.length) {
      setSelected(new Set());
    } else {
      setSelected(new Set(rows.map((r) => r.trade_id)));
    }
  };

  async function handleDelete(tradeId: string) {
    try {
      await api.deleteTradeDb(tradeId);
      setSelected((prev) => {
        const next = new Set(prev);
        next.delete(tradeId);
        return next;
      });
      setConfirmDelete(null);
      refetch();
      queryClient.invalidateQueries({ queryKey: ["todo_trades"] });
    } catch {
      // ignore
    }
  }

  async function handleDeleteSelected() {
    for (const id of selected) {
      try {
        await api.deleteTradeDb(id);
      } catch {
        // continue
      }
    }
    setSelected(new Set());
    setConfirmDelete(null);
    refetch();
    queryClient.invalidateQueries({ queryKey: ["todo_trades"] });
  }

  const totalPnl = rows.reduce((sum, r) => sum + r.pnl, 0);

  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="flex h-[85vh] w-[80vw] max-w-none flex-col gap-0 p-0">
        {/* Header */}
        <div className="flex shrink-0 flex-wrap items-center gap-3 border-b border-border px-4 py-2.5 pr-12">
          <div className="flex items-center gap-2 text-sm font-semibold">
            <Database className="h-4 w-4" />
            Trades (DB locale)
          </div>

          <div className="flex items-center gap-2 rounded-md border border-border bg-background px-2 py-1">
            <Search className="h-3.5 w-3.5 text-muted-foreground" />
            <input
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
              placeholder="Filtrer par ticker, trade_id, side..."
              className="w-56 bg-transparent text-xs outline-none placeholder:text-muted-foreground"
            />
          </div>

          <button
            onClick={() => refetch()}
            title="Rafraichir"
            className="flex items-center gap-1 rounded px-2 py-1 text-[11px] text-muted-foreground transition-colors hover:bg-accent/50 hover:text-foreground"
          >
            <RefreshCw className={cn("h-3.5 w-3.5", isFetching && "animate-spin")} /> Rafraichir
          </button>

          {selected.size > 0 && (
            <button
              onClick={() => setConfirmDelete("__bulk__")}
              className="flex items-center gap-1 rounded bg-red-900/40 px-2 py-1 text-[11px] text-red-400 transition-colors hover:bg-red-900/60"
            >
              <Trash2 className="h-3.5 w-3.5" />
              Supprimer {selected.size} trade(s)
            </button>
          )}

          <span className="ml-auto text-[11px] tabular-nums text-muted-foreground">
            {rows.length} trade(s) &middot; P&L total :{" "}
            <span className={totalPnl > 0 ? "text-emerald-400" : totalPnl < 0 ? "text-red-400" : ""}>
              {totalPnl > 0 ? "+" : ""}${totalPnl.toFixed(2)}
            </span>
          </span>
        </div>

        {/* Confirmation banner */}
        {confirmDelete && (
          <div className="flex items-center gap-3 border-b border-red-900/40 bg-red-950/30 px-4 py-2 text-xs text-red-300">
            <span>
              {confirmDelete === "__bulk__"
                ? `Supprimer ${selected.size} trade(s) de la DB locale ? (pas de suppression sur TradeTally)`
                : `Supprimer ce trade de la DB locale ? (pas de suppression sur TradeTally)`}
            </span>
            <button
              onClick={() =>
                confirmDelete === "__bulk__"
                  ? handleDeleteSelected()
                  : handleDelete(confirmDelete)
              }
              className="rounded bg-red-800 px-2 py-0.5 text-white hover:bg-red-700"
            >
              Confirmer
            </button>
            <button
              onClick={() => setConfirmDelete(null)}
              className="rounded px-2 py-0.5 text-muted-foreground hover:text-foreground"
            >
              Annuler
            </button>
          </div>
        )}

        {/* Table */}
        {isLoading ? (
          <div className="flex flex-1 items-center justify-center text-sm text-muted-foreground">
            Chargement...
          </div>
        ) : rows.length === 0 ? (
          <div className="flex flex-1 items-center justify-center text-sm text-muted-foreground">
            Aucun trade dans la base de donnees locale.
          </div>
        ) : (
          <div className="flex-1 overflow-auto">
            <table className="w-max border-collapse text-[11px]">
              <thead className="sticky top-0 z-10 bg-card">
                <tr className="border-b border-border">
                  {/* Checkbox column */}
                  <th className="w-8 px-2 py-1.5">
                    <input
                      type="checkbox"
                      checked={selected.size === rows.length && rows.length > 0}
                      onChange={toggleSelectAll}
                      className="h-3 w-3 accent-sky-500"
                    />
                  </th>
                  {COLS.map((c) => {
                    const active = sort?.key === c.key;
                    return (
                      <th
                        key={c.key}
                        onClick={() => toggleSort(c.key)}
                        style={{ width: c.w, minWidth: c.w }}
                        className="cursor-pointer select-none border-r border-border/40 px-1.5 py-1.5 text-left font-semibold text-muted-foreground hover:text-foreground"
                      >
                        <div className="flex items-center gap-0.5">
                          <span className="truncate">{c.header}</span>
                          {active ? (
                            sort!.dir === "asc" ? (
                              <ArrowUp className="h-3 w-3 shrink-0 text-foreground" />
                            ) : (
                              <ArrowDown className="h-3 w-3 shrink-0 text-foreground" />
                            )
                          ) : (
                            <ChevronsUpDown className="h-3 w-3 shrink-0 opacity-20" />
                          )}
                        </div>
                      </th>
                    );
                  })}
                  {/* Actions column */}
                  <th className="w-10 px-1.5 py-1.5 text-center text-muted-foreground">
                    <Trash2 className="mx-auto h-3 w-3" />
                  </th>
                </tr>
              </thead>
              <tbody>
                {rows.map((row, i) => (
                  <tr
                    key={row.trade_id}
                    className={cn(
                      "border-b border-border/30 hover:bg-accent/30",
                      i % 2 ? "bg-transparent" : "bg-muted/20",
                      selected.has(row.trade_id) && "bg-sky-900/20",
                    )}
                  >
                    <td className="px-2 py-1">
                      <input
                        type="checkbox"
                        checked={selected.has(row.trade_id)}
                        onChange={() => toggleSelect(row.trade_id)}
                        className="h-3 w-3 accent-sky-500"
                      />
                    </td>
                    {COLS.map((c) => {
                      if (c.key === "open") {
                        return (
                          <td
                            key={c.key}
                            style={{ width: c.w, minWidth: c.w, maxWidth: c.w }}
                            className="overflow-hidden truncate whitespace-nowrap border-r border-border/20 px-1.5 py-1 text-center"
                          >
                            {row.open ? (
                              <span className="rounded bg-sky-900/40 px-1.5 py-0.5 text-[10px] text-sky-400">
                                Open
                              </span>
                            ) : (
                              <span className="rounded bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">
                                Closed
                              </span>
                            )}
                          </td>
                        );
                      }
                      if (c.key === "has_note") {
                        return (
                          <td
                            key={c.key}
                            style={{ width: c.w, minWidth: c.w, maxWidth: c.w }}
                            className="border-r border-border/20 px-1.5 py-1 text-center"
                          >
                            {row.has_note ? (
                              <NotebookPen className="mx-auto h-3 w-3 text-emerald-400" />
                            ) : (
                              <span className="text-muted-foreground/30">·</span>
                            )}
                          </td>
                        );
                      }
                      if (c.key === "has_screenshot") {
                        return (
                          <td
                            key={c.key}
                            style={{ width: c.w, minWidth: c.w, maxWidth: c.w }}
                            className="border-r border-border/20 px-1.5 py-1 text-center"
                          >
                            {row.has_screenshot ? (
                              <Camera className="mx-auto h-3 w-3 text-emerald-400" />
                            ) : (
                              <span className="text-muted-foreground/30">·</span>
                            )}
                          </td>
                        );
                      }
                      if (c.key === "sent_to_tradetally" || c.key === "synced_on_tradetally") {
                        const val = row[c.key];
                        return (
                          <td
                            key={c.key}
                            style={{ width: c.w, minWidth: c.w, maxWidth: c.w }}
                            className="border-r border-border/20 px-1.5 py-1 text-center"
                          >
                            {val ? (
                              <Check className="mx-auto h-3 w-3 text-emerald-400" />
                            ) : (
                              <X className="mx-auto h-3 w-3 text-muted-foreground/30" />
                            )}
                          </td>
                        );
                      }
                      return (
                        <td
                          key={c.key}
                          style={{ width: c.w, minWidth: c.w, maxWidth: c.w }}
                          className={cn(
                            "overflow-hidden truncate whitespace-nowrap border-r border-border/20 px-1.5 py-1 tabular-nums",
                            c.key === "symbol" && "font-semibold text-foreground",
                          )}
                        >
                          {formatCell(c.kind, row[c.key])}
                        </td>
                      );
                    })}
                    <td className="px-1.5 py-1 text-center">
                      <button
                        onClick={() => setConfirmDelete(row.trade_id)}
                        title="Supprimer ce trade de la DB locale"
                        className="rounded p-0.5 text-muted-foreground/50 transition-colors hover:bg-red-900/30 hover:text-red-400"
                      >
                        <Trash2 className="h-3 w-3" />
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
