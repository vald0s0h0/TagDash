// Left sidebar of the « Revue » tab, top to bottom: the review tag in large type
// (« Manqué » badge for a missed trade, the screenshots button), the few metrics
// that matter, the executions to the millisecond, the trade notes filling all the
// height left (editable — TradeTally's own for a taken trade, TagDash's for a
// missed one), the review of the day, then the daily and 5-minute context charts.
//
// The replay plays a ticker's whole day: the sidebar shows the trade on screen and
// moves to the next one 5 s before its entry. What is typed for each trade (notes,
// review) is kept per trade in `drafts` while the others play. « Suivant » saves
// the edited notes, appends the review to the trade's notes as
// `Review YYYY-MM-DD : …` (below any earlier review) and moves to the day's next
// trade — the next day only after its last one.
//
// Works offline: a TradeTally trade's edits are saved locally at once and queued
// for upstream (see `review::outbox`); the badge by « Suivant » shows what waits.

import { useEffect, useMemo, useState } from "react";
import { ArrowRight, Check, CloudOff, LoaderCircle, Save } from "lucide-react";
import { cn } from "@/lib/utils";
import { api } from "@/lib/tauri";
import { nyDateKey, nyHmsMillis } from "@/lib/nyTime";
import type { ReviewDetail, ReviewRef, StrategyCard } from "@/types";
import { computeMetrics, type PlayFill } from "./model";
import { ContextCharts } from "./ContextCharts";
import { OutboxBadge, useRefreshOutbox, useReviewOutbox } from "./OutboxBadge";
import { imageSources } from "./queue";
import { TradeImagesButton } from "./TradeImages";

/** What was typed for one trade while the group is on screen. `notes` null until
 *  read from the source of truth; `saved` = what the source holds. */
export interface Draft {
  notes: string | null;
  saved: string | null;
  review: string;
}

interface Props {
  /** The trade on screen and its own fills. */
  detail: ReviewDetail;
  fills: PlayFill[];
  /** Its place in the ticker's day. */
  index: number;
  count: number;
  drafts: Map<string, Draft>;
  /** The whole day, for the context charts: every fill, each trade's levels. */
  allFills: PlayFill[];
  levelsOf: (trade: string) => { sl: number | null; tp: number | null };
  card: StrategyCard | null;
  onNext: () => Promise<void>;
  /** Append the review to the notes now; resolves to the notes as they then read. */
  onSaveReview: () => Promise<string>;
  onSkip: () => void;
}

const fmt = (v: number | null, digits = 2, suffix = "") =>
  v == null || !Number.isFinite(v) ? "—" : `${v.toFixed(digits)}${suffix}`;
const signed = (v: number | null, digits = 2, suffix = "") =>
  v == null || !Number.isFinite(v) ? "—" : `${v > 0 ? "+" : ""}${v.toFixed(digits)}${suffix}`;

const MONTHS = [
  "janvier", "février", "mars", "avril", "mai", "juin",
  "juillet", "août", "septembre", "octobre", "novembre", "décembre",
];

/** "2026-07-14" → « 14 juillet 2026 » and « il y a 2 mois », against today in New York. */
function dayLabel(day: string): { date: string; ago: string | null } {
  const m = /^(\d{4})-(\d{2})-(\d{2})/.exec(day);
  if (!m) return { date: day, ago: null };
  const [y, mo, d] = [Number(m[1]), Number(m[2]), Number(m[3])];
  const date = `${d} ${MONTHS[mo - 1]} ${y}`;
  const [ty, tm, td] = nyDateKey(new Date()).split("-").map(Number);
  const days = Math.round((Date.UTC(ty, tm - 1, td) - Date.UTC(y, mo - 1, d)) / 86_400_000);
  if (days < 0) return { date, ago: null };
  if (days === 0) return { date, ago: "aujourd'hui" };
  if (days === 1) return { date, ago: "hier" };
  if (days < 31) return { date, ago: `il y a ${days} jours` };
  const months = Math.max(1, (ty - y) * 12 + (tm - mo) - (td < d ? 1 : 0));
  if (months < 12) return { date, ago: `il y a ${months} mois` };
  const years = Math.floor(months / 12);
  const rest = months % 12;
  return { date, ago: `il y a ${years} an${years > 1 ? "s" : ""}${rest ? ` et ${rest} mois` : ""}` };
}

