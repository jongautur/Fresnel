// Requirements: labels, and the share of mapped AREA meeting a profile.
//
// Point results come from Rust (pass / fail / not evaluated per point). The
// area estimate uses the coverage heatmap's machinery: the continuous inputs
// (primary and secondary dBm) are interpolated per cell (IDW) and held
// against the profile's levels; the co-channel count is interpolated and
// rounded, like the overlap view; rules that are yes/no at a point (required
// bands, SNR, utilisation) come from the nearest evaluated point. Points that
// weren't evaluated (e.g. % only) don't take part.

import type { Band } from "../types/wifi";
import type { FloorPlan, SurveyPoint } from "../types/survey";
import type { PointEvaluation, RequirementProfile, RequirementValues, Rule, RuleResult } from "../types/requirements";
import { BAND_LABEL } from "./format";
import {
  MAX_CELLS,
  NOT_HEARD_DBM,
  OPACITY,
  STATUS_CRITICAL,
  STATUS_GOOD,
  estimateAt,
  influenceRadiusPx,
  type HeatGrid,
  type HeatPoint,
} from "./heatmap";

export const RULE_LABEL: Record<Rule, string> = {
  primary_signal: "Primary signal",
  secondary_signal: "Second AP",
  co_channel: "Co-channel",
  required_band: "Required band",
  snr: "SNR",
  utilisation: "Channel load",
};

export function ruleLabel(r: { rule: Rule; band: Band | null }): string {
  return r.band ? `${BAND_LABEL[r.band]} required` : RULE_LABEL[r.rule];
}

/** One line per active rule, for the editor and summaries. */
export function describeValues(v: RequirementValues): string[] {
  const out = [`Primary ≥ ${v.primaryMinDbm} dBm`];
  if (v.secondaryMinDbm != null) out.push(`second AP ≥ ${v.secondaryMinDbm} dBm`);
  if (v.cochannelMax != null) out.push(`≤ ${v.cochannelMax} co-channel at ${v.cochannelLevelDbm} dBm`);
  if (v.requiredBands.length) out.push(`${v.requiredBands.map((b) => BAND_LABEL[b]).join(" + ")} required`);
  if (v.minSnrDb != null) out.push(`SNR ≥ ${v.minSnrDb} dB`);
  if (v.maxUtilPct != null) out.push(`load ≤ ${v.maxUtilPct} %`);
  return out;
}

export const formatShare = (f: number) => `${Math.round(f * 100)} %`;

// ---------------------------------------------------------------------------
// Area estimate
// ---------------------------------------------------------------------------

type IdPoint = HeatPoint & { id: number };

export interface AreaInputs {
  values: RequirementValues;
  primary: IdPoint[];
  /** Empty when the profile has no secondary rule. */
  secondary: HeatPoint[];
  /** Points with a co-channel count (primary heard), when the rule is on. */
  cochannel: HeatPoint[];
  /** Point id → false when a yes/no rule (band, SNR, load) failed there. */
  discrete: Map<number, boolean>;
}

const DISCRETE: Rule[] = ["required_band", "snr", "utilisation"];

export function areaInputs(points: SurveyPoint[], evals: PointEvaluation[], profile: RequirementProfile): AreaInputs {
  const byId = new Map(evals.map((e) => [e.pointId, e]));
  const inputs: AreaInputs = { values: profile, primary: [], secondary: [], cochannel: [], discrete: new Map() };
  for (const p of points) {
    const e = byId.get(p.id);
    if (!e || e.outcome === "not_evaluated") continue;
    inputs.primary.push({ id: p.id, x: p.x, y: p.y, v: e.primaryDbm ?? NOT_HEARD_DBM });
    if (profile.secondaryMinDbm != null) inputs.secondary.push({ x: p.x, y: p.y, v: e.secondaryDbm ?? NOT_HEARD_DBM });
    if (profile.cochannelMax != null && e.cochannelCount != null)
      inputs.cochannel.push({ x: p.x, y: p.y, v: e.cochannelCount });
    inputs.discrete.set(p.id, !e.rules.some((r) => DISCRETE.includes(r.rule) && r.outcome === "fail"));
  }
  return inputs;
}

