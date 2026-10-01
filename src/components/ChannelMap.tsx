import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type PointerEvent } from "react";
import type { AccessPointObservation, Band } from "../types/wifi";
import { usePreferences, type SignalUnit } from "../state/Preferences";
import { formatAge } from "../lib/format";

// ---------------------------------------------------------------------------
// Band geometry
// ---------------------------------------------------------------------------

interface BandSpec {
  band: Band;
  label: string;
  minMhz: number;
  maxMhz: number;
  /** 20 MHz channel centres: [channel, MHz] */
  channels: [number, number][];
}

const range = (from: number, to: number, step: number) =>
  Array.from({ length: Math.floor((to - from) / step) + 1 }, (_, i) => from + i * step);

const BANDS: BandSpec[] = [
  {
    band: "2.4ghz",
    label: "2.4 GHz",
    minMhz: 2400,
    maxMhz: 2496,
    channels: [...range(1, 13, 1).map((c): [number, number] => [c, 2407 + c * 5]), [14, 2484]],
  },
  {
    band: "5ghz",
    label: "5 GHz",
    minMhz: 5160,
    maxMhz: 5895,
    channels: [...range(36, 64, 4), ...range(100, 144, 4), ...range(149, 177, 4)].map((c) => [c, 5000 + c * 5]),
  },
  {
    band: "6ghz",
    label: "6 GHz",
    minMhz: 5945,
    maxMhz: 7125,
    channels: range(1, 233, 4).map((c) => [c, 5950 + c * 5]),
  },
];

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

type Role = "connected" | "highlighted" | "other";

interface Shape {
  /** Representative BSSID (the strongest when several share one radio). */
  ap: AccessPointObservation;
  /** Every BSSID drawn by this shape; more than one when same-radio merging is on. */
  members: AccessPointObservation[];
  value: number;
  lo: number;
  hi: number;
  /** false when the centre had to be assumed (primary channel) */
  centreKnown: boolean;
  /** Not heard recently — NetworkManager's remembered value, not a current reading. */
  stale: boolean;
  role: Role;
}

function valueFor(ap: AccessPointObservation, unit: SignalUnit): number | null {
  // One axis, one unit: never mix dBm and % in the same plot.
  return unit === "dbm" ? ap.signal.dbm : ap.signal.qualityPercent;
}

const Y_DOMAIN: Record<SignalUnit, [number, number]> = { dbm: [-95, -25], percent: [0, 100] };
const Y_TICKS: Record<SignalUnit, number[]> = {
  dbm: [-90, -80, -70, -60, -50, -40, -30],
  percent: [0, 20, 40, 60, 80, 100],
};
/** Common design target for voice/video coverage. */
const REFERENCE_DBM = -67;

/** Same threshold as the table's dimmed rows. */
const STALE_MS = 30_000;

const MERGE_KEY = "fresnel.channelMap.mergeRadios";

function loadMerge(): boolean {
  try {
    return localStorage.getItem(MERGE_KEY) !== "0";
  } catch {
    return true;
  }
}

/**
 * Heuristic key for "same physical radio": one radio advertising several SSIDs
 * derives the BSSIDs from one base MAC, usually varying only the first octet
 * (locally-administered variants) or the last. Same channel, width and centre
 * plus identical octets 2–5 is treated as one radio.
 */
function radioKey(ap: AccessPointObservation): string {
  const o = ap.bssid.toLowerCase().split(":");
  return `${ap.frequencyMhz}|${ap.channelWidthMhz ?? 20}|${ap.channelCenterMhz ?? ""}|${o.slice(1, 5).join(":")}`;
}

function mergeRadios(shapes: Shape[]): Shape[] {
  const groups = new Map<string, Shape[]>();
  for (const s of shapes) {
    const k = radioKey(s.ap);
    const g = groups.get(k);
    if (g) g.push(s);
    else groups.set(k, [s]);
  }
  return [...groups.values()].map((g) => {
    if (g.length === 1) return g[0]!;
    g.sort((a, b) => rank(b.role) - rank(a.role) || b.value - a.value);
    const lead = g[0]!;
    return {
      ...lead,
      value: Math.max(...g.map((s) => s.value)),
      members: g.map((s) => s.ap),
      stale: g.every((s) => s.stale),
    };
  });
}

function shapeName(s: Shape): string {
  const names = [...new Set(s.members.map((m) => m.ssid ?? "hidden"))];
  return names.join(" + ");
}

const PAD = { left: 40, right: 12, top: 22, bottom: 26 };
const PLOT_HEIGHT = 190;

