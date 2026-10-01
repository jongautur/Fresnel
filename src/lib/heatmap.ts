// Heatmap interpolation (IDW) over survey points, in plan pixel space.
//
// Honesty rules:
// * Only dBm is interpolated (never %), and never converted.
// * A network not heard at a point counts as NOT_HEARD_DBM there: leaving it
//   out would let a strong neighbour "paint" signal into places where the
//   network was measurably absent.
// * Nothing is drawn further than INFLUENCE_M from the nearest real point.

import type { Band } from "../types/wifi";
import type { FloorPlan, PlacedAp, Sample, SurveyPoint } from "../types/survey";

export type HeatMetric = "signal" | "coverage" | "overlap" | "serving";

export type NetworkFilter =
  | { kind: "any" }
  | { kind: "ssid"; ssid: string }
  | { kind: "bssid"; bssid: string }
  /** All BSSIDs linked to a placed AP. */
  | { kind: "ap"; apId: number; bssids: string[] };

export interface HeatmapConfig {
  metric: HeatMetric;
  network: NetworkFilter;
  band: Band | "all";
  /** dBm: pass level for coverage, "counts as an AP" level for overlap and serving. */
  threshold: number;
}

/** Stand-in for "not heard": below any receiver's sensitivity. */
export const NOT_HEARD_DBM = -100;
export const SIGNAL_DOMAIN: [number, number] = [-90, -30];
/** Fade-out distance from the nearest measured point. */
export const INFLUENCE_M = 3;
/** Without a scale: fraction of the plan's longest side. */
const INFLUENCE_UNSCALED = 0.15;
const IDW_POWER = 2;
const OPACITY = 0.68;
const MAX_CELLS = 250_000;

// Sequential blue ramp, steps 100 → 700 (weak → strong). The heatmap sits on
// the plan image, not the theme surface, so it does not change with the theme.
export const SIGNAL_RAMP = [
  "#cde2fb", "#b7d3f6", "#9ec5f4", "#86b6ef", "#6da7ec", "#5598e7", "#3987e5",
  "#2a78d6", "#256abf", "#1c5cab", "#184f95", "#104281", "#0d366b",
];
/** Status colours (fixed): coverage is pass/fail, so it wears good/critical. */
export const STATUS_GOOD = "#0ca30c";
export const STATUS_CRITICAL = "#d03b3b";
/** Ordinal steps for 0, 1, 2, 3+ APs; 0 is neutral grey. */
export const OVERLAP_COLORS = ["#b4b4ae", "#86b6ef", "#3987e5", "#184f95"];
/**
 * Categorical slots for placed APs (light-mode steps: they sit on the white
 * plan). An AP's colour follows the AP (building order), never its rank.
 * Past 8 APs, the rest share OTHER_AP_COLOR.
 */
export const AP_COLORS = ["#2a78d6", "#eb6834", "#1baf7a", "#eda100", "#e87ba4", "#008300", "#4a3aa7", "#e34948"];
export const OTHER_AP_COLOR = "#7a7a75";
/** Serving view: below the level, no AP "serves" the spot. */
export const NO_SERVICE_COLOR = "#b4b4ae";

export function apColor(index: number): string {
  return index >= 0 && index < AP_COLORS.length ? AP_COLORS[index]! : OTHER_AP_COLOR;
}

const rgb = (hex: string): [number, number, number] => [
  parseInt(hex.slice(1, 3), 16),
  parseInt(hex.slice(3, 5), 16),
  parseInt(hex.slice(5, 7), 16),
];
const RAMP_RGB = SIGNAL_RAMP.map(rgb);
const GOOD_RGB = rgb(STATUS_GOOD);
const CRITICAL_RGB = rgb(STATUS_CRITICAL);
const OVERLAP_RGB = OVERLAP_COLORS.map(rgb);

/**
 * Same physical AP across bands/SSIDs: BSSIDs derived from one base MAC
 * usually differ only in the first and/or last octet. Heuristic.
 */
