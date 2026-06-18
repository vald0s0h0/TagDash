import { useEffect, useMemo, useState } from "react";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { ArrowDown, ArrowUp, ChevronsUpDown, Database, RefreshCw, Search } from "lucide-react";
import { Dialog, DialogContent } from "@/components/ui/dialog";
import { api } from "@/lib/tauri";
import type { TickerTableRow } from "@/types";
import { cn } from "@/lib/utils";

interface Props {
  open: boolean;
  onClose: () => void;
}

/** Max rows returned per query (the universe is too big to load whole). */
const LIMIT = 200;

// ─── Column definitions (key · header · width · value kind) ────────────────────

type Kind = "text" | "int" | "compact" | "usd" | "pct" | "num" | "bool" | "date" | "count" | "score";

interface Col {
  key: keyof TickerTableRow;
  header: string;
  w: number;
  kind: Kind;
}

/** Columns organised by data THEME — each group spans a header above its columns. */
interface ColGroup {
  title: string;
  cols: Col[];
}

const GROUPS: ColGroup[] = [
  {
    title: "Identité",
    cols: [
      { key: "symbol", header: "Ticker", w: 78, kind: "text" },
      { key: "name", header: "Nom", w: 200, kind: "text" },
      { key: "exchange", header: "Exch", w: 70, kind: "text" },
      { key: "country", header: "Pays", w: 110, kind: "text" },
      { key: "industry", header: "Industrie", w: 200, kind: "text" },
      { key: "sector", header: "Secteur", w: 140, kind: "text" },
      { key: "sic", header: "SIC", w: 64, kind: "text" },
    ],
  },
  {
    title: "Fondamentaux",
    cols: [
      { key: "float_shares", header: "Float", w: 90, kind: "compact" },
      { key: "market_cap", header: "Mkt cap", w: 100, kind: "compact" },
      { key: "avg_volume", header: "Vol moy", w: 90, kind: "compact" },
      { key: "outstanding_shares", header: "Shares out", w: 100, kind: "compact" },
      { key: "shares_outstanding_12m", header: "Shares 12m", w: 100, kind: "compact" },
      { key: "free_float", header: "Free float", w: 84, kind: "num" },
    ],
  },
  {
    title: "Prix & Volatilité",
    cols: [
      { key: "prev_close", header: "Prev close", w: 88, kind: "num" },
      { key: "atr", header: "ATR", w: 70, kind: "num" },
    ],
  },
  {
    title: "Variations (close-to-close)",
    cols: [
      { key: "change_1d_pct", header: "Δ 1j", w: 66, kind: "pct" },
      { key: "change_2d_pct", header: "Δ 2j", w: 66, kind: "pct" },
      { key: "change_3d_pct", header: "Δ 3j", w: 66, kind: "pct" },
      { key: "change_4d_pct", header: "Δ 4j", w: 66, kind: "pct" },
      { key: "change_5d_pct", header: "Δ 5j", w: 66, kind: "pct" },
      { key: "change_6d_pct", header: "Δ 6j", w: 66, kind: "pct" },
    ],
  },
  {
    title: "Comportement (0–100, 100 = pire)",
    cols: [{ key: "pump_dump_score", header: "Pump&Dump", w: 92, kind: "score" }],
  },
  {
    title: "Dilution — scores (0–100, 100 = pire)",
    cols: [
      { key: "dilution_capacity_score", header: "Capacité", w: 84, kind: "score" },
      { key: "dilution_need_score", header: "Besoin", w: 78, kind: "score" },
      { key: "dilution_score", header: "Hist. %ile", w: 86, kind: "score" },
      { key: "dilution_pct_12m", header: "Δ 12m", w: 84, kind: "pct" },
    ],
  },
  {
    title: "Short Interest",
    cols: [
      { key: "short_interest_score", header: "SI score", w: 80, kind: "score" },
      { key: "short_interest", header: "Short int", w: 90, kind: "compact" },
      { key: "days_to_cover", header: "DTC", w: 64, kind: "num" },
      { key: "short_interest_settlement", header: "SI date", w: 96, kind: "date" },
    ],
  },
  {
    title: "Santé financière (SEC)",
    cols: [
      { key: "net_income_last_q", header: "NI dern.Q", w: 100, kind: "usd" },
      { key: "net_income_ttm", header: "NI TTM", w: 100, kind: "usd" },
      { key: "negative_quarters_last4", header: "Q nég /4", w: 76, kind: "int" },
      { key: "operating_cash_flow_ttm", header: "OCF TTM", w: 100, kind: "usd" },
      { key: "cash_and_equivalents", header: "Cash", w: 100, kind: "usd" },
      { key: "financials_period_end", header: "Période fin.", w: 100, kind: "date" },
    ],
  },
  {
    title: "Dilution / Filings (SEC)",
    cols: [
      { key: "has_recent_shelf", header: "Shelf S-3", w: 78, kind: "bool" },
      { key: "latest_dilution_form", header: "Form dilut.", w: 92, kind: "text" },
      { key: "latest_dilution_date", header: "Date dilut.", w: 98, kind: "date" },
      { key: "dilution_atm", header: "ATM", w: 56, kind: "bool" },
      { key: "dilution_resale", header: "Resale", w: 66, kind: "bool" },
      { key: "dilution_warrants", header: "Warr.", w: 60, kind: "bool" },
      { key: "offering_amount", header: "Offering", w: 96, kind: "usd" },
      { key: "filings_count", header: "Filings", w: 74, kind: "count" },
    ],
  },
  {
    title: "Splits",
    cols: [
      { key: "last_split_date", header: "Dernier split", w: 100, kind: "date" },
      { key: "last_split_label", header: "Ratio", w: 70, kind: "text" },
      { key: "split_count_1y", header: "Splits 1a", w: 76, kind: "int" },
    ],
  },
  {
    title: "Propriété",
    cols: [
      { key: "institutional_ownership_pct", header: "Inst. %", w: 76, kind: "pct" },
      { key: "insider_ownership_pct", header: "Insider %", w: 80, kind: "pct" },
      { key: "holders_5pct_count", header: ">5% hold.", w: 84, kind: "int" },
      { key: "restricted_shares", header: "Restricted", w: 96, kind: "compact" },
    ],
  },
  {
    title: "Méta",
    cols: [
      { key: "news_count", header: "News", w: 66, kind: "count" },
      { key: "intel_updated_at", header: "Intel maj", w: 142, kind: "date" },
    ],
  },
];