function useWidth<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  const [width, setWidth] = useState(0);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(([entry]) => setWidth(Math.floor(entry!.contentRect.width)));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  return [ref, width] as const;
}

// ---------------------------------------------------------------------------
// One band
// ---------------------------------------------------------------------------

function BandChart({
  spec,
  shapes,
  omitted,
  unit,
  merged,
  onPick,
}: {
  spec: BandSpec;
  shapes: Shape[];
  omitted: number;
  unit: SignalUnit;
  merged: boolean;
  onPick: (ssid: string | null) => void;
}) {
  const noun = merged ? "radio" : "BSSID";
  const [ref, width] = useWidth<HTMLDivElement>();
  const [cursor, setCursor] = useState<number | null>(null); // index into spec.channels
  const [pointerY, setPointerY] = useState<number | null>(null);

  const plotW = Math.max(0, width - PAD.left - PAD.right);
  const height = PAD.top + PLOT_HEIGHT + PAD.bottom;
  const [y0, y1] = Y_DOMAIN[unit];
  const x = (mhz: number) => PAD.left + ((mhz - spec.minMhz) / (spec.maxMhz - spec.minMhz)) * plotW;
  const y = (v: number) => PAD.top + PLOT_HEIGHT - ((Math.min(Math.max(v, y0), y1) - y0) / (y1 - y0)) * PLOT_HEIGHT;
  const baseY = PAD.top + PLOT_HEIGHT;

  // Label every k-th channel so labels keep ≥ 24 px apart.
  const chPx = spec.channels.length > 1 ? x(spec.channels[1]![1]) - x(spec.channels[0]![1]) : plotW;
  const labelEvery = [1, 2, 4, 8].find((k) => chPx * k >= 24) ?? 8;

  const cursorMhz = cursor != null ? spec.channels[cursor]![1] : null;
  const covering = useMemo(
    () =>
      cursorMhz == null
        ? []
        : shapes.filter((s) => s.lo < cursorMhz && s.hi > cursorMhz).sort((a, b) => b.value - a.value),
    [shapes, cursorMhz],
  );

  const nearestChannel = (px: number) => {
    let best = 0;
    let bestD = Infinity;
    spec.channels.forEach(([, mhz], i) => {
      const d = Math.abs(x(mhz) - px);
      if (d < bestD) {
        bestD = d;
        best = i;
      }
    });
    return best;
  };

  const onMove = (e: PointerEvent<SVGSVGElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    setCursor(nearestChannel(e.clientX - r.left));
    setPointerY(e.clientY - r.top);
  };

  const onKey = (e: KeyboardEvent<SVGSVGElement>) => {
    if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
      e.preventDefault();
      const d = e.key === "ArrowRight" ? 1 : -1;
      setCursor((c) => Math.min(spec.channels.length - 1, Math.max(0, (c ?? -d) + d)));
      setPointerY(null);
    } else if (e.key === "Escape") {
      setCursor(null);
    } else if (e.key === "Enter" && covering[0]) {
      onPick(covering[0].ap.ssid);
    }
  };

  // Draw order: others underneath, emphasis on top.
  const ordered = [...shapes].sort((a, b) => rank(a.role) - rank(b.role) || a.value - b.value);
  const path = (s: Shape) => {
    const edge = Math.min(3, (s.hi - s.lo) * 0.12);
    const top = y(s.value);
    return `M${x(s.lo)},${baseY} L${x(s.lo + edge)},${top} L${x(s.hi - edge)},${top} L${x(s.hi)},${baseY}`;
  };

  const tooltipLeft = cursorMhz != null ? x(cursorMhz) : 0;
  const flip = tooltipLeft > width * 0.6;

  return (
    <section className="card channel-card">
      <header className="card-header">
        <h2>
          {spec.label}{" "}
          <span className="count" title={merged ? "Radios (BSSIDs on one radio merged)" : "BSSIDs"}>
            {shapes.length}
          </span>
        </h2>
        {omitted > 0 && (
          <span className="muted small" title="These BSSIDs have no reading in the selected unit">
            {omitted} without {unit === "dbm" ? "dBm" : "%"} not shown
          </span>
        )}
      </header>
      <div className="channel-plot" ref={ref} style={{ height }}>
        {width > 0 && (
          <svg
            width={width}
            height={height}
            role="img"
            aria-label={`${spec.label} channel occupancy: ${shapes.length} ${noun}s`}
            tabIndex={0}
            onPointerMove={onMove}
            onPointerLeave={() => setCursor(null)}
            onClick={() => covering[0] && onPick(covering[0].ap.ssid)}
            onKeyDown={onKey}
            onBlur={() => setCursor(null)}
          >
            {/* Grid + y axis */}
            {Y_TICKS[unit].map((t) => (
              <g key={t}>
                <line className="viz-grid" x1={PAD.left} x2={width - PAD.right} y1={y(t)} y2={y(t)} />
                <text className="viz-tick" x={PAD.left - 6} y={y(t)} dy="0.32em" textAnchor="end">
                  {t}
                </text>
              </g>
            ))}
            <text className="viz-tick" x={PAD.left - 6} y={PAD.top - 10} textAnchor="end">
              {unit === "dbm" ? "dBm" : "%"}
            </text>
            {unit === "dbm" && (
              <g>
                <line className="viz-ref" x1={PAD.left} x2={width - PAD.right} y1={y(REFERENCE_DBM)} y2={y(REFERENCE_DBM)} />
                <text className="viz-tick" x={width - PAD.right} y={y(REFERENCE_DBM) - 4} textAnchor="end">
                  {REFERENCE_DBM}
                </text>
              </g>
            )}

            {/* x axis: channel numbers */}
            <line className="viz-axis" x1={PAD.left} x2={width - PAD.right} y1={baseY} y2={baseY} />
            {spec.channels.map(([ch, mhz], i) => (
              <g key={ch}>
                <line className="viz-axis" x1={x(mhz)} x2={x(mhz)} y1={baseY} y2={baseY + 3} />
                {i % labelEvery === 0 && (
                  <text className="viz-tick" x={x(mhz)} y={baseY + 15} textAnchor="middle">
                    {ch}
                  </text>
                )}
              </g>
            ))}

            {/* Crosshair */}
            {cursorMhz != null && (
              <line className="viz-crosshair" x1={x(cursorMhz)} x2={x(cursorMhz)} y1={PAD.top} y2={baseY} />
            )}

            {/* BSSIDs */}
            {ordered.map((s) => {
              const dim = cursorMhz != null && !(s.lo < cursorMhz && s.hi > cursorMhz);
              return (
                <path
                  key={s.ap.bssid}
                  d={path(s)}
                  className={`viz-shape viz-${s.role} ${dim ? "viz-dim" : ""} ${s.stale ? "viz-stale" : ""} ${s.centreKnown ? "" : "viz-approx"}`}
                />
              );
            })}

            {/* Primary-channel markers + selective direct labels (emphasised only) */}
            {ordered
              .filter((s) => s.role !== "other")
              .map((s) => (
                <circle
                  key={`m-${s.ap.bssid}`}
                  className={`viz-marker viz-${s.role}`}
                  cx={x(s.ap.frequencyMhz)}
                  cy={y(s.value)}
                  r={4}
                />
              ))}
            {labelsFor(ordered).map((s) => {
              const cx = Math.min(Math.max(x((s.lo + s.hi) / 2), PAD.left + 40), width - PAD.right - 40);
              return (
                <text key={`l-${s.ap.bssid}`} className="viz-label" x={cx} y={y(s.value) - 8} textAnchor="middle">
                  {shapeName(s)}
                </text>
              );
            })}

            {shapes.length === 0 && (
              <text className="viz-empty" x={PAD.left + plotW / 2} y={PAD.top + PLOT_HEIGHT / 2} textAnchor="middle">
                No networks seen on {spec.label}
              </text>
            )}
          </svg>
        )}

        {cursorMhz != null && covering.length > 0 && (
          <div
            className="viz-tooltip"
            style={{
              left: flip ? undefined : tooltipLeft + 12,
              right: flip ? width - tooltipLeft + 12 : undefined,
              top: Math.min(Math.max((pointerY ?? PAD.top) - 20, 0), PAD.top + 40),
            }}
          >
            <div className="viz-tooltip-head">
              Ch {spec.channels[cursor!]![0]} · {cursorMhz} MHz · {covering.length} {noun}
              {covering.length > 1 ? "s" : ""}
            </div>
            {covering.slice(0, 8).map((s) => (
              <div key={s.ap.bssid} className="viz-tooltip-row">
                <span className={`viz-key viz-${s.role}`} />
                <strong>
                  {Math.round(s.value)}
                  <span className="unit">{unit === "dbm" ? " dBm" : "%"}</span>
                </strong>
                <span className="viz-tooltip-ssid">{shapeName(s)}</span>
                <span className="muted mono">
                  ch {s.ap.channel ?? "?"} · {s.ap.channelWidthMhz ?? 20} MHz{s.centreKnown ? "" : " (approx.)"}
                  {s.members.length > 1 && ` · ${s.members.length} BSSIDs`}
                  {s.stale && ` · seen ${formatAge(s.ap.lastSeenAgeMs)} ago`}
                </span>
              </div>
            ))}
            {covering.length > 8 && <div className="muted">+{covering.length - 8} more</div>}
          </div>
        )}
      </div>
    </section>
  );
}

