import { useEffect, useMemo, useRef, useState } from "react";

export interface Series {
  label: string;
  /** CSS colour (a theme token). */
  color: string;
  /** One value per x; null breaks the line (and with `missMarks`, marks the x). */
  values: (number | null)[];
}

const HEIGHT = 180;
const PAD = { top: 10, right: 12, bottom: 22, left: 52 };

function niceMax(v: number): number {
  if (v <= 0) return 1;
  const p = 10 ** Math.floor(Math.log10(v));
  for (const m of [1, 2, 2.5, 5, 10]) if (m * p >= v) return m * p;
  return 10 * p;
}

/**
 * Line chart over a shared x (probe number or second), one y axis, zero
 * based. Missing values break the line; with `missMarks` they are marked
 * with a × in the status colour at the bottom (labelled in the legend).
 * Hover shows a crosshair and the values at that x.
 */
export function SeriesChart({
  series,
  formatY,
  formatX,
  xLabel,
  missMarks,
  missLabel = "no reply",
  window: windowSize,
}: {
  series: Series[];
  formatY: (v: number) => string;
  formatX: (i: number) => string;
  xLabel: string;
  missMarks?: boolean;
  missLabel?: string;
  /** Show only the last N points (long continuous runs). */
  window?: number;
}) {
  const box = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(600);
  const [hover, setHover] = useState<number | null>(null);

  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const ro = new ResizeObserver(([e]) => {
      if (e) setWidth(Math.max(240, Math.floor(e.contentRect.width)));
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const total = Math.max(0, ...series.map((s) => s.values.length));
  const first = windowSize && total > windowSize ? total - windowSize : 0;
  const count = total - first;
  const yMax = useMemo(() => {
    let m = 0;
    for (const s of series) for (let i = first; i < s.values.length; i++) m = Math.max(m, s.values[i] ?? 0);
    return niceMax(m * 1.08);
  }, [series, first]);

  const plotW = width - PAD.left - PAD.right;
  const plotH = HEIGHT - PAD.top - PAD.bottom;
  const x = (i: number) => PAD.left + (count <= 1 ? plotW / 2 : ((i - first) / (count - 1)) * plotW);
  const y = (v: number) => PAD.top + plotH - (v / yMax) * plotH;

  const paths = series.map((s) => {
    const color = s.color;
    let d = "";
    let pen = false;
    for (let i = first; i < s.values.length; i++) {
      const v = s.values[i];
      if (v == null) {
        pen = false;
        continue;
      }
      d += `${pen ? "L" : "M"}${x(i).toFixed(1)},${y(v).toFixed(1)}`;
      pen = true;
    }
    return { d, color, label: s.label };
  });
  // Lone points between gaps would be invisible as a path: draw dots for them.
  const lone = series.map((s) => ({
    color: s.color,
    points: s.values.flatMap((v, i) =>
      i >= first && v != null && (i === first || s.values[i - 1] == null) && s.values[i + 1] == null ? [{ v, i }] : [],
    ),
  }));
  const misses =
    missMarks && series[0]
      ? series[0].values.map((v, i) => (v == null && i >= first ? i : -1)).filter((i) => i >= 0)
      : [];

  const ticks = [0, 0.25, 0.5, 0.75, 1].map((f) => f * yMax);
  const xTickCount = Math.min(count, Math.max(2, Math.floor(plotW / 90)));
  const xTicks =
    count === 0
      ? []
      : Array.from({ length: xTickCount }, (_, k) =>
          Math.round(first + (xTickCount === 1 ? 0 : (k / (xTickCount - 1)) * (count - 1))),
        );

  const onMove = (e: React.PointerEvent<SVGRectElement>) => {
    if (count === 0) return;
    const rect = (e.currentTarget as SVGRectElement).getBoundingClientRect();
    const f = (e.clientX - rect.left) / rect.width;
    setHover(Math.min(total - 1, Math.max(first, Math.round(first + f * (count - 1)))));
  };

  const tipLeft = hover != null ? Math.min(width - 170, Math.max(0, x(hover) + 10)) : 0;

  return (
    <div className="series-chart" ref={box}>
      {(series.length > 1 || misses.length > 0) && (
        <div className="viz-legend">
          {series.length > 1 &&
            series.map((s) => (
              <span key={s.label} className="legend-item">
                <span className="series-swatch" style={{ background: s.color }} />
                {s.label}
              </span>
            ))}
          {misses.length > 0 && (
            <span className="legend-item">
              <span className="miss-glyph">×</span>
              {missLabel} ({misses.length})
            </span>
          )}
        </div>
      )}
      <svg width={width} height={HEIGHT} role="img" aria-label={`${series.map((s) => s.label).join(", ")} by ${xLabel}`}>
        {ticks.map((t) => (
          <g key={t}>
            <line className="viz-grid" x1={PAD.left} x2={width - PAD.right} y1={y(t)} y2={y(t)} />
            <text className="viz-tick" x={PAD.left - 6} y={y(t) + 4} textAnchor="end">
              {formatY(t)}
            </text>
          </g>
        ))}
        {xTicks.map((i) => (
          <text key={i} className="viz-tick" x={x(i)} y={HEIGHT - 6} textAnchor="middle">
            {formatX(i)}
          </text>
        ))}
        {misses.map((i) => (
          <text key={`m${i}`} className="miss-mark" x={x(i)} y={PAD.top + plotH - 2} textAnchor="middle">
            ×
          </text>
        ))}
        {paths.map((p) => (
          <path key={p.label} d={p.d} fill="none" stroke={p.color} strokeWidth={2} strokeLinejoin="round" />
        ))}
        {lone.map(({ color, points }, k) =>
          points.map(({ v, i }) => <circle key={`${k}-${i}`} cx={x(i)} cy={y(v)} r={2.5} fill={color} />),
        )}
        {hover != null && (
          <g>
            <line className="viz-crosshair" x1={x(hover)} x2={x(hover)} y1={PAD.top} y2={PAD.top + plotH} />
            {series.map((s) => {
              const v = s.values[hover];
              return v == null ? null : (
                <circle key={s.label} cx={x(hover)} cy={y(v)} r={4} fill={s.color} stroke="var(--bg-elev)" strokeWidth={2} />
              );
            })}
          </g>
        )}
        <rect
          x={PAD.left}
          y={PAD.top}
          width={Math.max(0, plotW)}
          height={plotH}
          fill="transparent"
          onPointerMove={onMove}
          onPointerLeave={() => setHover(null)}
        />
      </svg>
      {hover != null && (
        <div className="viz-tooltip series-tooltip" style={{ left: tipLeft }}>
          <div className="viz-tooltip-head">
            {xLabel} {formatX(hover)}
          </div>
          {series.map((s) => {
            const v = s.values[hover];
            return (
              <div key={s.label} className="viz-tooltip-row">
                <span className="series-swatch" style={{ background: s.color }} />
                <span className="tip-label">{s.label}</span>
                <strong>{v == null ? missLabel : formatY(v)}</strong>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
