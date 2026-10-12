// « Attention du marché » — root of the detached window (label "attention").
//
// Polls the backend for the map as cut at the current clock: the live data
// horizon, the simulated instant of a Market Replay, or the playhead of the trade
// under « Revue » in the main window. The reply carries a key; an unchanged key
// comes back without the map, so idle polling costs nothing.
//
// CLICKS. First click on a ticker opens its detail here. A second click on the
// same ticker sends it to the main window, which opens it the way it opens any
// ticker. Clicking another ticker opens that one's detail first.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { Settings } from "lucide-react";
import { cn } from "@/lib/utils";
import { nyTime } from "@/lib/nyTime";
import { attentionApi, ATTENTION_REVIEW_EVENT } from "./api";
import { DetailPanel } from "./DetailPanel";
import { GlyphIcon, Legend } from "./Legend";
import {
  floatRotation, fmtMoney, fmtPct, fmtRel, shown, unknownCount, visible, type Shown,
} from "./model";
import { SettingsPopover } from "./SettingsPopover";
import { useAttentionPrefs } from "./store";
import { TrajectoryMap } from "./TrajectoryMap";
import type { AttentionOptions, MapReply, MapView, Universe } from "./types";
import { WeatherBar } from "./WeatherBar";

const POLL_MS = 2_000;

const UNIVERSES: { id: Universe; label: string; title: string }[] = [
  { id: "small", label: "Micro / small", title: "Actions de capitalisation < 2 Md$" },
  { id: "all",   label: "Toutes les actions", title: "Tout l'univers actions, capitalisations inconnues comprises" },
  { id: "big",   label: "Mid / big", title: "Actions de capitalisation ≥ 2 Md$" },
  { id: "etf",   label: "ETF", title: "ETF, ETN et fonds — jamais mélangés aux actions" },
];