/** Flattened columns, in group order — drives the body + sort/filter. */
const COLS: Col[] = GROUPS.flatMap((g) => g.cols);

// ─── Value formatting ──────────────────────────────────────────────────────────

function fmtCompact(v: number): string {
  const a = Math.abs(v);
  if (a >= 1e9) return (v / 1e9).toFixed(2) + "B";
  if (a >= 1e6) return (v / 1e6).toFixed(2) + "M";
  if (a >= 1e3) return (v / 1e3).toFixed(1) + "K";
  return String(Math.round(v));
}

function formatCell(kind: Kind, value: unknown): React.ReactNode {
  if (kind === "bool") {
    return value ? (
      <span className="text-emerald-400">✓</span>
    ) : (
      <span className="text-muted-foreground/30">·</span>
    );
  }
  if (value === null || value === undefined || value === "") {
    return <span className="text-muted-foreground/30">—</span>;
  }
  switch (kind) {
    case "text":
      return <span title={String(value)}>{String(value)}</span>;
    case "int":
      return (value as number).toLocaleString();
    case "compact":
      return fmtCompact(value as number);
    case "num":
      return (value as number).toFixed(2);
    case "pct": {
      const n = value as number;
      return (
        <span className={n > 0 ? "text-emerald-400" : n < 0 ? "text-red-400" : undefined}>
          {n > 0 ? "+" : ""}
          {n.toFixed(2)}%
        </span>
      );
    }
    case "usd": {
      const n = value as number;
      return (
        <span className={n > 0 ? "text-emerald-400" : n < 0 ? "text-red-400" : undefined}>
          {n < 0 ? "-" : ""}${fmtCompact(Math.abs(n))}
        </span>
      );
    }
    case "count": {
      const n = value as number;
      return <span className={n > 0 ? "text-sky-400" : "text-muted-foreground/30"}>{n}</span>;
    }
    case "score": {
      // 0..100 percentile rank, 100 = worst. Grey → red as the score rises.
      const n = value as number;
      const t = Math.max(0, Math.min(1, n / 100));
      const color = `rgb(${Math.round(125 + 130 * t)}, ${Math.round(125 - 85 * t)}, ${Math.round(125 - 85 * t)})`;
      return (
        <span style={{ color }} className="font-semibold">
          {n.toFixed(0)}
        </span>
      );
    }
    case "date":
      return String(value).replace("T", " ").slice(0, 16);
    default:
      return String(value);
  }
}

type SortState = { key: keyof TickerTableRow; dir: "asc" | "desc" } | null;

