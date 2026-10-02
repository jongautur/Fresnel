import type { ReactNode } from "react";
import type { Band } from "../../types/wifi";
import type { PlacedAp } from "../../types/survey";
import { BAND_LABEL } from "../../lib/format";
import { NumberInput } from "../NumberInput";
import {
  OVERLAP_COLORS,
  SIGNAL_DOMAIN,
  SIGNAL_RAMP,
  STATUS_CRITICAL,
  STATUS_GOOD,
  NO_SERVICE_COLOR,
  type HeatMetric,
  type NetworkFilter,
  floorNetworks,
} from "../../lib/heatmap";

export const METRICS: { id: HeatMetric; label: string; title: string }[] = [
  { id: "signal", label: "Signal", title: "Signal strength of the chosen network (strongest BSSID at each spot)" },
  { id: "coverage", label: "Coverage", title: "Where the signal meets a target level" },
  { id: "overlap", label: "AP overlap", title: "How many APs are heard above a level: 2+ helps roaming" },
  { id: "serving", label: "Serving AP", title: "Which placed access point is strongest at each spot" },
  { id: "requirements", label: "Requirements", title: "Where the floor's requirement profile is met" },
];

export const BANDS: (Band | "all")[] = ["all", "2.4ghz", "5ghz", "6ghz"];

/** Range of the coverage / overlap / serving threshold, dBm. */
export const THRESHOLD_MIN = -95;
export const THRESHOLD_MAX = -30;

export function encodeNetwork(n: NetworkFilter): string {
  switch (n.kind) {
    case "any":
      return "any";
    case "ssid":
      return `ssid:${n.ssid}`;
    case "bssid":
      return `bssid:${n.bssid}`;
    case "ap":
      return `ap:${n.apId}`;
  }
}

function decodeNetwork(v: string, aps: PlacedAp[]): NetworkFilter {
  if (v.startsWith("ssid:")) return { kind: "ssid", ssid: v.slice(5) };
  if (v.startsWith("bssid:")) return { kind: "bssid", bssid: v.slice(6) };
  if (v.startsWith("ap:")) {
    const ap = aps.find((a) => a.id === Number(v.slice(3)));
    if (ap) return { kind: "ap", apId: ap.id, bssids: ap.bssids };
  }
  return { kind: "any" };
}

export interface ColoredAp {
  ap: PlacedAp;
  color: string;
}