function Seg<T extends string>({
  value, options, onChange,
}: {
  value: T;
  options: { id: T; label: string; title?: string }[];
  onChange: (v: T) => void;
}) {
  return (
    <div className="flex overflow-hidden rounded border border-border">
      {options.map((o) => (
        <button
          key={o.id}
          type="button"
          title={o.title}
          onClick={() => onChange(o.id)}
          className={cn(
            "px-2.5 py-1 text-[11px] transition-colors",
            value === o.id
              ? "bg-foreground text-background"
              : "text-muted-foreground hover:bg-accent hover:text-foreground",
          )}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

function StatusLine({ reply, map }: { reply: MapReply | null; map: MapView | null }) {
  const st = reply?.status;
  const parts: string[] = [];
  if (reply?.mode === "review" && reply.review) {
    parts.push(`Revue · ${reply.review.day} · ${nyTime(reply.review.at_ms / 1000)} ET`);
  } else if (reply?.mode === "replay") {
    parts.push("Market Replay");
  } else {
    parts.push("Direct");
  }
  if (map) {
    parts.push(`données jusqu'à ${nyTime(map.seen_s)} ET`);
    if (map.delayed_min > 0 && reply?.mode === "live") {
      parts.push(`SIP différé ~${map.delayed_min} min (plan Alpaca Basic)`);
    }
    const hist = Math.min(map.hist_sessions[0], map.hist_sessions[1]);
    if (hist < map.hist_target) {
      parts.push(
        `historique ${hist}/${map.hist_target} séances${hist < 5 ? " — activité relative provisoire" : ""}`,
      );
    }
    if (map.no_ref > 0) parts.push(`${map.no_ref} sans clôture de référence`);
  }
  if (st && reply?.mode !== "review" && st.message && st.state !== "ready") parts.push(st.message);
  else if (st?.history_building && st.message) parts.push(st.message);
  return (
    <div className="truncate text-[11px] text-muted-foreground" title={parts.join(" · ")}>
      {parts.join(" · ")}
    </div>
  );
}

function Row({
  it, active, onPick,
}: {
  it: Shown;
  active: boolean;
  onPick: () => void;
}) {
  const rot = floatRotation(it);
  return (
    <button
      type="button"
      onClick={onPick}
      className={cn(
        "grid w-full grid-cols-[14px_56px_1fr_1fr_1fr_52px] items-center gap-1 px-2 py-1 text-left text-[11px] tabular-nums",
        active ? "bg-accent" : "hover:bg-accent/50",
      )}
    >
      <GlyphIcon pattern={it.pattern} size={11} />
      <span className="truncate font-semibold text-foreground">
        {it.t.s}{it.t.group === "unknown" ? " ?" : ""}
      </span>
      <span className="text-right text-foreground">{fmtPct(it.head[2])}</span>
      <span className="text-right text-muted-foreground">{fmtRel(it.xPeak?.x ?? null)}</span>
      <span className="text-right text-muted-foreground">{fmtMoney(it.dollars)}</span>
      <span className="text-right text-muted-foreground">
        {rot == null ? "—" : `${rot.toFixed(rot >= 100 ? 0 : 1)} %`}
      </span>
    </button>
  );
}

export function AttentionApp() {
  const prefs = useAttentionPrefs();
  const [reply, setReply] = useState<MapReply | null>(null);
  const [map, setMap] = useState<MapView | null>(null);
  const [opts, setOpts] = useState<AttentionOptions | null>(null);
  const [settings, setSettings] = useState(false);
  const [sent, setSent] = useState<string | null>(null);
  const keyRef = useRef<string | null>(null);

  useEffect(() => {
    document.title = "TagDash — Attention du marché";
    attentionApi.getOptions().then(setOpts).catch(() => {});
  }, []);

  const followReview = prefs.followReview;
  const refresh = useCallback(async () => {
    try {
      const r = await attentionApi.getMap(keyRef.current, followReview);
      setReply(r);
      if (r.map) {
        keyRef.current = r.key;
        setMap(r.map);
      } else if (r.key !== keyRef.current) {
        // Nothing to draw for this key (engine not ready, day never recorded).
        keyRef.current = r.key;
        setMap(null);
      }
    } catch {
      /* the backend is restarting: keep what is on screen */
    }
  }, [followReview]);

  useEffect(() => {
    keyRef.current = null;
    refresh();
    const id = window.setInterval(refresh, POLL_MS);
    return () => window.clearInterval(id);
  }, [refresh]);

  // « Revue » moved in the main window: follow at once, and open its ticker.
  const selectTicker = prefs.select;
  useEffect(() => {
    let off: (() => void) | undefined;
    listen<{ symbol: string | null } | null>(ATTENTION_REVIEW_EVENT, (e) => {
      if (e.payload?.symbol && useAttentionPrefs.getState().followReview) {
        selectTicker(e.payload.symbol);
      }
      refresh();
    }).then((u) => { off = u; }).catch(() => {});
    return () => off?.();
  }, [refresh, selectTicker]);

  const items = useMemo(
    () => (map ? visible(map, prefs.universe, prefs.includeUnknown, prefs.showAh) : []),
    [map, prefs.universe, prefs.includeUnknown, prefs.showAh],
  );

  // The selection survives a filter change: its detail is read from the whole
  // map, even when the current universe no longer lists it.
  const selected = useMemo(() => {
    if (!map || !prefs.selected) return null;
    const inList = items.find((it) => it.t.s === prefs.selected);
    if (inList) return inList;
    const t = map.tickers.find((x) => x.s === prefs.selected);
    return t ? shown(t, map.frame, prefs.showAh) : null;
  }, [map, items, prefs.selected, prefs.showAh]);

  const send = useCallback((symbol: string) => {
    attentionApi.propagate(symbol).catch(() => {});
    setSent(symbol);
    window.setTimeout(() => setSent((s) => (s === symbol ? null : s)), 1_600);
  }, []);

  const pick = useCallback(
    (symbol: string) => {
      if (useAttentionPrefs.getState().selected === symbol) send(symbol);
      else selectTicker(symbol);
    },
    [send, selectTicker],
  );

  const unknown = map ? unknownCount(map) : 0;
  const st = reply?.status;
  const blocking =
    !map &&
    (reply?.review_missing
      ? "Carte non enregistrée pour ce jour : l'écran Attention n'était pas actif."
      : st?.state === "no_creds" || st?.state === "offline" || st?.state === "error"
        ? st.message
        : (st?.message ?? "Chargement…"));

  return (
    <div className="flex h-full w-full flex-col overflow-hidden bg-background text-foreground">
      {/* ── Filters: one row above the chart ── */}
      <div className="relative flex shrink-0 flex-wrap items-center gap-2 border-b border-border px-3 py-2">
        <Seg value={prefs.universe} options={UNIVERSES} onChange={prefs.setUniverse} />
        {prefs.universe !== "etf" && prefs.universe !== "all" && (
          <button
            type="button"
            onClick={prefs.toggleUnknown}
            title="Actions dont la capitalisation n'a jamais été collectée (beaucoup d'ADR). Toujours signalées par « ? »."
            className={cn(
              "rounded border border-dashed px-2 py-1 text-[11px] transition-colors",
              prefs.includeUnknown
                ? "border-foreground/60 text-foreground"
                : "border-border text-muted-foreground hover:text-foreground",
            )}
          >
            + cap. inconnue ({unknown})
          </button>
        )}
        <button
          type="button"
          onClick={prefs.toggleAh}
          title="Afficher / masquer l'after-hours de la séance précédente (carte et aperçu). La référence de prix ne change pas."
          className={cn(
            "rounded border px-2.5 py-1 text-[11px] font-medium transition-colors",
            prefs.showAh
              ? "border-foreground/60 bg-accent text-foreground"
              : "border-border text-muted-foreground hover:text-foreground",
          )}
        >
          AH {prefs.showAh ? "affiché" : "masqué"}
        </button>
        <Seg
          value={prefs.mode}
          options={[
            { id: "traj", label: "Trajectoires" },
            { id: "bubbles", label: "Bulles" },
          ]}
          onChange={prefs.setMode}
        />
        {reply?.review && (
          <button
            type="button"
            onClick={() => prefs.setFollowReview(!followReview)}
            title="Suivre le trade en cours de revue dans la fenêtre principale"
            className={cn(
              "rounded border px-2.5 py-1 text-[11px] transition-colors",
              followReview
                ? "border-foreground/60 bg-accent text-foreground"
                : "border-border text-muted-foreground hover:text-foreground",
            )}
          >
            {followReview ? "Suit la Revue" : "Revue ignorée"}
          </button>
        )}
        <div className="ml-auto flex min-w-0 items-center gap-2">
          <StatusLine reply={reply} map={map} />
          <button
            type="button"
            onClick={() => setSettings((v) => !v)}
            title="Seuils"
            className="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground"
          >
            <Settings className="h-4 w-4" />
          </button>
        </div>
        {settings && opts && (
          <SettingsPopover
            opts={opts}
            onSaved={(o) => { setOpts(o); keyRef.current = null; refresh(); }}
            onClose={() => setSettings(false)}
          />
        )}
      </div>

      {map && opts && (
        <div className="shrink-0 border-b border-border px-3 py-2">
          <WeatherBar map={map} opts={opts} />
        </div>
      )}

      <div className="flex min-h-0 flex-1">
        <div className="flex min-w-0 flex-1 flex-col">
          <div className="min-h-0 flex-1">
            {map && items.length > 0 ? (
              <TrajectoryMap
                map={map}
                items={items}
                mode={prefs.mode}
                maxTraces={prefs.maxTraces}
                selected={prefs.selected}
                onPick={pick}
              />
            ) : (
              <div className="flex h-full items-center justify-center px-6 text-center text-xs text-muted-foreground">
                {blocking ||
                  (prefs.showAh
                    ? "Aucun ticker ne franchit les seuils dans cet univers."
                    : "Aucun échange notable en pré-market pour l'instant dans cet univers.")}
              </div>
            )}
          </div>
          <div className="shrink-0 border-t border-border px-3 py-1.5">
            <Legend />
          </div>
        </div>

        {/* ── Side: detail of the selection, then the same tickers as a table ── */}
        <aside className="flex w-[400px] shrink-0 flex-col border-l border-border">
          {map && selected ? (
            <div className="shrink-0 border-b border-border p-3">
              <DetailPanel
                map={map}
                item={selected}
                showAh={prefs.showAh}
                followReview={followReview}
                mapKey={reply?.key ?? ""}
                onSend={() => send(selected.t.s)}
                sent={sent === selected.t.s}
              />
            </div>
          ) : (
            <div className="shrink-0 border-b border-border p-3 text-[11px] text-muted-foreground">
              Cliquez un ticker pour ouvrir son détail ; un second clic l'envoie au graphique de la
              fenêtre principale.
            </div>
          )}
          <div className="grid shrink-0 grid-cols-[14px_56px_1fr_1fr_1fr_52px] gap-1 border-b border-border px-2 py-1 text-[10px] uppercase tracking-wide text-muted-foreground/70">
            <span />
            <span>Ticker</span>
            <span className="text-right">Var.</span>
            <span className="text-right">Pic act.</span>
            <span className="text-right">Échangé</span>
            <span className="text-right">Rot.</span>
          </div>
          <div className="min-h-0 flex-1 overflow-y-auto">
            {items.map((it) => (
              <Row key={it.t.s} it={it} active={it.t.s === prefs.selected} onPick={() => pick(it.t.s)} />
            ))}
          </div>
          {map && (
            <div className="shrink-0 border-t border-border px-2 py-1 text-[10px] text-muted-foreground/70">
              {items.length} tickers affichés · {map.universe.toLocaleString("fr-FR")} suivis ·
              séance {map.frame.prev_day} → {map.frame.day}
              {map.frame.early_close ? " (clôture anticipée 13:00)" : ""}
            </div>
          )}
        </aside>
      </div>
    </div>
  );
}