export function TickersTableModal({ open, onClose }: Props) {
  // Backend-driven search (debounced) — only a bounded extract is ever loaded.
  const [search, setSearch] = useState("");
  const [debounced, setDebounced] = useState("");
  const [localFilter, setLocalFilter] = useState("");
  const [sort, setSort] = useState<SortState>(null);

  useEffect(() => {
    const t = setTimeout(() => setDebounced(search.trim()), 300);
    return () => clearTimeout(t);
  }, [search]);

  // Reset everything when the modal opens.
  useEffect(() => {
    if (open) {
      setSearch("");
      setDebounced("");
      setLocalFilter("");
      setSort(null);
    }
  }, [open]);

  const { data = [], isLoading, isFetching, refetch } = useQuery({
    queryKey: ["tickers_table", debounced],
    queryFn: () => api.getTickersTable(debounced, LIMIT),
    enabled: open,
    staleTime: 30_000,
    placeholderData: keepPreviousData,
  });

  // Client-side filter (across all columns) + sort, on the small loaded extract.
  const rows = useMemo(() => {
    let out = data;
    const q = localFilter.trim().toLowerCase();
    if (q) {
      out = out.filter((row) =>
        COLS.some((c) => String(row[c.key] ?? "").toLowerCase().includes(q))
      );
    }
    if (sort) {
      const { key, dir } = sort;
      out = [...out].sort((a, b) => {
        const av = a[key];
        const bv = b[key];
        if (av === null || av === undefined || av === "") return 1; // nulls last
        if (bv === null || bv === undefined || bv === "") return -1;
        let cmp: number;
        if (typeof av === "number" && typeof bv === "number") cmp = av - bv;
        else if (typeof av === "boolean" && typeof bv === "boolean")
          cmp = av === bv ? 0 : av ? 1 : -1;
        else cmp = String(av).localeCompare(String(bv));
        return dir === "asc" ? cmp : -cmp;
      });
    }
    return out;
  }, [data, localFilter, sort]);

  const toggleSort = (key: keyof TickerTableRow) =>
    setSort((s) =>
      s?.key === key ? (s.dir === "asc" ? { key, dir: "desc" } : null) : { key, dir: "asc" }
    );

  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="flex h-[92vh] w-[96vw] max-w-none flex-col gap-0 p-0">
        {/* Header / toolbar */}
        <div className="flex shrink-0 flex-wrap items-center gap-3 border-b border-border px-4 py-2.5 pr-12">
          <div className="flex items-center gap-2 text-sm font-semibold">
            <Database className="h-4 w-4" />
            Données tickers (DB)
          </div>

          <div className="flex items-center gap-2 rounded-md border border-border bg-background px-2 py-1">
            <Search className="h-3.5 w-3.5 text-muted-foreground" />
            <input
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder="Rechercher un ticker (ex: AAPL) ou un nom…"
              className="w-72 bg-transparent text-xs outline-none placeholder:text-muted-foreground"
            />
          </div>

          <input
            value={localFilter}
            onChange={(e) => setLocalFilter(e.target.value)}
            placeholder="Filtrer l'extrait…"
            className="rounded-md border border-border bg-background px-2 py-1 text-xs outline-none placeholder:text-muted-foreground"
          />

          <button
            onClick={() => refetch()}
            title="Rafraîchir"
            className="flex items-center gap-1 rounded px-2 py-1 text-[11px] text-muted-foreground transition-colors hover:bg-accent/50 hover:text-foreground"
          >
            <RefreshCw className={cn("h-3.5 w-3.5", isFetching && "animate-spin")} /> Rafraîchir
          </button>

          <span className="ml-auto text-[11px] tabular-nums text-muted-foreground">
            {debounced
              ? `${rows.length} résultat(s)`
              : `extrait : ${rows.length} ligne(s) récemment collectées`}
            {data.length >= LIMIT && ` · max ${LIMIT}, affinez la recherche`}
          </span>
        </div>

        {/* Table */}
        {isLoading ? (
          <div className="flex flex-1 items-center justify-center text-sm text-muted-foreground">
            Chargement…
          </div>
        ) : rows.length === 0 ? (
          <div className="flex flex-1 items-center justify-center px-6 text-center text-sm text-muted-foreground">
            {debounced
              ? `Aucun résultat pour « ${debounced} ».`
              : "Aucun ticker enrichi pour l'instant — recherchez un ticker (ex: AAPL) pour voir ses données."}
          </div>
        ) : (
          <div className="flex-1 overflow-auto">
            <table className="w-max border-collapse text-[11px]">
              <thead className="sticky top-0 z-10 bg-card">
                {/* Theme group headers — each spans its columns. */}
                <tr className="border-b border-border">
                  {GROUPS.map((g, gi) => (
                    <th
                      key={g.title}
                      colSpan={g.cols.length}
                      className={cn(
                        "select-none border-r border-border/60 px-1.5 py-1 text-center text-[10px] font-bold uppercase tracking-wide text-muted-foreground/80",
                        gi % 2 ? "bg-muted/20" : "bg-muted/40"
                      )}
                    >
                      {g.title}
                    </th>
                  ))}
                </tr>
                <tr className="border-b border-border">
                  {COLS.map((c) => {
                    const active = sort?.key === c.key;
                    return (
                      <th
                        key={c.key}
                        onClick={() => toggleSort(c.key)}
                        style={{ width: c.w, minWidth: c.w }}
                        className="cursor-pointer select-none border-r border-border/40 px-1.5 py-1.5 text-left font-semibold text-muted-foreground hover:text-foreground"
                        title="Cliquer pour trier"
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
                </tr>
              </thead>
              <tbody>
                {rows.map((row, i) => (
                  <tr
                    key={row.symbol}
                    className={cn(
                      "border-b border-border/30 hover:bg-accent/30",
                      i % 2 ? "bg-transparent" : "bg-muted/20"
                    )}
                  >
                    {COLS.map((c) => (
                      <td
                        key={c.key}
                        style={{ width: c.w, minWidth: c.w, maxWidth: c.w }}
                        className={cn(
                          "overflow-hidden truncate whitespace-nowrap border-r border-border/20 px-1.5 py-1 tabular-nums",
                          c.key === "symbol" && "font-semibold text-foreground"
                        )}
                      >
                        {formatCell(c.kind, row[c.key])}
                      </td>
                    ))}
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