export function apKey(bssid: string): string {
  return bssid.toLowerCase().split(":").slice(1, 5).join(":");
}

function matches(s: Sample, cfg: HeatmapConfig): boolean {
  if (cfg.band !== "all" && s.band !== cfg.band) return false;
  switch (cfg.network.kind) {
    case "any":
      return true;
    case "ssid":
      return s.ssid === cfg.network.ssid;
    case "bssid":
      return s.bssid === cfg.network.bssid;
    case "ap":
      return cfg.network.bssids.includes(s.bssid);
  }
}

/**
 * The value a point contributes, or null if it can't contribute (its
 * adapter reported no dBm at all).
 */
export function pointValue(p: SurveyPoint, cfg: HeatmapConfig): number | null {
  if (p.samples.length > 0 && p.samples.every((s) => s.signal.dbm == null)) return null;
  const heard = p.samples.filter((s) => s.signal.dbm != null && matches(s, cfg));
  if (cfg.metric === "overlap") {
    return new Set(heard.filter((s) => s.signal.dbm! >= cfg.threshold).map((s) => apKey(s.bssid))).size;
  }
  return heard.length ? Math.max(...heard.map((s) => s.signal.dbm!)) : NOT_HEARD_DBM;
}

export interface HeatPoint {
  x: number;
  y: number;
  v: number;
}

export function influenceRadiusPx(plan: FloorPlan, ppm: number | null): number {
  return ppm ? INFLUENCE_M * ppm : INFLUENCE_UNSCALED * Math.max(plan.width, plan.height);
}

export interface Estimate {
  value: number;
  /** Distance to the nearest real point, plan px. */
  nearestPx: number;
  nearest: HeatPoint;
}

/** IDW estimate at (x, y) from points within 2 × radius; null outside the radius. */
export function estimateAt(pts: HeatPoint[], x: number, y: number, radius: number): Estimate | null {
  const reach = radius * 2;
  let num = 0;
  let den = 0;
  let nearestPx = Infinity;
  let nearest: HeatPoint | null = null;
  for (const p of pts) {
    const dx = p.x - x;
    const dy = p.y - y;
    if (Math.abs(dx) > reach || Math.abs(dy) > reach) continue;
    const d2 = dx * dx + dy * dy;
    const d = Math.sqrt(d2);
    if (d < nearestPx) {
      nearestPx = d;
      nearest = p;
    }
    if (d2 < 1e-6) return { value: p.v, nearestPx: 0, nearest: p };
    const w = 1 / Math.pow(d2, IDW_POWER / 2);
    num += w * p.v;
    den += w;
  }
  if (!nearest || nearestPx > radius || den === 0) return null;
  return { value: num / den, nearestPx, nearest };
}

/** Passing the coverage threshold, for the given metric's estimate. */
export function passes(value: number, cfg: HeatmapConfig): boolean {
  if (cfg.metric === "serving") return value >= cfg.threshold;
  return cfg.metric === "overlap" ? Math.round(value) >= 2 : value >= cfg.threshold;
}

function colour(value: number, cfg: HeatmapConfig): [number, number, number] {
  if (cfg.metric === "coverage") return value >= cfg.threshold ? GOOD_RGB : CRITICAL_RGB;
  if (cfg.metric === "overlap") return OVERLAP_RGB[Math.min(3, Math.max(0, Math.round(value)))]!;
  const [lo, hi] = SIGNAL_DOMAIN;
  const t = Math.min(1, Math.max(0, (value - lo) / (hi - lo))) * (RAMP_RGB.length - 1);
  const i = Math.min(RAMP_RGB.length - 2, Math.floor(t));
  const f = t - i;
  const a = RAMP_RGB[i]!;
  const b = RAMP_RGB[i + 1]!;
  return [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f, a[2] + (b[2] - a[2]) * f];
}