export interface AreaEstimate {
  pass: boolean;
  /** What fails here, e.g. "primary ≈ −71 dBm". */
  failing: string[];
  nearestPx: number;
  nearestId: number;
}

export function requirementsAt(inputs: AreaInputs, x: number, y: number, radius: number): AreaEstimate | null {
  const v = inputs.values;
  const e = estimateAt(inputs.primary, x, y, radius);
  if (!e) return null;
  const failing: string[] = [];
  const dbm = (n: number) => (n <= NOT_HEARD_DBM ? "not heard" : `≈ ${Math.round(n)} dBm`);
  if (e.value < v.primaryMinDbm) failing.push(`primary ${dbm(e.value)}`);
  if (v.secondaryMinDbm != null) {
    const s = estimateAt(inputs.secondary, x, y, radius);
    if (!s || s.value < v.secondaryMinDbm) failing.push(`second AP ${s ? dbm(s.value) : "not heard"}`);
  }
  if (v.cochannelMax != null) {
    const c = estimateAt(inputs.cochannel, x, y, radius);
    if (c && Math.round(c.value) > v.cochannelMax) failing.push(`${Math.round(c.value)} co-channel`);
  }
  const nearestId = (e.nearest as IdPoint).id;
  if (inputs.discrete.get(nearestId) === false) failing.push("band / SNR / load (nearest point)");
  return { pass: failing.length === 0, failing, nearestPx: e.nearestPx, nearestId };
}

const rgb = (hex: string): [number, number, number] => [
  parseInt(hex.slice(1, 3), 16),
  parseInt(hex.slice(3, 5), 16),
  parseInt(hex.slice(5, 7), 16),
];

/**
 * Pass/fail raster on the same grid as the coverage heatmap (same cell size
 * and mask), so the two views' area shares are comparable.
 */
export function buildRequirementsGrid(plan: FloorPlan, ppm: number | null, inputs: AreaInputs): HeatGrid | null {
  if (inputs.primary.length === 0) return null;
  let cell = ppm ? ppm * 0.1 : Math.max(plan.width, plan.height) / 300;
  cell = Math.max(cell, Math.sqrt((plan.width * plan.height) / MAX_CELLS), 1);
  const cols = Math.ceil(plan.width / cell);
  const rows = Math.ceil(plan.height / cell);
  const radius = influenceRadiusPx(plan, ppm);
  const fade = radius * 0.25;
  const rgba = new Uint8ClampedArray(cols * rows * 4);
  const good = rgb(STATUS_GOOD);
  const bad = rgb(STATUS_CRITICAL);
  let mapped = 0;
  let passing = 0;
  for (let r = 0; r < rows; r++) {
    const y = (r + 0.5) * cell;
    for (let c = 0; c < cols; c++) {
      const e = requirementsAt(inputs, (c + 0.5) * cell, y, radius);
      if (!e) continue;
      mapped++;
      if (e.pass) passing++;
      const [R, G, B] = e.pass ? good : bad;
      const o = (r * cols + c) * 4;
      rgba[o] = R;
      rgba[o + 1] = G;
      rgba[o + 2] = B;
      rgba[o + 3] = 255 * OPACITY * Math.min(1, (radius - e.nearestPx) / fade);
    }
  }
  return { cols, rows, rgba, passingFraction: mapped ? passing / mapped : null };
}

/** Marker text on the plan. */
export function outcomeMark(e: PointEvaluation | undefined): string {
  if (!e) return "—";
  return e.outcome === "pass" ? "✓" : e.outcome === "fail" ? "✗" : "–";
}

export function failingRules(e: PointEvaluation): RuleResult[] {
  return e.rules.filter((r) => r.outcome === "fail");
}
