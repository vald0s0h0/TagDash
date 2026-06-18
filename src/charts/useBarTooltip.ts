import { useEffect, type MutableRefObject } from "react";
import type {
  IChartApi, ISeriesApi, MouseEventParams, CandlestickData, HistogramData, Time,
} from "lightweight-charts";

/** Discreet press-and-hold tooltip, on every pane by default. While the LEFT mouse
 *  button is held, it shows — for the bar under the crosshair — the bar's volume
 *  (just above the bar) and its body % move (close vs open, just above the volume;
 *  clamped to the top of the pane, still in the bar's x axis, when the bar sits too
 *  high to fit). Built on the official lightweight-charts crosshair-move API.
 *  Hidden the rest of the time, so it never clutters the chart. */
export function useBarTooltip(
  chartRef:        MutableRefObject<IChartApi | null>,
  candleRef:       MutableRefObject<ISeriesApi<"Candlestick"> | null>,
  volumeSeriesRef: MutableRefObject<ISeriesApi<"Histogram"> | null>,
  containerRef:    MutableRefObject<HTMLDivElement | null>,
) {
  useEffect(() => {
    const chart     = chartRef.current;
    const container = containerRef.current;
    if (!chart || !container) return;

    const mkLabel = (): HTMLDivElement => {
      const el = document.createElement("div");
      el.style.cssText =
        "position:absolute;z-index:25;pointer-events:none;display:none;" +
        "transform:translateX(-50%);white-space:nowrap;" +
        "font:10px ui-monospace,monospace;line-height:1.3;" +
        "padding:0 3px;border-radius:2px;background:rgba(8,8,8,0.72);";
      container.appendChild(el);
      return el;
    };
    const volEl = mkLabel();
    const pctEl = mkLabel();
    volEl.style.color = "#9aa0a6";

    let pressed = false;
    // Last hovered bar — so a press without moving still renders immediately.
    let last: { x: number; high: number; pct: number; vol: number | null } | null = null;

    const fmtVol = (v: number): string => {
      if (v >= 1e9) return (v / 1e9).toFixed(2) + "B";
      if (v >= 1e6) return (v / 1e6).toFixed(2) + "M";
      if (v >= 1e3) return (v / 1e3).toFixed(1) + "K";
      return String(Math.round(v));
    };

    const hide = () => { volEl.style.display = "none"; pctEl.style.display = "none"; };

    const render = () => {
      const candle = candleRef.current;
      if (!pressed || !last || !candle) { hide(); return; }
      const highY = candle.priceToCoordinate(last.high);
      if (highY == null) { hide(); return; }

      // Volume — just above the bar high (may scroll off the top with the bar).
      let stackTop = highY - 6;
      if (last.vol != null) {
        volEl.textContent   = fmtVol(last.vol);
        volEl.style.display = "block";
        volEl.style.left    = `${last.x}px`;
        volEl.style.top     = `${highY - 6 - volEl.offsetHeight}px`;
        stackTop = highY - 6 - volEl.offsetHeight;
      } else {
        volEl.style.display = "none";
      }

      // Percentage — just above the volume; clamped to the top of the pane (still in
      // the bar's x axis) when the bar is too high for the label to fit on screen.
      pctEl.textContent   = `${last.pct >= 0 ? "+" : ""}${last.pct.toFixed(2)}%`;
      pctEl.style.color   = last.pct >= 0 ? "#26a69a" : "#ef5350";
      pctEl.style.display = "block";
      pctEl.style.left    = `${last.x}px`;
      pctEl.style.top     = `${Math.max(2, stackTop - pctEl.offsetHeight - 2)}px`;
    };

    const onMove = (param: MouseEventParams) => {
      const candle = candleRef.current;
      if (!candle || param.time == null || !param.point) { last = null; hide(); return; }
      const ohlc = param.seriesData.get(candle) as CandlestickData | undefined;
      if (!ohlc || typeof ohlc.open !== "number") { last = null; hide(); return; }
      const x = chart.timeScale().timeToCoordinate(param.time as Time);
      if (x == null) { last = null; hide(); return; }
      const volSeries = volumeSeriesRef.current;
      const volData   = volSeries
        ? (param.seriesData.get(volSeries) as HistogramData | undefined)
        : undefined;
      const pct = ohlc.open !== 0 ? ((ohlc.close - ohlc.open) / ohlc.open) * 100 : 0;
      last = { x, high: ohlc.high, pct, vol: volData?.value ?? null };
      render();
    };
    chart.subscribeCrosshairMove(onMove);

    const onDown = (e: MouseEvent) => { if (e.button === 0) { pressed = true; render(); } };
    const onUp   = () => { pressed = false; hide(); };
    container.addEventListener("mousedown", onDown);
    window.addEventListener("mouseup", onUp);

    return () => {
      chart.unsubscribeCrosshairMove(onMove);
      container.removeEventListener("mousedown", onDown);
      window.removeEventListener("mouseup", onUp);
      volEl.remove();
      pctEl.remove();
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps
}