export interface HeatGrid {
  cols: number;
  rows: number;
  rgba: Uint8ClampedArray;
  /** Share of the mapped (non-masked) area at or above the threshold. */
  passingFraction: number | null;
}

/** Rasterise the heatmap at ~10 cm per cell (scaled plans), capped at MAX_CELLS. */
export function buildGrid(plan: FloorPlan, ppm: number | null, pts: HeatPoint[], cfg: HeatmapConfig): HeatGrid | null {
  if (pts.length === 0) return null;
  let cell = ppm ? ppm * 0.1 : Math.max(plan.width, plan.height) / 300;
  cell = Math.max(cell, Math.sqrt((plan.width * plan.height) / MAX_CELLS), 1);
  const cols = Math.ceil(plan.width / cell);
  const rows = Math.ceil(plan.height / cell);
  const radius = influenceRadiusPx(plan, ppm);
  const fade = radius * 0.25;
  const rgba = new Uint8ClampedArray(cols * rows * 4);
  let mapped = 0;
  let passing = 0;

  for (let r = 0; r < rows; r++) {
    const y = (r + 0.5) * cell;
    for (let c = 0; c < cols; c++) {
      const e = estimateAt(pts, (c + 0.5) * cell, y, radius);
      if (!e) continue;
      mapped++;
      if (passes(e.value, cfg)) passing++;
      const [R, G, B] = colour(e.value, cfg);
      const edge = Math.min(1, (radius - e.nearestPx) / fade);
      const o = (r * cols + c) * 4;
      rgba[o] = R;
      rgba[o + 1] = G;
      rgba[o + 2] = B;
      rgba[o + 3] = 255 * OPACITY * edge;
    }
  }
  return { cols, rows, rgba, passingFraction: mapped ? passing / mapped : null };
}

/** Networks heard on this floor, most widely heard first. */
export function floorNetworks(points: SurveyPoint[]) {
  const ssids = new Map<string, number>();
  const bssids = new Map<string, { bssid: string; ssid: string | null; channel: number | null; band: Band; points: number }>();
  for (const p of points) {
    const seenSsid = new Set<string>();
    for (const s of p.samples) {
      if (s.ssid != null && !seenSsid.has(s.ssid)) {
        seenSsid.add(s.ssid);
        ssids.set(s.ssid, (ssids.get(s.ssid) ?? 0) + 1);
      }
      const b = bssids.get(s.bssid);
      if (b) b.points++;
      else bssids.set(s.bssid, { bssid: s.bssid, ssid: s.ssid, channel: s.channel, band: s.band, points: 1 });
    }
  }
  return {
    ssids: [...ssids.entries()].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0])).map(([ssid, n]) => ({ ssid, points: n })),
    bssids: [...bssids.values()].sort(
      (a, b) => (a.ssid ?? "~").localeCompare(b.ssid ?? "~") || a.band.localeCompare(b.band) || a.bssid.localeCompare(b.bssid),
    ),
  };
}

// ---------------------------------------------------------------------------
// Serving AP: which placed AP is strongest at each spot
// ---------------------------------------------------------------------------

export interface ServingAp {
  ap: PlacedAp;
  color: string;
  pts: HeatPoint[];
}

/** Per-AP point values (strongest of its BSSIDs, NOT_HEARD where none heard). */
export function servingInputs(points: SurveyPoint[], aps: { ap: PlacedAp; color: string }[], band: Band | "all"): ServingAp[] {
  return aps
    .filter(({ ap }) => ap.bssids.length > 0)
    .map(({ ap, color }) => {
      const cfg: HeatmapConfig = { metric: "signal", network: { kind: "ap", apId: ap.id, bssids: ap.bssids }, band, threshold: 0 };
      const pts: HeatPoint[] = [];
      for (const p of points) {
        const v = pointValue(p, cfg);
        if (v != null) pts.push({ x: p.x, y: p.y, v });
      }
      return { ap, color, pts };
    })
    .filter((s) => s.pts.some((p) => p.v > NOT_HEARD_DBM));
}

