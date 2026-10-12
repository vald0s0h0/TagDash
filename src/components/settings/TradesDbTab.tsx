// Settings → Données stockées → Trades (DB).
//
// A mirror of TradeTally (the source of truth: every trade, whoever placed it) with
// what TagDash adds on top — millisecond fill timing and latencies, the market data
// recorded for the ticker, voice notes, screenshots, reviews — plus the MISSED
// trades, which exist in TagDash only. « Rafraîchir » re-syncs from TradeTally.
// Only a missed trade can be deleted here: a TradeTally trade is deleted in
// TradeTally.

import { useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  ArrowDown,
  ArrowUp,
  ChevronsUpDown,
  RefreshCw,
  Search,
  Trash2,
} from "lucide-react";
import { api } from "@/lib/tauri";
import { nyHmsMillis, nyTime } from "@/lib/nyTime";
import type { ReviewTradeRow } from "@/types";
import { cn } from "@/lib/utils";

type SortKey =
  | "category" | "symbol" | "day" | "setup" | "side" | "quantity" | "entry_price"
  | "exit_price" | "pnl" | "r_value" | "entry_ms" | "first_fill_ms" | "data_source"
  | "voice_notes" | "screenshots" | "review_count" | "source";

interface Col {
  key: SortKey;
  header: string;
  w: number;
  title?: string;
  render: (r: ReviewTradeRow) => React.ReactNode;
}

const dash = <span className="text-muted-foreground/30">-</span>;
const price = (v: number | null) => (v == null ? dash : v.toFixed(v < 1 ? 4 : 2));

function latency(r: ReviewTradeRow): React.ReactNode {
  const parts: string[] = [];
  if (r.first_sent_ms != null && r.first_ack_ms != null) parts.push(`ack ${r.first_ack_ms - r.first_sent_ms}`);
  if (r.first_recv_ms != null && r.first_fill_ms != null) {
    const rx = r.first_recv_ms - r.first_fill_ms;
    if (rx >= 0 && rx < 60_000) parts.push(`rx ${rx}`);
  }
  return parts.length ? parts.join(" · ") + " ms" : dash;
}

const COLS: Col[] = [
  {
    key: "category", header: "Type", w: 66,
    render: (r) => (
      <span
        className={cn(
          "rounded px-1.5 py-0.5 text-[10px]",
          r.category === "missed" ? "bg-red-900/40 text-red-400" : "bg-emerald-900/30 text-emerald-400",
        )}
      >
        {r.category === "missed" ? "Manqué" : "Pris"}
      </span>
    ),
  },
  { key: "symbol", header: "Ticker", w: 70, render: (r) => <span className="font-semibold">{r.symbol}</span> },
  { key: "day", header: "Jour", w: 84, render: (r) => r.day || dash },
  {
    key: "entry_ms", header: "Heure", w: 64, title: "Entrée (ou apparition pour un manqué), heure de New York",
    render: (r) => (r.entry_ms != null ? nyTime(new Date(r.entry_ms), true) : dash),
  },
  { key: "setup", header: "Setup", w: 110, render: (r) => r.setup ?? dash },
  {
    key: "side", header: "Side", w: 52,
    render: (r) =>
      r.side ? (
        <span className={r.side.toLowerCase() === "long" ? "text-emerald-400" : "text-red-400"}>{r.side}</span>
      ) : dash,
  },
  { key: "quantity", header: "Qté", w: 54, render: (r) => (r.quantity != null ? r.quantity : dash) },
  { key: "entry_price", header: "Entrée", w: 64, render: (r) => price(r.entry_price) },
  { key: "exit_price", header: "Sortie", w: 64, render: (r) => price(r.exit_price) },
  {
    key: "pnl", header: "P&L", w: 80,
    render: (r) =>
      r.pnl == null ? dash : (
        <span className={r.pnl > 0 ? "text-emerald-400" : r.pnl < 0 ? "text-red-400" : undefined}>
          {r.pnl > 0 ? "+" : ""}${r.pnl.toFixed(2)}
        </span>
      ),
  },
  { key: "r_value", header: "R", w: 50, render: (r) => (r.r_value == null ? dash : r.r_value.toFixed(2)) },
  {
    key: "first_fill_ms", header: "1er fill (ms)", w: 92, title: "Heure du 1er fill, HHMMSSmmm (New York) — TagDash uniquement",
    render: (r) => (r.first_fill_ms != null ? <span className="font-mono">{nyHmsMillis(r.first_fill_ms)}</span> : dash),
  },
  {
    key: "first_fill_ms", header: "Latences", w: 110, title: "Envoi → accusé broker · fill → réception TagDash",
    render: (r) => <span className="font-mono text-muted-foreground">{latency(r)}</span>,
  },
  {
    key: "data_source", header: "Données", w: 150, title: "Tape enregistré pour le ticker ce jour-là",
    render: (r) => (r.data_source ? <span className="text-muted-foreground">{r.data_source}</span> : dash),
  },
  { key: "voice_notes", header: "Voix", w: 44, render: (r) => (r.voice_notes ? r.voice_notes : dash) },
  { key: "screenshots", header: "Capt.", w: 44, render: (r) => (r.screenshots ? r.screenshots : dash) },
  {
    key: "review_count", header: "Revues", w: 96,
    render: (r) => (r.review_count ? `${r.review_count} · ${r.last_review ?? ""}` : dash),
  },
  {
    key: "source", header: "Source", w: 76,
    render: (r) => (r.source === "tagdash" ? "TagDash" : "TradeTally"),
  },
];