function rank(r: Role) {
  return r === "other" ? 0 : r === "highlighted" ? 1 : 2;
}

/** Direct-label only emphasised networks, one label per SSID per band (the strongest). */
function labelsFor(shapes: Shape[]): Shape[] {
  const best = new Map<string, Shape>();
  for (const s of shapes) {
    if (s.role === "other") continue;
    const k = shapeName(s);
    const cur = best.get(k);
    if (!cur || s.value > cur.value) best.set(k, s);
  }
  return [...best.values()];
}

// ---------------------------------------------------------------------------
// Public component
// ---------------------------------------------------------------------------

export function ChannelMap({
  accessPoints,
  connectedSsid,
  highlightSsid,
  onHighlight,
}: {
  accessPoints: AccessPointObservation[];
  connectedSsid: string | null;
  highlightSsid: string | null;
  onHighlight: (ssid: string | null) => void;
}) {
  const { signalUnit } = usePreferences();
  const [merge, setMergeState] = useState(loadMerge);
  const setMerge = (on: boolean) => {
    setMergeState(on);
    try {
      localStorage.setItem(MERGE_KEY, on ? "1" : "0");
    } catch {
      /* non-essential */
    }
  };

  const perBand = useMemo(() => {
    const out = new Map<Band, { shapes: Shape[]; omitted: number }>();
    for (const spec of BANDS) out.set(spec.band, { shapes: [], omitted: 0 });
    for (const ap of accessPoints) {
      const slot = out.get(ap.band);
      if (!slot) continue;
      const value = valueFor(ap, signalUnit);
      if (value == null) {
        slot.omitted++;
        continue;
      }
      const width = ap.channelWidthMhz ?? 20;
      const centre = ap.channelCenterMhz ?? ap.frequencyMhz;
      const role: Role =
        connectedSsid != null && ap.ssid === connectedSsid
          ? "connected"
          : highlightSsid != null && ap.ssid === highlightSsid
            ? "highlighted"
            : "other";
      slot.shapes.push({
        ap,
        members: [ap],
        value,
        lo: centre - width / 2,
        hi: centre + width / 2,
        centreKnown: ap.channelCenterMhz != null,
        stale: ap.lastSeenAgeMs != null && ap.lastSeenAgeMs > STALE_MS,
        role,
      });
    }
    if (merge) for (const slot of out.values()) slot.shapes = mergeRadios(slot.shapes);
    return out;
  }, [accessPoints, signalUnit, connectedSsid, highlightSsid, merge]);

  const visibleBands = BANDS.filter((b) => {
    const d = perBand.get(b.band)!;
    return b.band !== "6ghz" || d.shapes.length + d.omitted > 0;
  });

  const pick = (ssid: string | null) => {
    if (ssid == null || ssid === connectedSsid) return;
    onHighlight(ssid === highlightSsid ? null : ssid);
  };

  return (
    <div className="channel-map">
      <div className="viz-legend">
        <span className="legend-title">Channel occupancy</span>
        {connectedSsid && (
          <span className="legend-item">
            <span className="viz-key viz-connected" /> Connected: {connectedSsid}
          </span>
        )}
        {highlightSsid && (
          <span className="legend-item">
            <span className="viz-key viz-highlighted" /> Highlighted: {highlightSsid}
            <button type="button" className="legend-clear" onClick={() => onHighlight(null)} title="Clear highlight">
              ×
            </button>
          </span>
        )}
        <span className="legend-item">
          <span className="viz-key viz-other" /> Other networks
        </span>
        <label
          className="checkbox small"
          title="Draw BSSIDs that share one radio (same channel, BSSIDs differing only in the first or last octet) as a single shape"
        >
          <input type="checkbox" checked={merge} onChange={(e) => setMerge(e.target.checked)} />
          Merge SSIDs on one radio
        </label>
        <span className="muted small legend-hint">
          Height = signal, width = occupied channel. Click a network here or in the table to highlight it.
        </span>
      </div>
      <div className={`channel-grid bands-${visibleBands.length}`}>
        {visibleBands.map((spec) => {
          const d = perBand.get(spec.band)!;
          return (
            <BandChart
              key={spec.band}
              spec={spec}
              shapes={d.shapes}
              omitted={d.omitted}
              unit={signalUnit}
              merged={merge}
              onPick={pick}
            />
          );
        })}
      </div>
    </div>
  );
}
