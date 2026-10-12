// Screenshots of the reviewed trade — the images saved in its TradeTally gallery
// (a missed trade: the local captures). A button, greyed when there is none, with
// the count when there are several; it opens them in a modal, one at a time, with
// a tab per image (a trade has one, sometimes two, rarely three).

import { useEffect, useState } from "react";
import { ChevronLeft, ChevronRight, Image as ImageIcon, LoaderCircle, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { reviewImage, type ReviewImageSrc } from "./queue";

function mime(path: string): string {
  const ext = path.split(".").pop()?.toLowerCase();
  return ext === "webp" ? "image/webp" : ext === "jpg" || ext === "jpeg" ? "image/jpeg" : "image/png";
}

export function TradeImagesButton({ sources, title }: { sources: ReviewImageSrc[]; title: string }) {
  const [open, setOpen] = useState(false);
  const n = sources.length;
  return (
    <>
      <button
        type="button"
        disabled={n === 0}
        onClick={() => setOpen(true)}
        title={n === 0 ? "Aucune capture enregistrée" : `${n} capture${n > 1 ? "s" : ""} — ${title}`}
        className={cn(
          "relative flex h-7 w-7 shrink-0 items-center justify-center rounded-md border transition-colors",
          n === 0
            ? "cursor-not-allowed border-border text-muted-foreground/30"
            : "border-border text-foreground hover:border-sky-600 hover:text-sky-300",
        )}
      >
        <ImageIcon className="h-4 w-4" />
        {n > 1 && (
          <span className="absolute -right-1.5 -top-1.5 rounded-full bg-sky-600 px-1 text-[9px] font-semibold leading-[14px] text-white">
            {n}
          </span>
        )}
      </button>
      {open && n > 0 && <ImagesModal sources={sources} title={title} onClose={() => setOpen(false)} />}
    </>
  );
}

function ImagesModal({ sources, title, onClose }: { sources: ReviewImageSrc[]; title: string; onClose: () => void }) {
  const [index, setIndex] = useState(0);
  // Object URL, null while loading, or the error.
  const [urls, setUrls] = useState<(string | null | { error: string })[]>(() => sources.map(() => null));

  useEffect(() => {
    let cancelled = false;
    const made: string[] = [];
    sources.forEach((src, i) => {
      reviewImage(src).then(
        (buf) => {
          if (cancelled) return;
          const u = URL.createObjectURL(new Blob([buf], { type: mime(src.path) }));
          made.push(u);
          setUrls((prev) => prev.map((x, k) => (k === i ? u : x)));
        },
        (e) => !cancelled && setUrls((prev) => prev.map((x, k) => (k === i ? { error: String(e) } : x))),
      );
    });
    return () => {
      cancelled = true;
      made.forEach((u) => URL.revokeObjectURL(u));
    };
  }, [sources]);

  // Esc closes, ←/→ change image — caught before the replay's own keys.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
      else if (e.key === "ArrowRight") setIndex((i) => Math.min(sources.length - 1, i + 1));
      else if (e.key === "ArrowLeft") setIndex((i) => Math.max(0, i - 1));
      else if (e.code !== "Space") return;
      e.preventDefault();
      e.stopPropagation();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose, sources.length]);

  const cur = urls[index];
  return (
    <div data-review-modal className="fixed inset-0 z-[3000] flex flex-col bg-black/85 p-6" onClick={onClose}>
      <div className="mb-3 flex items-center gap-2" onClick={(e) => e.stopPropagation()}>
        <span className="text-sm font-medium text-white">{title}</span>
        {sources.length > 1 &&
          sources.map((_, i) => (
            <button
              key={i}
              type="button"
              onClick={() => setIndex(i)}
              className={cn(
                "h-6 min-w-6 rounded px-2 text-xs",
                i === index ? "bg-sky-600 text-white" : "bg-white/10 text-white/70 hover:bg-white/20",
              )}
            >
              {i + 1}
            </button>
          ))}
        <button type="button" onClick={onClose} className="ml-auto text-white/80 hover:text-white" title="Fermer (Échap)">
          <X className="h-5 w-5" />
        </button>
      </div>
      <div className="relative flex min-h-0 flex-1 items-center justify-center">
        {cur === null ? (
          <LoaderCircle className="h-6 w-6 animate-spin text-white/70" />
        ) : typeof cur === "object" ? (
          <span className="max-w-lg text-center text-sm text-red-300">Image indisponible : {cur.error}</span>
        ) : (
          <img src={cur} alt="" className="max-h-full max-w-full rounded" onClick={(e) => e.stopPropagation()} />
        )}
        {sources.length > 1 && (
          <>
            <NavBtn side="left" disabled={index === 0} onClick={() => setIndex(index - 1)} />
            <NavBtn side="right" disabled={index === sources.length - 1} onClick={() => setIndex(index + 1)} />
          </>
        )}
      </div>
    </div>
  );
}

function NavBtn({ side, disabled, onClick }: { side: "left" | "right"; disabled: boolean; onClick: () => void }) {
  const Icon = side === "left" ? ChevronLeft : ChevronRight;
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={(e) => {
        e.stopPropagation();
        onClick();
      }}
      className={cn(
        "absolute top-1/2 -translate-y-1/2 rounded-full bg-black/60 p-2 text-white disabled:opacity-20",
        side === "left" ? "left-2" : "right-2",
      )}
    >
      <Icon className="h-6 w-6" />
    </button>
  );
}
