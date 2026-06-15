import { useEffect, useMemo, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Search } from "lucide-react";
import { api } from "@/lib/tauri";
import { useUiStore } from "@/stores/uiStore";
import { useLayoutStore } from "@/stores/layoutStore";
import { fmtCompact } from "@/components/chartZoneParts";
import { cn } from "@/lib/utils";

const MAX_RESULTS = 50;

/** Global ticker search. Typing any letter (outside an input, with no modal open)
 *  opens a Spotlight-style overlay; selecting a ticker opens it in the active
 *  tab's first free zone as a manual 5-minute chart (VWAP + Bollinger). */
export function TickerSpotlight() {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [highlight, setHighlight] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  const activeSession  = useUiStore((s) => s.activeSession);
  const openModal      = useUiStore((s) => s.openModal);
  const openTicker     = useLayoutStore((s) => s.openTickerInZone);

  // Universe is fetched lazily on first open, then cached for the session.
  const { data: universe } = useQuery({
    queryKey: ["streamable_universe"],
    queryFn:  () => api.getStreamableUniverse(),
    enabled:  open,
    staleTime: 30 * 60 * 1000,
  });

  // ── Global trigger: a letter opens the spotlight prefilled with it ─────────
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (open) return;
      if (e.ctrlKey || e.metaKey || e.altKey) return;
      if (openModal != null) return;
      const el = document.activeElement as HTMLElement | null;
      const tag = el?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || el?.isContentEditable) return;
      if (/^[a-zA-Z]$/.test(e.key)) {
        setQuery(e.key.toUpperCase());
        setHighlight(0);
        setOpen(true);
        e.preventDefault();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, openModal]);

  useEffect(() => {
    if (open) requestAnimationFrame(() => inputRef.current?.focus());
  }, [open]);

  const results = useMemo(() => {
    const q = query.trim().toUpperCase();
    if (!q || !universe) return [];
    const prefix: typeof universe = [];
    const contains: typeof universe = [];
    for (const u of universe) {
      const s = u.symbol.toUpperCase();
      if (s.startsWith(q)) prefix.push(u);
      else if (s.includes(q)) contains.push(u);
    }
    prefix.sort((a, b) => a.symbol.localeCompare(b.symbol));
    contains.sort((a, b) => a.symbol.localeCompare(b.symbol));
    return [...prefix, ...contains].slice(0, MAX_RESULTS);
  }, [query, universe]);

  useEffect(() => { setHighlight((h) => Math.min(h, Math.max(0, results.length - 1))); }, [results.length]);

  const close = () => { setOpen(false); setQuery(""); setHighlight(0); };
  const select = (symbol: string) => { openTicker(symbol, activeSession); close(); };

  if (!open) return null;

  return (
    <div
      className="fixed inset-0 z-[100] flex items-start justify-center bg-black/50 pt-[15vh]"
      onMouseDown={(e) => { if (e.target === e.currentTarget) close(); }}
    >
      <div className="w-[34rem] max-w-[90vw] overflow-hidden rounded-lg border border-border bg-zinc-900 shadow-2xl">
        <div className="flex items-center gap-2 border-b border-border px-3 py-2">
          <Search className="h-4 w-4 text-muted-foreground" />
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => { setQuery(e.target.value); setHighlight(0); }}
            onKeyDown={(e) => {
              if (e.key === "ArrowDown") { e.preventDefault(); setHighlight((h) => Math.min(h + 1, results.length - 1)); }
              else if (e.key === "ArrowUp") { e.preventDefault(); setHighlight((h) => Math.max(h - 1, 0)); }
              else if (e.key === "Enter") { e.preventDefault(); if (results[highlight]) select(results[highlight].symbol); }
              else if (e.key === "Escape") { e.preventDefault(); close(); }
            }}
            placeholder="Rechercher un ticker…"
            className="flex-1 bg-transparent text-sm text-foreground placeholder-muted-foreground/50 outline-none"
          />
          <span className="text-[10px] text-muted-foreground/50">↑↓ · Entrée · Échap</span>
        </div>

        <div className="max-h-[50vh] overflow-y-auto">
          {results.length === 0 ? (
            <div className="px-3 py-6 text-center text-xs text-muted-foreground/50">
              {query.trim() ? "Aucun ticker" : "Tapez pour rechercher"}
            </div>
          ) : (
            results.map((u, i) => (
              <button
                key={u.symbol}
                onMouseEnter={() => setHighlight(i)}
                onClick={() => select(u.symbol)}
                className={cn(
                  "flex w-full items-center gap-3 px-3 py-1.5 text-left",
                  i === highlight ? "bg-accent" : "hover:bg-accent/50",
                )}
              >
                <span className="w-16 shrink-0 text-sm font-bold tabular-nums">{u.symbol}</span>
                <span className="flex-1 truncate text-[11px] text-muted-foreground">
                  {u.industry ?? "—"}
                </span>
                <span className="shrink-0 text-[10px] text-muted-foreground/70">{u.country ?? ""}</span>
                <span className="w-16 shrink-0 text-right text-[10px] tabular-nums text-muted-foreground/70">
                  {u.float_shares != null ? `${fmtCompact(u.float_shares)} fl` : ""}
                </span>
              </button>
            ))
          )}
        </div>
      </div>
    </div>
  );
}