export function ReviewSidebar({
  detail, fills, index, count, drafts, allFills, levelsOf, card, onNext, onSaveReview, onSkip,
}: Props) {
  const item = detail.item;
  const target: ReviewRef = { kind: item.kind, id: item.id };
  const m = useMemo(() => computeMetrics(detail, fills), [detail, fills]);
  const missed = item.kind === "missed";
  const { data: outbox } = useReviewOutbox();
  const refreshOutbox = useRefreshOutbox();
  // This trade has edits saved locally that TradeTally could not take yet (a send
  // simply in flight is not worth flagging).
  const waiting = missed ? undefined : outbox?.targets.find((t) => t.tt_id === item.id);
  const unsent = !!waiting && (outbox!.offline || waiting.attempts > 0);

  const draftOf = (id: string): Draft => {
    let d = drafts.get(id);
    if (!d) {
      d = { notes: null, saved: null, review: "" };
      drafts.set(id, d);
    }
    return d;
  };

  // Notes: read from the source of truth the first time a trade is shown, then
  // kept (with any edit) in its draft while the other trades of the day play.
  const [notes, setNotesState0] = useState<string>("");
  const [savedNotes, setSavedNotes] = useState<string>("");
  const [notesState, setNotesState] = useState<"loading" | "idle" | "saving" | "saved" | "error">("loading");
  const [notesError, setNotesError] = useState<string | null>(null);
  const [review, setReviewState] = useState("");
  useEffect(() => {
    const d = draftOf(item.id);
    setReviewState(d.review);
    setNotesError(null);
    if (d.notes != null) {
      setNotesState0(d.notes);
      setSavedNotes(d.saved ?? "");
      setNotesState("idle");
      return;
    }
    let cancelled = false;
    setNotesState("loading");
    api.reviewGetNotes(target).then(
      (n) => {
        if (cancelled) return;
        d.notes = d.notes ?? n;
        d.saved = n;
        setNotesState0(d.notes);
        setSavedNotes(n);
        setNotesState("idle");
      },
      (e) => {
        if (cancelled) return;
        setNotesError(String(e));
        setNotesState("error");
      },
    );
    return () => { cancelled = true; };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [item.kind, item.id]);

  const setNotes = (v: string) => {
    setNotesState0(v);
    draftOf(item.id).notes = v;
  };
  const setReview = (v: string) => {
    setReviewState(v);
    draftOf(item.id).review = v;
  };

  const saveNotes = async () => {
    setNotesState("saving");
    setNotesError(null);
    try {
      await api.reviewSaveNotes(target, notes);
      draftOf(item.id).saved = notes;
      setSavedNotes(notes);
      refreshOutbox();
      setNotesState("saved");
      setTimeout(() => setNotesState("idle"), 1500);
    } catch (e) {
      setNotesError(String(e));
      setNotesState("error");
    }
  };

  const [busy, setBusy] = useState(false);
  const [nextError, setNextError] = useState<string | null>(null);
  const [reviewSave, setReviewSave] = useState<"idle" | "saving" | "saved">("idle");

  const saveReview = async () => {
    setReviewSave("saving");
    setNextError(null);
    try {
      const n = await onSaveReview();
      setNotesState0(n);
      setSavedNotes(n);
      setReviewState("");
      refreshOutbox();
      setReviewSave("saved");
      setTimeout(() => setReviewSave("idle"), 1500);
    } catch (e) {
      setNextError(String(e));
      setReviewSave("idle");
    }
  };

  const next = async () => {
    setBusy(true);
    setNextError(null);
    try {
      await onNext();
      refreshOutbox();
    } catch (e) {
      setNextError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const refLabel = m.entryMs != null ? "à l'entrée" : "à l'alerte";

  const title = item.tag ?? item.setup ?? "Sans tag";
  const when = dayLabel(item.day);
  const images = useMemo(() => imageSources(detail), [detail]);

  return (
    <aside className="flex w-[340px] shrink-0 flex-col overflow-hidden border-r border-border bg-card">
      {/* Tag · ticker */}
      <div className="shrink-0 border-b border-border px-3 py-2">
        <div className="flex items-center gap-2">
          <span className="truncate text-2xl font-semibold leading-tight tracking-tight" title={item.setup ?? undefined}>
            {title}
          </span>
          {count > 1 && (
            <span
              className="shrink-0 rounded bg-accent px-1.5 py-0.5 text-[10px] tabular-nums text-foreground"
              title={`Trade ${index + 1} sur ${count} de ${item.symbol} ce jour-là`}
            >
              {index + 1}/{count}
            </span>
          )}
          <span className="ml-auto" />
          {missed && (
            <span className="shrink-0 rounded bg-red-600/20 px-1.5 py-0.5 text-[10px] font-medium text-red-400">
              Manqué
            </span>
          )}
          <TradeImagesButton
            sources={images}
            title={`${item.symbol} · ${when.date}${missed ? " · captures TagDash" : " · TradeTally"}`}
          />
        </div>
        <div className="mt-0.5 flex flex-wrap items-baseline gap-x-2 text-[11px] text-muted-foreground">
          <span className="text-sm font-medium text-foreground">{item.symbol}</span>
          <span title={item.day}>
            {when.date}
            {when.ago && <span className="text-muted-foreground/70"> · {when.ago}</span>}
          </span>
          {!missed && m.pnl != null && (
            <span className={m.pnl >= 0 ? "text-emerald-400" : "text-red-400"}>{signed(m.pnl, 2, " $")}</span>
          )}
        </div>
      </div>

      <div className="shrink-0 space-y-3 px-3 pt-3">
        {/* Metrics */}
        <Section title="Métriques">
          <div className="grid grid-cols-2 gap-x-3 gap-y-1.5 text-xs">
            {!missed && (
              <>
                <Metric label="R réalisé" value={signed(m.realizedR, 2, " R")} tone={m.realizedR} />
                <Metric label="MAE / MFE" value={`${fmt(m.maeR, 2)} / ${fmt(m.mfeR, 2)} R`} />
              </>
            )}
            {m.alertPrice != null && (
              <Metric
                label={missed ? "Prix d'alerte" : "Alerte → entrée"}
                value={missed ? fmt(m.alertPrice, m.alertPrice < 1 ? 4 : 2) : `${signed(m.alertToEntryCents, 1, "¢")} · ${signed(m.alertToEntryPct, 1, "%")}`}
              />
            )}
            <Metric label={`Spread ${refLabel}`} value={`${fmt(m.spreadCents, 1, "¢")} · ${fmt(m.spreadPct, 2, "%")}`} />
            <Metric
              label={`Speed of tape ${refLabel}`}
              value={`${fmt(m.tapeAtRef, 1)} p/s · moy. ${fmt(m.tapeAvg, 1)}`}
            />
          </div>
        </Section>

        {/* Executions */}
        {fills.length > 0 && (
          <Section title="Exécutions">
            <div className="max-h-28 space-y-1 overflow-y-auto font-mono text-[11px] tabular-nums">
              {fills.map((f, i) => {
                const lat =
                  f.sent_ms != null && f.ack_ms != null ? `ack ${f.ack_ms - f.sent_ms} ms` : null;
                // Received on the real clock, filled on the market clock: only
                // comparable live (a replayed fill is years away from its receipt).
                const rxMs = f.recv_ms != null ? f.recv_ms - f.ms : null;
                const rx = rxMs != null && rxMs >= 0 && rxMs < 60_000 ? `rx ${rxMs} ms` : null;
                return (
                  <div key={i} className="flex items-baseline gap-2">
                    <span className="text-muted-foreground">{nyHmsMillis(f.ms)}</span>
                    <span className={f.qty > 0 ? "text-emerald-400" : "text-red-400"}>
                      {f.qty > 0 ? "B" : "S"} {Math.abs(f.qty)}
                    </span>
                    <span>@ {f.price.toFixed(f.price < 1 ? 4 : 2)}</span>
                    {(lat || rx) && (
                      <span className="ml-auto text-[10px] text-muted-foreground" title="Envoi→accusé broker · fill→réception TagDash">
                        {[lat, rx].filter(Boolean).join(" · ")}
                      </span>
                    )}
                  </div>
                );
              })}
            </div>
          </Section>
        )}
      </div>

      {/* Notes — every remaining pixel. The voice notes and earlier reviews are
          already in them (appended by the dictée and by « Suivant »). */}
      <div className="flex min-h-[96px] flex-1 flex-col px-3 pt-3">
        <div className="mb-1.5 flex items-center justify-between">
          <h3 className="text-[10px] font-medium uppercase tracking-wide text-muted-foreground">
            {missed ? "Note (TagDash)" : "Notes TradeTally"}
          </h3>
          {notesState === "loading" || notesState === "saving" ? (
            <LoaderCircle className="h-3 w-3 animate-spin text-muted-foreground" />
          ) : notesState === "saved" ? (
            <Check className="h-3 w-3 text-emerald-400" />
          ) : notes !== savedNotes ? (
            <button type="button" onClick={saveNotes} title="Enregistrer les notes" className="flex items-center gap-1 text-[10px] text-sky-400 hover:text-sky-300">
              <Save className="h-3 w-3" /> Enregistrer
            </button>
          ) : unsent ? (
            <span
              className="flex items-center gap-1 text-[10px] text-amber-400"
              title="Enregistré dans TagDash — sera envoyé à TradeTally au retour de la connexion"
            >
              <CloudOff className="h-3 w-3" /> Non synchronisé
            </span>
          ) : null}
        </div>
        <textarea
          value={notes}
          onChange={(e) => setNotes(e.target.value)}
          disabled={notesState === "loading"}
          className="min-h-0 w-full flex-1 resize-none rounded-md border border-border bg-background px-2 py-1.5 text-xs leading-relaxed outline-none focus:border-sky-600"
          placeholder={missed ? "Pourquoi ce trade n'a pas été pris…" : "Aucune note"}
        />
        {notesError && <p className="mt-1 text-[10px] text-red-400">{notesError}</p>}
      </div>

      {/* Review of the day + Suivant */}
      <div className="shrink-0 px-3 py-2">
        <div className="mb-1.5 flex items-center justify-between">
          <h3 className="text-[10px] font-medium uppercase tracking-wide text-muted-foreground">Review du jour</h3>
          {reviewSave === "saving" ? (
            <LoaderCircle className="h-3 w-3 animate-spin text-muted-foreground" />
          ) : reviewSave === "saved" ? (
            <Check className="h-3 w-3 text-emerald-400" />
          ) : review.trim() ? (
            <button
              type="button"
              onClick={saveReview}
              disabled={busy || notesState === "loading"}
              title="Ajouter la review en bas des notes, sans passer au trade suivant"
              className="flex items-center gap-1 text-[10px] text-sky-400 hover:text-sky-300 disabled:opacity-60"
            >
              <Save className="h-3 w-3" /> Enregistrer
            </button>
          ) : null}
        </div>
        <textarea
          value={review}
          onChange={(e) => setReview(e.target.value)}
          rows={2}
          className="w-full resize-none rounded-md border border-border bg-background px-2 py-1.5 text-xs outline-none focus:border-sky-600"
          placeholder={count > 1 ? "Review de ce trade — ajoutée en bas de ses notes" : "Ajoutée en bas des notes"}
        />
        {nextError && <p className="mb-1 text-[10px] text-red-400">{nextError}</p>}
        <div className="mt-1 flex items-center gap-2">
          <button
            type="button"
            onClick={onSkip}
            disabled={busy}
            className="text-[11px] text-muted-foreground hover:text-foreground"
            title="Passer sans enregistrer de review"
          >
            Passer
          </button>
          <OutboxBadge />
          <button
            type="button"
            onClick={next}
            disabled={busy || reviewSave === "saving"}
            title={index < count - 1 ? `Trade suivant de ${item.symbol} ce jour-là` : undefined}
            className="ml-auto flex items-center gap-1.5 rounded-md bg-sky-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-sky-500 disabled:opacity-60"
          >
            {busy ? <LoaderCircle className="h-3.5 w-3.5 animate-spin" /> : <ArrowRight className="h-3.5 w-3.5" />}
            Suivant
            {index < count - 1 && <span className="tabular-nums opacity-75">· T{index + 2}/{count}</span>}
          </button>
        </div>
      </div>

      <ContextCharts data={detail.data} symbol={item.symbol} fills={allFills} levelsOf={levelsOf} card={card} />
    </aside>
  );
}

function Section({ title, right, children }: { title: string; right?: React.ReactNode; children: React.ReactNode }) {
  return (
    <section>
      <div className="mb-1.5 flex items-center justify-between">
        <h3 className="text-[10px] font-medium uppercase tracking-wide text-muted-foreground">{title}</h3>
        {right}
      </div>
      {children}
    </section>
  );
}

function Metric({ label, value, tone }: { label: string; value: string; tone?: number | null }) {
  return (
    <div>
      <div className="text-[10px] text-muted-foreground">{label}</div>
      <div
        className={cn(
          "font-mono tabular-nums",
          tone != null && tone > 0 && "text-emerald-400",
          tone != null && tone < 0 && "text-red-400",
        )}
      >
        {value}
      </div>
    </div>
  );
}