type SortState = { key: SortKey; dir: "asc" | "desc" } | null;

export function TradesDbTab() {
  const [filter, setFilter] = useState("");
  const [category, setCategory] = useState<"all" | "taken" | "missed">("all");
  const [sort, setSort] = useState<SortState>(null);
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  const [syncing, setSyncing] = useState(false);
  const [syncError, setSyncError] = useState<string | null>(null);

  const { data = [], isLoading, isFetching, refetch } = useQuery({
    queryKey: ["review_trades_db"],
    queryFn: () => api.getReviewTradesDb(),
    staleTime: 10_000,
  });

  const refresh = async () => {
    setSyncing(true);
    setSyncError(null);
    try {
      await api.syncTradetallyTrades();
    } catch (e) {
      setSyncError(String(e));
    } finally {
      setSyncing(false);
      refetch();
    }
  };

  const rows = useMemo(() => {
    let out = data;
    if (category !== "all") out = out.filter((r) => r.category === category);
    const q = filter.trim().toLowerCase();
    if (q) {
      out = out.filter(
        (r) =>
          r.symbol.toLowerCase().includes(q) ||
          r.id.toLowerCase().includes(q) ||
          (r.setup ?? "").toLowerCase().includes(q) ||
          r.tags.some((t) => t.toLowerCase().includes(q)),
      );
    }
    if (sort) {
      const { key, dir } = sort;
      out = [...out].sort((a, b) => {
        const av = a[key];
        const bv = b[key];
        if (av === null || av === undefined) return 1;
        if (bv === null || bv === undefined) return -1;
        const cmp = typeof av === "number" && typeof bv === "number" ? av - bv : String(av).localeCompare(String(bv));
        return dir === "asc" ? cmp : -cmp;
      });
    }
    return out;
  }, [data, filter, category, sort]);

  const toggleSort = (key: SortKey) =>
    setSort((s) => (s?.key === key ? (s.dir === "asc" ? { key, dir: "desc" } : null) : { key, dir: "asc" }));

  async function handleDelete(sessionId: string) {
    try {
      await api.reviewDeleteMissed(sessionId);
    } finally {
      setConfirmDelete(null);
      refetch();
    }
  }

  const totalPnl = rows.reduce((sum, r) => sum + (r.pnl ?? 0), 0);
  const missedCount = data.filter((r) => r.category === "missed").length;

  return (
    <div className="flex h-full flex-col gap-0">
      {/* Toolbar */}
      <div className="flex shrink-0 flex-wrap items-center gap-3 border-b border-border pb-2.5">
        <div className="flex items-center gap-2 rounded-md border border-border bg-background px-2 py-1">
          <Search className="h-3.5 w-3.5 text-muted-foreground" />
          <input
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder="Filtrer par ticker, setup, tag…"
            className="w-56 bg-transparent text-xs outline-none placeholder:text-muted-foreground"
          />
        </div>

        <div className="flex items-center gap-0.5 text-[11px]">
          {(["all", "taken", "missed"] as const).map((c) => (
            <button
              key={c}
              onClick={() => setCategory(c)}
              className={cn(
                "rounded px-2 py-1 transition-colors",
                category === c ? "bg-accent text-foreground" : "text-muted-foreground hover:text-foreground",
              )}
            >
              {c === "all" ? "Tous" : c === "taken" ? "Pris" : `Manqués (${missedCount})`}
            </button>
          ))}
        </div>

        <button
          onClick={refresh}
          title="Re-synchroniser depuis TradeTally"
          className="flex items-center gap-1 rounded px-2 py-1 text-[11px] text-muted-foreground transition-colors hover:bg-accent/50 hover:text-foreground"
        >
          <RefreshCw className={cn("h-3.5 w-3.5", (syncing || isFetching) && "animate-spin")} /> Rafraîchir
        </button>
        {syncError && <span className="text-[11px] text-red-400">TradeTally : {syncError}</span>}

        <span className="ml-auto text-[11px] tabular-nums text-muted-foreground">
          {rows.length} trade(s) &middot; P&L total :{" "}
          <span className={totalPnl > 0 ? "text-emerald-400" : totalPnl < 0 ? "text-red-400" : ""}>
            {totalPnl > 0 ? "+" : ""}${totalPnl.toFixed(2)}
          </span>
        </span>
      </div>

      {confirmDelete && (
        <div className="flex shrink-0 items-center gap-3 border-b border-red-900/40 bg-red-950/30 px-3 py-2 text-xs text-red-300">
          <span>Supprimer ce trade manqué (note, voix, captures, tape enregistré) ?</span>
          <button
            onClick={() => handleDelete(confirmDelete)}
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

      {isLoading ? (
        <div className="flex flex-1 items-center justify-center text-sm text-muted-foreground">Chargement…</div>
      ) : rows.length === 0 ? (
        <div className="flex flex-1 items-center justify-center text-sm text-muted-foreground">
          Aucun trade. « Rafraîchir » synchronise depuis TradeTally.
        </div>
      ) : (
        <div className="flex-1 overflow-auto">
          <table className="w-max border-collapse text-[11px]">
            <thead className="sticky top-0 z-10 bg-card">
              <tr className="border-b border-border">
                {COLS.map((c, i) => {
                  const active = sort?.key === c.key;
                  return (
                    <th
                      key={`${c.key}-${i}`}
                      onClick={() => toggleSort(c.key)}
                      title={c.title}
                      style={{ width: c.w, minWidth: c.w }}
                      className="cursor-pointer select-none border-r border-border/40 px-1.5 py-1.5 text-left font-semibold text-muted-foreground hover:text-foreground"
                    >
                      <div className="flex items-center gap-0.5">
                        <span className="truncate">{c.header}</span>
                        {active ? (
                          sort!.dir === "asc" ? <ArrowUp className="h-3 w-3 shrink-0 text-foreground" /> : <ArrowDown className="h-3 w-3 shrink-0 text-foreground" />
                        ) : (
                          <ChevronsUpDown className="h-3 w-3 shrink-0 opacity-20" />
                        )}
                      </div>
                    </th>
                  );
                })}
                <th className="w-10 px-1.5 py-1.5 text-center text-muted-foreground">
                  <Trash2 className="mx-auto h-3 w-3" />
                </th>
              </tr>
            </thead>
            <tbody>
              {rows.map((row, i) => (
                <tr
                  key={row.id}
                  title={row.local_trade_ids.length ? `TagDash : ${row.local_trade_ids.join(", ")}` : row.id}
                  className={cn("border-b border-border/30 hover:bg-accent/30", i % 2 ? "bg-transparent" : "bg-muted/20")}
                >
                  {COLS.map((c, j) => (
                    <td
                      key={`${c.key}-${j}`}
                      style={{ width: c.w, minWidth: c.w, maxWidth: c.w }}
                      className="overflow-hidden truncate whitespace-nowrap border-r border-border/20 px-1.5 py-1 tabular-nums"
                    >
                      {c.render(row)}
                    </td>
                  ))}
                  <td className="px-1.5 py-1 text-center">
                    {row.category === "missed" ? (
                      <button
                        onClick={() => setConfirmDelete(row.id)}
                        title="Supprimer ce trade manqué"
                        className="text-muted-foreground hover:text-red-400"
                      >
                        <Trash2 className="mx-auto h-3 w-3" />
                      </button>
                    ) : null}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