export function HeatmapControls({
  metric,
  setMetric,
  network,
  setNetwork,
  band,
  setBand,
  threshold,
  setThreshold,
  networks,
  passingFraction,
  usedPoints,
  excludedPoints,
  scaled,
  aps,
  servingShares,
  apNameByBssid,
  requirementsLegend,
}: {
  metric: HeatMetric;
  setMetric: (m: HeatMetric) => void;
  network: NetworkFilter;
  setNetwork: (n: NetworkFilter) => void;
  band: Band | "all";
  setBand: (b: Band | "all") => void;
  threshold: number;
  setThreshold: (t: number) => void;
  networks: ReturnType<typeof floorNetworks>;
  passingFraction: number | null;
  usedPoints: number;
  excludedPoints: number;
  scaled: boolean;
  /** Placed APs in the building, with their colours. */
  aps: ColoredAp[];
  /** Serving view: share of mapped area per AP id, and the unserved share. */
  servingShares: { shares: Map<number, number>; unserved: number } | null;
  apNameByBssid: Map<string, string>;
  /** Legend for the Requirements view; null when no profile applies (view disabled). */
  requirementsLegend: ReactNode | null;
}) {
  const servable = aps.filter((a) => a.ap.bssids.length > 0);
  const [lo, hi] = SIGNAL_DOMAIN;
  const pos = (dbm: number) => `${((dbm - lo) / (hi - lo)) * 100}%`;
  const pct = passingFraction != null ? `${Math.round(passingFraction * 100)} %` : "—";

  return (
    <section className="card">
      <header className="card-header">
        <h2>Heatmap</h2>
        <span className="muted small">
          {usedPoints} point{usedPoints === 1 ? "" : "s"}
        </span>
      </header>
      <div className="panel-section">
        <div className="segmented heat-metrics" role="tablist" aria-label="Heatmap metric">
          {METRICS.map((m) => (
            <button
              key={m.id}
              type="button"
              role="tab"
              title={m.title}
              aria-selected={metric === m.id}
              className={metric === m.id ? "active" : ""}
              disabled={(m.id === "serving" && servable.length === 0) || (m.id === "requirements" && !requirementsLegend)}
              onClick={() => setMetric(m.id)}
            >
              {m.label}
            </button>
          ))}
        </div>

        {metric === "serving" && servable.length === 0 && (
          <p className="muted small">Place access points and link their BSSIDs to compare them.</p>
        )}
        <div className="heat-fields">
          {metric !== "serving" && metric !== "requirements" && (
          <label>
            <span className="field-label">Network</span>
            <select
              className="input"
              value={encodeNetwork(network)}
              onChange={(e) => setNetwork(decodeNetwork(e.target.value, aps.map((a) => a.ap)))}
            >
              <option value="any">Any network (strongest heard)</option>
              {servable.length > 0 && (
                <optgroup label="Access point (all its BSSIDs)">
                  {servable.map(({ ap }) => (
                    <option key={ap.id} value={`ap:${ap.id}`}>
                      {ap.name} ({ap.bssids.length} BSSID{ap.bssids.length === 1 ? "" : "s"})
                    </option>
                  ))}
                </optgroup>
              )}
              {networks.ssids.length > 0 && (
                <optgroup label="SSID">
                  {networks.ssids.map((s) => (
                    <option key={s.ssid} value={`ssid:${s.ssid}`}>
                      {s.ssid} ({s.points} pts)
                    </option>
                  ))}
                </optgroup>
              )}
              <optgroup label="Single BSSID">
                {networks.bssids.map((b) => (
                  <option key={b.bssid} value={`bssid:${b.bssid}`}>
                    {b.ssid ?? "hidden"} · {b.bssid} · ch {b.channel ?? "?"}
                    {apNameByBssid.has(b.bssid) ? ` · ${apNameByBssid.get(b.bssid)}` : ""}
                  </option>
                ))}
              </optgroup>
            </select>
          </label>
          )}
          {metric !== "requirements" && (
          <label>
            <span className="field-label">Band</span>
            <select className="input" value={band} onChange={(e) => setBand(e.target.value as Band | "all")}>
              {BANDS.map((b) => (
                <option key={b} value={b}>
                  {b === "all" ? "All bands" : BAND_LABEL[b]}
                </option>
              ))}
            </select>
          </label>
          )}
          {metric !== "signal" && metric !== "requirements" && (
            <label>
              <span className="field-label">
                {metric === "coverage" ? "Target" : metric === "serving" ? "Serves from" : "AP counts at"}
              </span>
              <span className="length-input">
                <NumberInput value={threshold} min={THRESHOLD_MIN} max={THRESHOLD_MAX} onCommit={setThreshold} />
                <span className="muted">dBm</span>
              </span>
            </label>
          )}
        </div>

        {/* Legend */}
        {metric === "signal" && (
          <div className="heat-legend" aria-label={`Signal scale ${lo} to ${hi} dBm`}>
            <div className="heat-ramp" style={{ background: `linear-gradient(to right, ${SIGNAL_RAMP.join(", ")})` }}>
              <span className="heat-target" style={{ left: pos(-67) }} title="-67 dBm: common voice/video design target" />
            </div>
            <div className="heat-ticks">
              {[-90, -80, -70, -60, -50, -40, -30].map((t) => (
                <span key={t} style={{ left: pos(t) }}>
                  {t}
                </span>
              ))}
            </div>
            <div className="heat-legend-note muted small">
              dBm. Lightest = −90 or not heard; tick mark = −67 target.
            </div>
          </div>
        )}
        {metric === "coverage" && (
          <div className="heat-legend">
            <div className="heat-stat">
              <strong>{pct}</strong> <span className="muted">of the mapped area reaches {threshold} dBm</span>
            </div>
            <div className="heat-swatches">
              <span>
                <i style={{ background: STATUS_GOOD }} /> ✓ ≥ {threshold} dBm
              </span>
              <span>
                <i style={{ background: STATUS_CRITICAL }} /> ✗ below
              </span>
            </div>
          </div>
        )}
        {metric === "overlap" && (
          <div className="heat-legend">
            <div className="heat-stat">
              <strong>{pct}</strong> <span className="muted">of the mapped area hears 2+ APs at {threshold} dBm</span>
            </div>
            <div className="heat-swatches">
              {["0", "1", "2", "3+"].map((n, i) => (
                <span key={n}>
                  <i style={{ background: OVERLAP_COLORS[i] }} /> {n} AP{n === "1" ? "" : "s"}
                </span>
              ))}
            </div>
            <div className="heat-legend-note muted small">
              BSSIDs from one AP (same MAC apart from the first/last byte) count once.
            </div>
          </div>
        )}

        {metric === "requirements" && requirementsLegend}

        {metric === "serving" && servingShares && (
          <div className="heat-legend">
            <div className="heat-swatches heat-swatches-list">
              {servable.map(({ ap, color }) => (
                <span key={ap.id}>
                  <i style={{ background: color }} /> {ap.name}
                  <span className="muted mono">{Math.round((servingShares.shares.get(ap.id) ?? 0) * 100)} %</span>
                </span>
              ))}
              <span>
                <i style={{ background: NO_SERVICE_COLOR }} /> None at {threshold} dBm
                <span className="muted mono">{Math.round(servingShares.unserved * 100)} %</span>
              </span>
            </div>
            <div className="heat-legend-note muted small">
              Share of the mapped area. Only placed APs with linked BSSIDs are compared.
              {servable.length > 3 && " With more than 3 APs, use the names on the plan or hover to tell regions apart."}
            </div>
          </div>
        )}

        <p className="muted small">
          Estimated between points (inverse-distance weighting); walls are not modelled. Shading stops{" "}
          {scaled ? "3 m" : "a short distance"} from the nearest point.
          {!scaled && " Set the scale for a true 3 m limit."}
          {excludedPoints > 0 &&
            ` ${excludedPoints} point${excludedPoints === 1 ? " has" : "s have"} no dBm readings and ${excludedPoints === 1 ? "is" : "are"} left out.`}
        </p>
      </div>
    </section>
  );
}
