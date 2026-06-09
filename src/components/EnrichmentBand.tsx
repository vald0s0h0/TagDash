import type { ReactNode } from "react";
import { LoaderCircle, Sparkles } from "lucide-react";
import type { AlertEnrichment } from "@/types";
import { cn } from "@/lib/utils";

/** Strategy whose LLM read is manual (button-triggered) rather than automatic. */
const PANIC_ID = "panic_mean_reversion";

// Thin progressive info band for strategies that declare enrichment (e.g.
// micro_pullback). The common info (strategy name + priority badge) stays in the
// zone header; this band adds only the strategy-specific, asynchronously-filled
// fields, each showing a loading spinner until its value lands.

function fmtFloat(v: number | null): string {
  if (v == null) return "—";
  if (v >= 1_000_000) return `${(v / 1_000_000).toFixed(1)}M`;
  if (v >= 1_000)     return `${(v / 1_000).toFixed(0)}K`;
  return String(Math.round(v));
}

const CLASS_BADGE: Record<string, { label: string; cls: string }> = {
  momo_former: { label: "Momo former", cls: "bg-emerald-900/50 text-emerald-300" },
  pump_dump:   { label: "Pump & dump", cls: "bg-red-900/60 text-red-300" },
};

function Spinner() {
  return <LoaderCircle className="h-2.5 w-2.5 animate-spin text-blue-400/70" />;
}

function Chip({ label, children }: { label: string; children: ReactNode }) {
  return (
    <span className="flex items-center gap-0.5">
      <span className="text-[8px] uppercase tracking-wide text-muted-foreground/45">{label}</span>
      <span className="flex items-center gap-0.5 text-[10px] tabular-nums text-foreground/80">
        {children}
      </span>
    </span>
  );
}

export function EnrichmentBand({
  e,
  onRunLlm,
}: {
  e: AlertEnrichment;
  /** Fired by the manual "Analyser (IA)" button (panic strategy only). */
  onRunLlm?: () => void;
}) {
  const loading = e.status === "loading";
  const isPanic = e.strategy_id === PANIC_ID;
  const hasPanicResult = e.llm_context != null || e.llm_reversion != null;

  return (
    <div className="flex flex-wrap items-center gap-x-2.5 gap-y-0.5 px-2 pt-1">
      {/* Float (immediate) */}
      <Chip label="Float">
        {e.float_shares != null ? `${fmtFloat(e.float_shares)} fl` : loading ? <Spinner /> : "—"}
      </Chip>

      {/* Country — red badge for China / Hong Kong */}
      <Chip label="Pays">
        {e.country != null ? (
          <span
            className={cn(
              "rounded px-1 py-0.5 text-[9px] font-semibold",
              e.country_flagged ? "bg-red-900/60 text-red-300" : "bg-muted text-muted-foreground",
            )}
          >
            {e.country}
          </span>
        ) : loading ? (
          <Spinner />
        ) : (
          "—"
        )}
      </Chip>

      {/* Industry */}
      {e.industry && (
        <Chip label="Industrie">
          <span className="max-w-[120px] truncate">{e.industry}</span>
        </Chip>
      )}

      {/* Days since split (after daily calc) */}
      <Chip label="Split">
        {e.days_since_split != null && e.split_label ? (
          `${e.days_since_split}j depuis split ${e.split_label}`
        ) : e.daily_done ? (
          "—"
        ) : (
          <Spinner />
        )}
      </Chip>

      {/* Classification badge (green momo / red pump / nothing) */}
      {e.daily_done ? (
        e.classification && CLASS_BADGE[e.classification] ? (
          <span
            className={cn(
              "rounded px-1.5 py-0.5 text-[9px] font-bold uppercase",
              CLASS_BADGE[e.classification].cls,
            )}
          >
            {CLASS_BADGE[e.classification].label}
          </span>
        ) : null
      ) : (
        <Chip label="Profil">
          <Spinner />
        </Chip>
      )}

      {/* News title, or red "no news" badge once checked */}
      <Chip label="News">
        {e.news_title ? (
          <span className="max-w-[200px] truncate" title={e.news_url ?? undefined}>
            {e.news_title}
          </span>
        ) : e.news_checked ? (
          <span className="rounded bg-red-900/60 px-1 py-0.5 text-[9px] font-semibold text-red-300">
            no news
          </span>
        ) : (
          <Spinner />
        )}
      </Chip>

      {/* ── LLM, micro_pullback (auto) — dilution + news bluff/solid ── */}
      {!isPanic && (e.llm_dilution || e.llm_pending) && (
        <Chip label="Dilution">
          {e.llm_dilution ?? <Spinner />}
        </Chip>
      )}
      {!isPanic && (e.llm_news || (e.llm_pending && e.news_title)) && (
        <Chip label="News?">
          {e.llm_news ?? <Spinner />}
        </Chip>
      )}

      {/* ── LLM, panic_mean_reversion (manual) — button + context + verdict ── */}
      {isPanic && (
        <>
          {/* The call is NEVER automatic: the user fires it from this button. */}
          <button
            type="button"
            onClick={onRunLlm}
            disabled={e.llm_pending}
            className={cn(
              "flex items-center gap-1 rounded px-1.5 py-0.5 text-[9px] font-semibold",
              "bg-blue-900/50 text-blue-200 hover:bg-blue-800/60 disabled:opacity-50",
            )}
            title="Analyser le contexte et la probabilité de retour à l'équilibre (Deepseek)"
          >
            {e.llm_pending ? <Spinner /> : <Sparkles className="h-2.5 w-2.5" />}
            {hasPanicResult ? "Réanalyser" : "Analyser (IA)"}
          </button>

          {(e.llm_context || e.llm_pending) && (
            <Chip label="Contexte">
              <span className="max-w-[260px] truncate" title={e.llm_context ?? undefined}>
                {e.llm_context ?? <Spinner />}
              </span>
            </Chip>
          )}
          {(e.llm_reversion || e.llm_pending) && (
            <Chip label="Verdict">
              <span className="max-w-[260px] truncate" title={e.llm_reversion ?? undefined}>
                {e.llm_reversion ?? <Spinner />}
              </span>
            </Chip>
          )}
        </>
      )}
    </div>
  );
}