export interface ServingEstimate {
  /** Strongest first. */
  ranked: { ap: PlacedAp; color: string; value: number }[];
  nearestPx: number;
}

export function servingAt(inputs: ServingAp[], x: number, y: number, radius: number): ServingEstimate | null {
  let nearestPx = Infinity;
  const ranked: ServingEstimate["ranked"] = [];
  for (const s of inputs) {
    const e = estimateAt(s.pts, x, y, radius);
    if (!e) continue;
    nearestPx = Math.min(nearestPx, e.nearestPx);
    ranked.push({ ap: s.ap, color: s.color, value: e.value });
  }
  if (!ranked.length) return null;
  ranked.sort((a, b) => b.value - a.value);
  return { ranked, nearestPx };
}

export interface ServingGrid extends HeatGrid {
  /** Share of the mapped area each AP serves, by AP id. */
  shares: Map<number, number>;
  /** Share where no AP reaches the threshold. */
  unserved: number;
}

export function buildServingGrid(
  plan: FloorPlan,
  ppm: number | null,
  inputs: ServingAp[],
  threshold: number,
): ServingGrid | null {
  if (!inputs.length) return null;
  let cell = ppm ? ppm * 0.1 : Math.max(plan.width, plan.height) / 300;
  // Cost scales with the AP count; keep the grid a bit coarser.
  cell = Math.max(cell, Math.sqrt((plan.width * plan.height * inputs.length) / (MAX_CELLS * 2)), 1);
  const cols = Math.ceil(plan.width / cell);
  const rows = Math.ceil(plan.height / cell);
  const radius = influenceRadiusPx(plan, ppm);
  const fade = radius * 0.25;
  const rgba = new Uint8ClampedArray(cols * rows * 4);
  const counts = new Map<number, number>();
  const noService = rgb(NO_SERVICE_COLOR);
  let mapped = 0;
  let unserved = 0;
  for (let r = 0; r < rows; r++) {
    const y = (r + 0.5) * cell;
    for (let c = 0; c < cols; c++) {
      const e = servingAt(inputs, (c + 0.5) * cell, y, radius);
      if (!e) continue;
      mapped++;
      const best = e.ranked[0]!;
      let col: [number, number, number];
      if (best.value >= threshold) {
        counts.set(best.ap.id, (counts.get(best.ap.id) ?? 0) + 1);
        col = rgb(best.color);
      } else {
        unserved++;
        col = noService;
      }
      const o = (r * cols + c) * 4;
      rgba[o] = col[0];
      rgba[o + 1] = col[1];
      rgba[o + 2] = col[2];
      rgba[o + 3] = 255 * OPACITY * Math.min(1, (radius - e.nearestPx) / fade);
    }
  }
  const shares = new Map<number, number>();
  for (const [id, n] of counts) shares.set(id, n / mapped);
  return { cols, rows, rgba, passingFraction: mapped ? 1 - unserved / mapped : null, shares, unserved: mapped ? unserved / mapped : 0 };
}

/**
 * Suggest which BSSIDs belong to an AP placed at (x, y): the BSSID group
 * (same base MAC) with the strongest reading among points within `radius`.
 */
export function suggestBssidGroup(points: SurveyPoint[], x: number, y: number, radius: number, exclude: Set<string>): string[] {
  const near = points.filter((p) => Math.hypot(p.x - x, p.y - y) <= radius);
  let best: { key: string; dbm: number } | null = null;
  for (const p of near)
    for (const s of p.samples)
      if (s.signal.dbm != null && !exclude.has(s.bssid) && (!best || s.signal.dbm > best.dbm))
        best = { key: apKey(s.bssid), dbm: s.signal.dbm };
  if (!best) return [];
  const all = new Set<string>();
  for (const p of points) for (const s of p.samples) if (apKey(s.bssid) === best.key && !exclude.has(s.bssid)) all.add(s.bssid);
  return [...all].sort();
}
