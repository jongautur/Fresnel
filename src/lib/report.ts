// Survey report: one self-contained HTML file the customer opens in their
// own browser and prints to PDF, plus the raw data as CSV and JSON.
//
// Safety: the file carries its own CSP (nothing may load), contains no
// script, and every image is a data: URI. SSIDs, names, notes and captions
// are untrusted, so all text goes through esc() where it enters markup.
// SVG plans are only ever referenced as images (<image href="data:…">),
// never inlined.
//
// Honesty: maps are IDW estimates and say so; every derived finding states
// what it is based on; what wasn't measured is said, not filled in.
//
// No DOM here: PNG encoding and grid building are passed in, so node scripts
// (scripts/test-report.mjs) can build a report too.

import type { Band } from "../types/wifi";
import type { Building, Floor, PlacedAp, Sample, SurveyPoint } from "../types/survey";
import type { Project } from "../types/project";
import type { FloorAnnotations } from "../types/notes";
import type { Findings } from "../types/findings";
import type { FloorRequirements, RequirementProfile } from "../types/requirements";
import { BAND_LABEL, SECURITY_LABEL } from "./format";
import {
  INFLUENCE_M,
  NOT_HEARD_DBM,
  SIGNAL_DOMAIN,
  SIGNAL_RAMP,
  STATUS_CRITICAL,
  STATUS_GOOD,
  NO_SERVICE_COLOR,
  apColor,
  pointValue,
  servingInputs,
  type HeatGrid,
  type HeatPoint,
  type HeatmapConfig,
  type ServingGrid,
} from "./heatmap";
import type { HeatJob, HeatResult } from "./heatmapJobs";
import { RULE_LABEL, areaInputs, describeValues, formatShare } from "./requirements";

export const REPORT_CSP = "default-src 'none'; img-src data:; style-src 'unsafe-inline'";
/** Above this the UI warns before saving: large files are slow to open and print. */
export const REPORT_SIZE_LIMIT = 25 * 1024 * 1024;
/** Serving-AP level without a requirements profile; the screen's default overlap level. */
const DEFAULT_SERVING_DBM = -75;
/** Coverage target without a requirements profile. */
export const DEFAULT_TARGET_DBM = -67;
/** Longest lists in findings; the rest are counted. */
const MAX_ITEMS = 15;
const WORST_POINTS = 5;

/** Provider ids as stored with each point → the OS they run on. Unknown ids are shown as recorded. */
const PROVIDER_LABEL: Record<string, string> = {
  networkmanager: "Linux (NetworkManager)",
  windows: "Windows (Native Wifi API)",
};

// ---------------------------------------------------------------------------
// Escaping and raw data
// ---------------------------------------------------------------------------

const ESCAPES: Record<string, string> = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };

/** Text → HTML text or attribute value. null/undefined → "". */
export function esc(v: unknown): string {
  return String(v ?? "").replace(/[&<>"']/g, (c) => ESCAPES[c]!);
}

/** Multi-line user text. */
const escLines = (v: string) => esc(v).replace(/\r?\n/g, "<br>");

/**
 * One CSV cell. Numbers stay numbers; text is quoted, and text a
 * spreadsheet would run as a formula (= + - @, tab, CR) gets a leading '.
 */
export function csvCell(v: string | number | boolean | null | undefined): string {
  if (v == null) return "";
  if (typeof v === "number") return Number.isFinite(v) ? String(v) : "";
  if (typeof v === "boolean") return v ? "true" : "false";
  const safe = /^[=+\-@\t\r]/.test(v) ? `'${v}` : v;
  return `"${safe.replaceAll('"', '""')}"`;
}

export interface RawFloor {
  building: Building;
  floor: Floor;
  points: SurveyPoint[];
}

const enc = new TextEncoder();

/** Every sample of every point, one row each; UTF-8 with BOM so Excel reads it right. */
export function rawCsv(floors: RawFloor[]): Uint8Array {
  const head = [
    "building", "floor", "point_number", "point_id", "measured_at", "x_px", "y_px", "x_m", "y_m",
    "adapter_id", "adapter_provider", "adapter_model", "adapter_driver", "scan_duration_ms",
    "bssid", "ssid", "dbm", "quality_percent", "frequency_mhz", "channel", "band", "width_mhz",
    "center_mhz", "security", "snr_db", "noise_dbm", "utilization_pct", "station_count", "connected",
  ];
  const rows: string[] = [head.join(",")];
  for (const { building, floor, points } of floors) {
    const ppm = floor.scale ? pxPerMetreOf(floor) : null;
    points.forEach((p, i) => {
      const base = [
        csvCell(building.name), csvCell(floor.name), csvCell(i + 1), csvCell(p.id), csvCell(p.measuredAt),
        csvCell(p.x), csvCell(p.y), csvCell(ppm ? round(p.x / ppm, 2) : null), csvCell(ppm ? round(p.y / ppm, 2) : null),
        csvCell(p.adapter.id), csvCell(p.adapter.provider), csvCell(p.adapter.model), csvCell(p.adapter.driver),
        csvCell(p.scanDurationMs),
      ];
      // A point that heard nothing still gets a row: it is a measurement.
      if (p.samples.length === 0) rows.push([...base, ...new Array<string>(15).fill("")].join(","));
      for (const s of p.samples)
        rows.push(
          [
            ...base,
            csvCell(s.bssid), csvCell(s.ssid), csvCell(s.signal.dbm), csvCell(s.signal.qualityPercent),
            csvCell(s.frequencyMhz), csvCell(s.channel), csvCell(s.band), csvCell(s.channelWidthMhz),
            csvCell(s.channelCenterMhz), csvCell(s.security), csvCell(s.snrDb), csvCell(s.noiseDbm),
            csvCell(s.channelUtilizationPct), csvCell(s.stationCount), csvCell(s.isConnected),
          ].join(","),
        );
    });
  }
  return enc.encode("﻿" + rows.join("\r\n") + "\r\n");
}

/** Everything stored for the floors, as the app holds it. */
export function rawJson(data: { project: Project; floors: RawFloor[]; aps: PlacedAp[]; version: string; exportedAt: string }): Uint8Array {
  return enc.encode(
    JSON.stringify(
      {
        format: "fresnel-survey-data",
        formatVersion: 1,
        fresnelVersion: data.version,
        exportedAt: data.exportedAt,
        project: data.project,
        floors: data.floors.map((f) => ({ building: f.building, floor: f.floor, points: f.points })),
        accessPoints: data.aps,
      },
      null,
      2,
    ),
  );
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

export type ReportScope = { kind: "floor" | "building" | "project"; id: number };

export interface ReportFloor {
  building: Building;
  floor: Floor;
  /** Measurement order (as numbered on the plan). */
  points: SurveyPoint[];
  annotations: FloorAnnotations;
  requirements: FloorRequirements | null;
  /** The plan as stored, as a data: URI; null without a plan. */
  planData: string | null;
  /** Photo id → data: URI of its report copy. */
  photoData: Map<number, string>;
}

export interface ReportAp {
  ap: PlacedAp;
  buildingId: number;
  floorName: string;
}

export interface ReportSource {
  project: Project;
  scope: ReportScope;
  floors: ReportFloor[];
  /** Every placed AP in the project: whether a BSSID is "ours" is decided project-wide. */
  aps: ReportAp[];
  /** Rogue / unknown-transmitter findings for the scope. */
  findings: Findings;
  ssids: string[];
  branding: { technicianName: string | null; companyName: string | null; logoData: string | null };
  version: string;
  generatedAt: Date;
}

export interface ReportDeps {
  compute(job: HeatJob): Promise<HeatResult>;
  /** A grid as a PNG data: URI (a canvas in the app). */
  encodePng(grid: HeatGrid): string;
  onProgress?(done: number, total: number, what: string): void;
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

const round = (v: number, d = 0) => Math.round(v * 10 ** d) / 10 ** d;
const pct = (f: number) => formatShare(f);
const dbm = (v: number | null) => (v == null ? "—" : v <= NOT_HEARD_DBM ? "not heard" : `${Math.round(v)} dBm`);
const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;
const ssidText = (s: string | null) => (s == null || s === "" ? "(hidden)" : `“${s}”`);

function pxPerMetreOf(floor: Floor): number | null {
  const s = floor.scale;
  return s ? Math.hypot(s.x2 - s.x1, s.y2 - s.y1) / s.lengthM : null;
}

function unique<T>(xs: T[]): T[] {
  return [...new Set(xs)];
}

/** "a, b and 3 more" */
function capped(items: string[], max = MAX_ITEMS): string[] {
  return items.length > max ? [...items.slice(0, max), `… and ${items.length - max} more`] : items;
}

function targetOf(f: ReportFloor): { level: number; source: string; profile: RequirementProfile | null } {
  const profile = f.requirements?.profile ?? null;
  return profile
    ? { level: profile.primaryMinDbm, source: `requirements profile “${profile.name}”`, profile }
    : { level: DEFAULT_TARGET_DBM, source: "default target (no requirements profile)", profile: null };
}

// ---------------------------------------------------------------------------
// Channel spans (as in the Rust requirements check)
// ---------------------------------------------------------------------------

interface Radio {
  bssid: string;
  ssid: string | null;
  band: Band;
  channel: number | null;
  frequencyMhz: number;
  widthMhz: number | null;
  lo: number;
  hi: number;
  /** false: no centre/width reported, span is the 20 MHz primary channel. */
  widthKnown: boolean;
  security: string;
  strongest: number | null;
  strongestAt: string | null;
  points: number;
  apId: number | null;
  markedOurs: boolean;
}

function span(s: Sample): { lo: number; hi: number; widthKnown: boolean } {
  if (s.channelCenterMhz != null && s.channelWidthMhz != null && s.channelWidthMhz > 0)
    return { lo: s.channelCenterMhz - s.channelWidthMhz / 2, hi: s.channelCenterMhz + s.channelWidthMhz / 2, widthKnown: true };
  return { lo: s.frequencyMhz - 10, hi: s.frequencyMhz + 10, widthKnown: false };
}

const overlaps = (a: { lo: number; hi: number }, b: { lo: number; hi: number }) => a.lo < b.hi && b.lo < a.hi;

function channelText(r: Radio): string {
  const ch = r.channel != null ? `ch ${r.channel}` : `${r.frequencyMhz} MHz`;
  return r.widthKnown ? `${ch}, ${r.widthMhz} MHz wide` : `${ch}, width not reported: primary channel only`;
}

/** Every BSSID heard in scope; channel from its latest reading, level from its strongest. */
function collectRadios(src: ReportSource): Map<string, Radio> {
  const apByBssid = new Map<string, number>();
  for (const { ap } of src.aps) for (const b of ap.bssids) apByBssid.set(b, ap.id);
  const ours = new Set(src.findings.marks.filter((m) => m.status === "ours_unplaced").map((m) => m.bssid));
  const latest = new Map<string, { at: string; s: Sample }>();
  const radios = new Map<string, Radio>();
  for (const f of src.floors)
    f.points.forEach((p, i) => {
      for (const s of p.samples) {
        const l = latest.get(s.bssid);
        if (!l || p.measuredAt > l.at) latest.set(s.bssid, { at: p.measuredAt, s });
        let r = radios.get(s.bssid);
        if (!r) {
          r = {
            bssid: s.bssid, ssid: s.ssid, band: s.band, channel: s.channel, frequencyMhz: s.frequencyMhz,
            widthMhz: s.channelWidthMhz, lo: 0, hi: 0, widthKnown: false, security: SECURITY_LABEL[s.security],
            strongest: null, strongestAt: null, points: 0, apId: apByBssid.get(s.bssid) ?? null, markedOurs: ours.has(s.bssid),
          };
          radios.set(s.bssid, r);
        }
        r.points++;
        if (s.signal.dbm != null && (r.strongest == null || s.signal.dbm > r.strongest)) {
          r.strongest = s.signal.dbm;
          r.strongestAt = `point ${i + 1}, ${f.floor.name}`;
        }
      }
    });
  for (const [bssid, { s }] of latest) {
    const r = radios.get(bssid)!;
    Object.assign(r, { ssid: s.ssid, band: s.band, channel: s.channel, frequencyMhz: s.frequencyMhz, widthMhz: s.channelWidthMhz, security: SECURITY_LABEL[s.security] }, span(s));
  }
  return radios;
}

// ---------------------------------------------------------------------------
// Per-floor maps
// ---------------------------------------------------------------------------

interface SsidMaps {
  ssid: string;
  /** Points where the SSID was heard with dBm. */
  heardAt: number;
  signal: string | null;
  coverage: string | null;
  passing: number | null;
  /** Lowest points below the target: number (1-based) and value. */
  worst: { n: number; v: number }[];
  belowPoints: number;
}

interface FloorReport {
  f: ReportFloor;
  ppm: number | null;
  target: ReturnType<typeof targetOf>;
  usedPoints: number;
  ssids: SsidMaps[];
  serving: { map: string; grid: ServingGrid; level: number; aps: { ap: PlacedAp; color: string }[] } | null;
  requirements: { map: string | null; passing: number | null } | null;
  /** Building's APs with their colours (building order, as on screen). */
  colored: { ap: PlacedAp; color: string }[];
}

function stepsFor(src: ReportSource): number {
  return src.floors.reduce((n, f) => n + (f.floor.plan && f.points.length ? src.ssids.length * 2 + 2 : 0), 0);
}

async function floorMaps(src: ReportSource, f: ReportFloor, deps: ReportDeps, tick: (what: string) => void): Promise<FloorReport> {
  const plan = f.floor.plan;
  const ppm = pxPerMetreOf(f.floor);
  const target = targetOf(f);
  const colored = src.aps
    .filter((a) => a.buildingId === f.building.id)
    .map((a) => a.ap)
    .sort((a, b) => a.id - b.id)
    .map((ap, i) => ({ ap, color: apColor(i) }));
  const out: FloorReport = { f, ppm, target, usedPoints: 0, ssids: [], serving: null, requirements: null, colored };
  if (!plan || f.points.length === 0) return out;
  const label = `${f.building.name} / ${f.floor.name}`;
  const png = (g: HeatResult) => (g ? deps.encodePng(g) : null);

  for (const ssid of src.ssids) {
    const cfg: HeatmapConfig = { metric: "signal", network: { kind: "ssid", ssid }, band: "all", threshold: target.level };
    const pts: (HeatPoint & { n: number })[] = [];
    f.points.forEach((p, i) => {
      const v = pointValue(p, cfg);
      if (v != null) pts.push({ x: p.x, y: p.y, v, n: i + 1 });
    });
    out.usedPoints = Math.max(out.usedPoints, pts.length);
    const heardAt = pts.filter((p) => p.v > NOT_HEARD_DBM).length;
    const below = pts.filter((p) => p.v < target.level).sort((a, b) => a.v - b.v || a.n - b.n);
    const m: SsidMaps = { ssid, heardAt, signal: null, coverage: null, passing: null, worst: below.slice(0, WORST_POINTS).map(({ n, v }) => ({ n, v })), belowPoints: below.length };
    if (heardAt > 0) {
      m.signal = png(await deps.compute({ kind: "grid", plan, ppm, pts, cfg }));
      tick(`${label}: signal for ${ssid}`);
      const cov = await deps.compute({ kind: "grid", plan, ppm, pts, cfg: { ...cfg, metric: "coverage" } });
      m.coverage = png(cov);
      m.passing = cov?.passingFraction ?? null;
      tick(`${label}: coverage for ${ssid}`);
    } else {
      tick(`${label}: ${ssid} not heard`);
      tick(`${label}: ${ssid} not heard`);
    }
    out.ssids.push(m);
  }

  const servable = colored.filter(({ ap }) => ap.bssids.length > 0);
  const inputs = servingInputs(f.points, servable, "all");
  if (inputs.length) {
    // Same level as the screen's serving view: the profile's second-AP level, else its primary.
    const servingLevel = target.profile?.secondaryMinDbm ?? target.profile?.primaryMinDbm ?? DEFAULT_SERVING_DBM;
    const grid = (await deps.compute({ kind: "serving", plan, ppm, inputs, threshold: servingLevel })) as ServingGrid | null;
    if (grid) out.serving = { map: deps.encodePng(grid), grid, level: servingLevel, aps: inputs.map(({ ap, color }) => ({ ap, color })) };
  }
  tick(`${label}: serving AP`);

  const req = f.requirements;
  if (req?.profile) {
    const grid = await deps.compute({ kind: "requirements", plan, ppm, inputs: areaInputs(f.points, req.points, req.profile) });
    out.requirements = { map: png(grid), passing: grid?.passingFraction ?? null };
  }
  tick(`${label}: requirements`);
  return out;
}

// ---------------------------------------------------------------------------
// Derived findings
// ---------------------------------------------------------------------------

interface Derived {
  title: string;
  summary: string;
  items: string[];
  basis: string;
}

function deriveFindings(src: ReportSource, floors: FloorReport[], radios: Map<string, Radio>): Derived[] {
  const out: Derived[] = [];
  const apName = new Map(src.aps.map(({ ap }) => [ap.id, ap.name]));

  // 1. Areas below target
  {
    const items: string[] = [];
    for (const fr of floors)
      for (const m of fr.ssids) {
        if (m.passing == null || (m.passing >= 1 && m.belowPoints === 0)) continue;
        const worst = m.worst.map((w) => `point ${w.n} (${dbm(w.v)})`).join(", ");
        items.push(
          `${fr.f.building.name} / ${fr.f.floor.name}, ${ssidText(m.ssid)}: ≈ ${pct(1 - m.passing)} of the mapped area is below ${fr.target.level} dBm; ` +
            `${plural(m.belowPoints, "point")} below the target${worst ? `, weakest: ${worst}` : ""}.`,
        );
      }
    out.push({
      title: "Areas below the coverage target",
      summary: items.length ? `${plural(items.length, "floor/SSID combination")} with areas below the target.` : "Every mapped area reaches the target for the chosen SSIDs.",
      items: capped(items),
      basis:
        "the coverage grid (IDW estimate between measured points, up to the distance limit below) per floor and SSID, " +
        "and each point's strongest reading of that SSID (“not heard” counts as below). Target: the floor's requirements profile, else " +
        `${DEFAULT_TARGET_DBM} dBm. Floors without a plan or points are not mapped.`,
    });
  }

  // 2. Own APs sharing or overlapping a channel
  const own = [...radios.values()].filter((r) => r.apId != null);
  {
    // One entry per AP radio (several SSIDs on one radio share its channel by design).
    const byKey = new Map<string, Radio>();
    for (const r of own) byKey.set(`${r.apId}|${r.band}|${r.lo}|${r.hi}`, r);
    const entries = [...byKey.values()].sort((a, b) => a.lo - b.lo || a.apId! - b.apId!);
    const items: string[] = [];
    for (let i = 0; i < entries.length; i++)
      for (let j = i + 1; j < entries.length; j++) {
        const a = entries[i]!;
        const b = entries[j]!;
        if (a.apId === b.apId || a.band !== b.band || !overlaps(a, b)) continue;
        const same = a.lo === b.lo && a.hi === b.hi;
        items.push(
          `${apName.get(a.apId!)} (${channelText(a)}) and ${apName.get(b.apId!)} (${channelText(b)}): ${same ? "same channel" : "overlapping channels"} in ${BAND_LABEL[a.band]}.`,
        );
      }
    out.push({
      title: "Own access points sharing or overlapping a channel",
      summary: !own.length
        ? "No BSSID linked to a placed access point was heard in this scope, so the own channel plan is unknown."
        : items.length
          ? `${plural(items.length, "pair")} of own radios share or overlap a channel.`
          : "No two own access points overlap on a channel.",
      items: capped(items),
      basis:
        `the latest reading of each BSSID linked to a placed AP (${plural(own.length, "BSSID")} heard in scope); a radio occupies its channel centre ± width / 2. ` +
        "Where the OS/driver reported no width, only the 20 MHz primary channel is compared (said per radio), so a wider channel may overlap more than shown.",
    });
  }

  // 3. Neighbouring networks on the same (or overlapping) channels
  {
    const neighbours = [...radios.values()].filter((r) => r.apId == null && !r.markedOurs);
    const projectSsids = new Set(src.findings.projectSsids);
    const hits = neighbours
      .map((n) => ({ n, with: unique(own.filter((o) => o.band === n.band && overlaps(o, n)).map((o) => apName.get(o.apId!)!)) }))
      .filter((h) => h.with.length > 0)
      .sort((a, b) => (b.n.strongest ?? -999) - (a.n.strongest ?? -999));
    const items = hits.map(
      ({ n, with: w }) =>
        `${ssidText(n.ssid)} ${n.bssid} (${channelText(n)}): strongest ${dbm(n.strongest)}${n.strongestAt ? ` at ${n.strongestAt}` : ""}, heard at ${plural(n.points, "point")}; overlaps ${w.join(", ")}` +
        (n.ssid != null && projectSsids.has(n.ssid) ? ". Uses a project SSID: may be an own AP not yet linked (see the rogue findings)." : "."),
    );
    out.push({
      title: "Neighbouring networks on own channels",
      summary: !own.length
        ? "Own channels are unknown (no linked BSSID heard), so neighbours can't be compared with them."
        : items.length
          ? `${plural(items.length, "neighbouring BSSID")} on channels that overlap own access points.`
          : "No neighbouring network was heard on an own channel.",
      items: capped(items),
      basis:
        `${plural(neighbours.length, "BSSID")} heard in scope that are neither linked to a placed AP nor marked as ours; channel from the latest reading, ` +
        "level the strongest reading at any point. Overlap as above (centre ± width / 2, else the primary channel).",
    });
  }

  // 4. 2.4 GHz off 1 / 6 / 11
  {
    const off = [...radios.values()]
      .filter((r) => r.band === "2.4ghz" && r.channel != null && ![1, 6, 11].includes(r.channel))
      .sort((a, b) => (b.strongest ?? -999) - (a.strongest ?? -999));
    const items = off.map(
      (r) =>
        `${r.apId != null ? `Own (${apName.get(r.apId)})` : r.markedOurs ? "Own (marked, not placed)" : "Neighbour"}: ${ssidText(r.ssid)} ${r.bssid} on channel ${r.channel}, strongest ${dbm(r.strongest)}.`,
    );
    out.push({
      title: "2.4 GHz networks not on channel 1, 6 or 11",
      summary: items.length ? `${plural(items.length, "BSSID")} on 2.4 GHz channels that overlap two of the non-overlapping channels.` : "Every 2.4 GHz network heard is on channel 1, 6 or 11.",
      items: capped(items),
      basis: "the primary channel of every 2.4 GHz BSSID heard in scope, from its latest reading.",
    });
  }

  // 5. Dead zones
  {
    const items: string[] = [];
    for (const fr of floors) {
      const dead = fr.f.points.flatMap((p, i) => (p.samples.length === 0 ? [i + 1] : []));
      if (dead.length) items.push(`${fr.f.building.name} / ${fr.f.floor.name}: ${dead.length === 1 ? "point" : "points"} ${dead.join(", ")}.`);
    }
    out.push({
      title: "Points where nothing was heard",
      summary: items.length ? "At these points the scan heard no network at all." : "Every point heard at least one network.",
      items: capped(items),
      basis: "points whose scan stored no BSSID (Measure Here keeps only networks heard during that scan).",
    });
  }
  return out;
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

const STYLE = `
@page{size:A4;margin:14mm}
*{box-sizing:border-box}
body{font:10pt/1.4 Arial,Helvetica,sans-serif;color:#18212a;margin:0 auto;max-width:190mm;padding:8mm 0}
h1{font-size:24pt;margin:0 0 6pt}h2{font-size:15pt;margin:18pt 0 6pt;break-after:avoid}h3{font-size:11pt;margin:10pt 0 4pt;break-after:avoid}
p{margin:4pt 0}ul{margin:4pt 0;padding-left:16pt}
table{border-collapse:collapse;width:100%;margin:6pt 0;font-size:9pt}
th,td{border:1px solid #b9c1c9;padding:3pt 5pt;text-align:left;vertical-align:top}
th{font-weight:bold}
.cover{break-after:page}
.cover-head{display:flex;justify-content:space-between;align-items:flex-start;gap:12pt}
.logo{max-width:60mm;max-height:25mm}
dl{display:grid;grid-template-columns:max-content 1fr;gap:3pt 12pt;margin:12pt 0}dt{font-weight:bold}dd{margin:0}
.floor{break-before:page}
figure{margin:8pt 0;break-inside:avoid}
figcaption{margin-bottom:4pt}
svg.map{display:block;width:100%;height:auto;max-height:200mm;border:1px solid #b9c1c9}
.legend{display:flex;flex-wrap:wrap;gap:4pt 14pt;font-size:8.5pt;margin-top:3pt;align-items:center}
.legend svg{vertical-align:middle;margin-right:3pt}
.muted{color:#55606b}.small{font-size:8.5pt}
.finding{break-inside:avoid;margin:8pt 0}
.basis{font-size:8.5pt;color:#55606b}
.photos{display:flex;flex-wrap:wrap;gap:8pt}
.photo{width:58mm;margin:0;break-inside:avoid}.photo img{width:100%;height:auto;display:block}
.photo figcaption{font-size:8.5pt;margin-top:2pt}
.mono{font-family:"DejaVu Sans Mono",Consolas,monospace;font-size:8.5pt}
.warn{color:#a3420f}
`;

function swatch(color: string, shape: "rect" | "circle" = "rect"): string {
  return shape === "circle"
    ? `<svg width="10" height="10" aria-hidden="true"><circle cx="5" cy="5" r="4.5" fill="${esc(color)}" stroke="#333" stroke-width="0.5"/></svg>`
    : `<svg width="12" height="10" aria-hidden="true"><rect width="12" height="10" fill="${esc(color)}" stroke="#333" stroke-width="0.5"/></svg>`;
}

function signalLegend(): string {
  const n = SIGNAL_RAMP.length;
  const stops = SIGNAL_RAMP.map((c, i) => `<rect x="${(i * 120) / n}" width="${120 / n + 0.5}" height="10" fill="${c}"/>`).join("");
  return `<div class="legend"><span>${SIGNAL_DOMAIN[0]} dBm <svg width="120" height="10" aria-hidden="true">${stops}</svg> ${SIGNAL_DOMAIN[1]} dBm</span></div>`;
}

/** Markers in plan coordinates, sized relative to the plan. */
function markers(fr: FloorReport): string {
  const plan = fr.f.floor.plan!;
  const u = Math.max(plan.width, plan.height) / 110;
  const t = (n: number) => round(n, 1);
  const parts: string[] = [];
  for (const { ap, color } of fr.colored)
    if (ap.floorId === fr.f.floor.id)
      parts.push(
        `<g transform="translate(${t(ap.x)} ${t(ap.y)})"><rect x="${t(-u)}" y="${t(-u)}" width="${t(2 * u)}" height="${t(2 * u)}" fill="${esc(color)}" stroke="#fff" stroke-width="${t(u / 4)}"/>` +
          `<text y="${t(2.3 * u)}" text-anchor="middle" font-size="${t(1.1 * u)}" font-weight="bold" fill="#111" stroke="#fff" stroke-width="${t(u / 5)}" paint-order="stroke">${esc(ap.name)}</text></g>`,
      );
  fr.f.points.forEach((p, i) => {
    const dead = p.samples.length === 0;
    parts.push(
      `<g transform="translate(${t(p.x)} ${t(p.y)})"><circle r="${t(0.8 * u)}" fill="${dead ? STATUS_CRITICAL : "#1f2933"}" stroke="#fff" stroke-width="${t(u / 5)}"/>` +
        `<text y="${t(0.32 * u)}" text-anchor="middle" font-size="${t(0.85 * u)}" fill="#fff">${i + 1}</text></g>`,
    );
  });
  for (const pin of fr.f.annotations.pins)
    parts.push(
      `<path transform="translate(${t(pin.x)} ${t(pin.y)})" d="M0 ${t(-1.1 * u)}L${t(0.9 * u)} ${t(0.7 * u)}H${t(-0.9 * u)}Z" fill="#eda100" stroke="#fff" stroke-width="${t(u / 6)}"/>`,
    );
  return parts.join("");
}

const MARKER_LEGEND =
  `<span>${swatch("#1f2933", "circle")}Survey point (number)</span><span>${swatch(STATUS_CRITICAL, "circle")}Nothing heard</span>` +
  `<span><svg width="10" height="10" aria-hidden="true"><path d="M5 0L10 10H0Z" fill="#eda100"/></svg>Note pin</span><span>${swatch("#2a78d6")}Access point (colour per AP)</span>`;

function mapFigure(fr: FloorReport, title: string, caption: string, heat: string | null, legend: string): string {
  const plan = fr.f.floor.plan!;
  const planRef = fr.f.planData ? `<use href="#plan-${fr.f.floor.id}"/>` : "";
  const heatImg = heat ? `<image href="${esc(heat)}" width="${plan.width}" height="${plan.height}" preserveAspectRatio="none"/>` : "";
  return (
    `<figure><figcaption><b>${esc(title)}.</b> ${caption}</figcaption>` +
    `<svg class="map" viewBox="0 0 ${plan.width} ${plan.height}" role="img" aria-label="${esc(title)}">${planRef}${heatImg}${markers(fr)}</svg>` +
    `<div class="legend">${legend}${MARKER_LEGEND}</div></figure>`
  );
}

function limitText(fr: FloorReport): string {
  return fr.ppm ? `${INFLUENCE_M} m` : "15 % of the plan's longest side (the plan has no scale)";
}

function floorSection(src: ReportSource, fr: FloorReport): string {
  const { f } = fr;
  const plan = f.floor.plan;
  const out: string[] = [];
  out.push(`<section class="floor"><h2>${esc(f.building.name)} / ${esc(f.floor.name)}</h2>`);
  const dates = unique(f.points.map((p) => p.measuredAt.slice(0, 10))).sort();
  const pctOnly = f.points.filter((p) => p.samples.length > 0 && p.samples.every((s) => s.signal.dbm == null)).length;
  out.push(
    `<p>${plural(f.points.length, "survey point")}${dates.length ? `, measured ${esc(dates.join(", "))}` : ""}. ` +
      `${fr.ppm ? `Scale: ${round(fr.ppm, 1)} px/m.` : "The plan has no scale, so distances are not in metres."} ` +
      `Coverage target: <b>${fr.target.level} dBm</b> (${esc(fr.target.source)}).` +
      (pctOnly ? ` <span class="warn">${plural(pctOnly, "point")} only reported a quality %, not dBm, and ${pctOnly === 1 ? "is" : "are"} left out of the maps.</span>` : "") +
      `</p>`,
  );
  if (!plan) out.push(`<p class="muted">This floor has no plan, so there are no maps.</p>`);
  else if (!f.points.length) out.push(`<p class="muted">No points were measured on this floor.</p>`);
  else {
    if (f.planData)
      out.push(
        `<svg width="0" height="0" style="position:absolute" aria-hidden="true"><defs><image id="plan-${f.floor.id}" href="${esc(f.planData)}" width="${plan.width}" height="${plan.height}"/></defs></svg>`,
      );
    const estimate = `IDW estimate between ${plural(fr.usedPoints, "point")} with dBm readings, drawn up to ${limitText(fr)} from a point; walls are not modelled.`;
    for (const m of fr.ssids) {
      if (m.heardAt === 0) {
        out.push(`<h3>${esc(ssidText(m.ssid))}</h3><p>Not heard at any of this floor's points with dBm readings.</p>`);
        continue;
      }
      out.push(`<h3>${esc(ssidText(m.ssid))}</h3>`);
      out.push(
        mapFigure(fr, `Signal: ${ssidText(m.ssid)}`, `Strongest BSSID of the SSID at each spot. ${estimate}`, m.signal, signalLegend()),
      );
      const share = m.passing == null ? "not evaluated" : pct(m.passing);
      out.push(
        mapFigure(
          fr,
          `Coverage at ${fr.target.level} dBm: ${ssidText(m.ssid)}`,
          `<b>${share} of the mapped area reaches ${fr.target.level} dBm</b> (estimate; ${plural(m.heardAt, "point")} heard the SSID). ${estimate}`,
          m.coverage,
          `<span>${swatch(STATUS_GOOD)}≥ ${fr.target.level} dBm</span><span>${swatch(STATUS_CRITICAL)}below</span>`,
        ),
      );
    }
    if (fr.serving) {
      const g = fr.serving.grid;
      const legend =
        fr.serving.aps.map(({ ap, color }) => `<span>${swatch(color)}${esc(ap.name)} ${g.shares.has(ap.id) ? pct(g.shares.get(ap.id)!) : "0 %"}</span>`).join("") +
        `<span>${swatch(NO_SERVICE_COLOR)}no AP at ${fr.serving.level} dBm: ${pct(g.unserved)}</span>`;
      out.push(
        mapFigure(
          fr,
          `Serving access point at ${fr.serving.level} dBm`,
          `The placed AP (all its linked BSSIDs) estimated strongest at each spot; shares are of the mapped area. ${estimate}`,
          fr.serving.map,
          legend,
        ),
      );
    }
    const req = f.requirements;
    if (req?.profile && fr.requirements) {
      out.push(
        mapFigure(
          fr,
          `Requirements: ${req.profile.name}`,
          `${fr.requirements.passing == null ? "Not evaluated" : `≈ ${pct(fr.requirements.passing)} of the mapped area meets the profile`} (estimate). ${estimate}`,
          fr.requirements.map,
          `<span>${swatch(STATUS_GOOD)}meets</span><span>${swatch(STATUS_CRITICAL)}fails</span>`,
        ),
      );
    }
  }

  // Notes, pins, photos
  const a = f.annotations;
  const notes: string[] = [];
  if (a.notes) notes.push(`<p>${escLines(a.notes)}</p>`);
  const pointNo = new Map(f.points.map((p, i) => [p.id, i + 1]));
  const items = [
    ...a.pointNotes.map((n) => `<li><b>Point ${pointNo.get(n.id) ?? "?"}:</b> ${escLines(n.notes)}</li>`),
    ...a.apNotes.map((n) => `<li><b>${esc(n.name ?? "AP")}:</b> ${escLines(n.notes)}</li>`),
    ...a.pins.map((p) => `<li><b>Pin${p.category ? ` (${esc(p.category)})` : ""}:</b> ${escLines(p.text)}</li>`),
  ];
  if (items.length) notes.push(`<ul>${items.join("")}</ul>`);
  const photos = a.photos
    .filter((p) => f.photoData.has(p.id))
    .map((p) => {
      const on =
        p.target.kind === "point" ? `point ${pointNo.get(p.target.id) ?? "?"}` : p.target.kind === "ap" ? esc(src.aps.find((x) => x.ap.id === p.target.id)?.ap.name ?? "an AP") : p.target.kind === "pin" ? "a note pin" : "the floor";
      return `<figure class="photo"><img src="${esc(f.photoData.get(p.id))}" alt="${esc(p.caption ?? "Photo")}"><figcaption>${esc(p.caption ?? "")}${p.caption ? " · " : ""}<span class="muted">on ${on}${p.takenAt ? `, taken ${esc(p.takenAt.slice(0, 16).replace("T", " "))}` : ""}</span></figcaption></figure>`;
    });
  if (photos.length) notes.push(`<div class="photos">${photos.join("")}</div>`);
  if (notes.length) out.push(`<h3>Notes and photos</h3>${notes.join("")}`);
  out.push(`</section>`);
  return out.join("");
}

function apTable(src: ReportSource, radios: Map<string, Radio>): string {
  const buildings = new Set(src.floors.map((f) => f.building.id));
  const aps = src.aps.filter((a) => buildings.has(a.buildingId)).sort((a, b) => a.ap.id - b.ap.id);
  if (!aps.length) return `<h2>Access points</h2><p class="muted">No access points are placed in this scope.</p>`;
  const rows = aps.map(({ ap, floorName }) => {
    const heard = ap.bssids.map((b) => radios.get(b)).filter((r): r is Radio => !!r);
    const list = (xs: string[]) => esc(unique(xs).join(", ")) || "—";
    return (
      `<tr><td>${esc(ap.name)}</td><td>${esc(ap.model ?? "—")}</td><td>${esc(floorName)}</td>` +
      `<td class="mono">${ap.bssids.map(esc).join("<br>") || "—"}</td>` +
      `<td>${list(heard.map((r) => ssidText(r.ssid)))}</td>` +
      `<td>${list(heard.map((r) => (r.channel != null ? String(r.channel) : `${r.frequencyMhz} MHz`)))}</td>` +
      `<td>${list(heard.map((r) => BAND_LABEL[r.band]))}</td>` +
      `<td>${list(heard.map((r) => (r.widthKnown ? `${r.widthMhz} MHz` : "not reported")))}</td>` +
      `<td>${ap.notes ? escLines(ap.notes) : ""}</td></tr>`
    );
  });
  return (
    `<h2>Access points</h2><p class="small muted">SSIDs, channels, bands and widths are from the linked BSSIDs' latest readings in this report's scope; a linked BSSID not heard here adds nothing.</p>` +
    `<table><tr><th>Name</th><th>Model</th><th>Floor</th><th>Linked BSSIDs</th><th>SSIDs</th><th>Channels</th><th>Bands</th><th>Widths</th><th>Notes</th></tr>${rows.join("")}</table>`
  );
}

function requirementsSection(floors: FloorReport[]): string {
  const withProfile = floors.filter((fr) => fr.f.requirements?.profile);
  if (!withProfile.length) return "";
  const profiles = new Map<number, RequirementProfile>();
  for (const fr of withProfile) profiles.set(fr.f.requirements!.profile!.id, fr.f.requirements!.profile!);
  const defs = [...profiles.values()].map(
    (p) => `<li><b>${esc(p.name)}</b>: ${describeValues(p).map(esc).join(" · ")}${p.targets.length ? `. Applies to ${p.targets.map((t) => esc(t.label)).join(", ")}` : ""}.</li>`,
  );
  const rows = withProfile.map((fr) => {
    const r = fr.f.requirements!;
    const s = r.summary;
    const fails = s?.failuresByRule.map((c) => `${esc(c.band ? `${BAND_LABEL[c.band]} required` : RULE_LABEL[c.rule])}: ${c.count}`).join(", ");
    return (
      `<tr><td>${esc(fr.f.building.name)} / ${esc(fr.f.floor.name)}</td><td>${esc(r.profile!.name)}${r.source === "floor_override" ? " (floor setting)" : ""}</td>` +
      `<td>${s?.passed ?? 0}</td><td>${s?.failed ?? 0}</td><td>${s?.notEvaluated ?? 0}</td>` +
      `<td>${fr.requirements?.passing == null ? "—" : `≈ ${pct(fr.requirements.passing)}`}</td><td>${fails || "—"}</td></tr>`
    );
  });
  const heuristic = withProfile.some((fr) => fr.f.requirements?.summary?.usesHeuristic);
  return (
    `<h2>Requirements</h2><p>Profile(s) as they were when this report was made:</p><ul>${defs.join("")}</ul>` +
    `<table><tr><th>Floor</th><th>Profile</th><th>Points pass</th><th>Points fail</th><th>Not evaluated</th><th>Mapped area meeting it</th><th>Failures by rule (points)</th></tr>${rows.join("")}</table>` +
    `<p class="small muted">Point results are judged from each point's own readings; the area share is an IDW estimate.` +
    (heuristic ? " Some second-AP / co-channel checks told physical APs apart by the BSSID heuristic, because not every AP is placed." : "") +
    `</p>`
  );
}

function rogueSection(src: ReportSource): string {
  const fs = src.findings.findings;
  const items = fs.map((f) => {
    const strongest = f.heard[0];
    const where = unique(f.heard.map((h) => `${h.buildingName} / ${h.floorName}`)).join(", ");
    return (
      `<li class="finding"><b>${esc(f.title)}</b> <span class="muted">(${esc(f.severity)})</span>: ${esc(f.explanation)} ` +
      `<span class="basis">Based on ${plural(f.heard.length, "stored reading")}${strongest?.signal.dbm != null ? `, strongest ${dbm(strongest.signal.dbm)}` : ""}${where ? `, on ${esc(where)}` : ""}.</span></li>`
    );
  });
  return (
    `<h2>Rogue and unknown-transmitter findings</h2>` +
    `<p class="small muted">Project SSIDs: ${src.findings.projectSsids.map((s) => esc(ssidText(s))).join(", ") || "none (no placed AP has linked BSSIDs)"}. ` +
    `Based on ${plural(src.findings.points, "point")} in scope and ${plural(src.findings.linkedBssids, "linked BSSID")}.</p>` +
    `<ul>${items.join("") || "<li>None found in the stored readings.</li>"}</ul>` +
    (src.findings.limits.length ? `<p class="small muted">${src.findings.limits.map(esc).join(" ")}</p>` : "")
  );
}

/** Which optional fields the adapters/OS never reported, from the samples themselves. */
function unreportedFields(floors: ReportFloor[]): string[] {
  const samples = floors.flatMap((f) => f.points.flatMap((p) => p.samples));
  if (!samples.length) return [];
  const missing = (label: string, has: (s: Sample) => boolean) => {
    const n = samples.filter((s) => !has(s)).length;
    return n === 0 ? null : n === samples.length ? `${label}: never` : `${label}: missing in ${n} of ${samples.length} readings`;
  };
  return [
    missing("dBm signal", (s) => s.signal.dbm != null),
    missing("channel width", (s) => s.channelWidthMhz != null),
    missing("noise floor / SNR", (s) => s.noiseDbm != null || s.snrDb != null),
    missing("channel load (BSS Load)", (s) => s.channelUtilizationPct != null),
  ].filter((x): x is string => x != null);
}

function adaptersOf(floors: ReportFloor[]): string[] {
  return unique(
    floors.flatMap((f) =>
      f.points.map((p) => {
        const a = p.adapter;
        const iface = a.id.includes(":") ? a.id.slice(a.id.indexOf(":") + 1) : a.id;
        return `${a.model ?? "model not reported"} (${iface}${a.driver ? `, driver ${a.driver}` : ""})`;
      }),
    ),
  );
}

function coverPage(src: ReportSource, floors: FloorReport[]): string {
  const b = src.branding;
  const all = src.floors.flatMap((f) => f.points);
  const dates = unique(all.map((p) => p.measuredAt.slice(0, 10))).sort();
  const providers = unique(all.map((p) => p.adapter.provider)).map((id) => PROVIDER_LABEL[id] ?? `provider “${id}”`);
  const buildings = unique(src.floors.map((f) => f.building.name));
  const scope =
    src.scope.kind === "project" ? "Whole project" : src.scope.kind === "building" ? `Building ${buildings[0] ?? ""}` : `One floor`;
  const row = (k: string, v: string) => `<dt>${esc(k)}</dt><dd>${v}</dd>`;
  const summary = floors.map((fr) => {
    const cov = fr.ssids.map((m) => `${esc(ssidText(m.ssid))}: ${m.passing == null ? (m.heardAt ? "—" : "not heard") : pct(m.passing)}`).join("; ");
    const s = fr.f.requirements?.summary;
    return `<tr><td>${esc(fr.f.building.name)} / ${esc(fr.f.floor.name)}</td><td>${fr.f.points.length}</td><td>${cov || "—"}</td><td>${s ? `${s.passed} / ${s.points}` : "—"}</td></tr>`;
  });
  return (
    `<section class="cover"><div class="cover-head"><div><h1>Wi-Fi site survey</h1><p>${esc(src.project.name)}</p></div>` +
    (b.logoData ? `<img class="logo" src="${esc(b.logoData)}" alt="${esc(b.companyName ?? "Logo")}">` : "") +
    `</div><dl>` +
    row("Customer", esc(src.project.customerName ?? "—")) +
    row("Scope", `${esc(scope)}: ${src.floors.map((f) => `${esc(f.building.name)} / ${esc(f.floor.name)}`).join(", ")}`) +
    row("Survey dates", esc(dates.length ? (dates.length > 4 ? `${dates[0]} – ${dates[dates.length - 1]} (${dates.length} days)` : dates.join(", ")) : "no points measured")) +
    row("Survey points", String(all.length)) +
    row("Adapters", esc(adaptersOf(src.floors).join("; ") || "—")) +
    row("OS / provider", esc(providers.join("; ") || "—")) +
    row("SSIDs mapped", src.ssids.map((s) => esc(ssidText(s))).join(", ") || "—") +
    (b.technicianName ? row("Technician", esc(b.technicianName)) : "") +
    (b.companyName ? row("Company", esc(b.companyName)) : "") +
    row("Report", `${esc(src.generatedAt.toISOString().slice(0, 16).replace("T", " "))} UTC · Fresnel ${esc(src.version)} · offline`) +
    `</dl><h3>Summary</h3><table><tr><th>Floor</th><th>Points</th><th>Mapped area reaching the target</th><th>Points meeting requirements</th></tr>${summary.join("")}</table>` +
    `<p class="small muted">Area shares are estimates interpolated between measured points (see Caveats). Point counts are measurements.</p></section>`
  );
}

function caveats(src: ReportSource, floors: FloorReport[]): string {
  const unscaled = floors.filter((fr) => fr.f.floor.plan && !fr.ppm).map((fr) => `${fr.f.building.name} / ${fr.f.floor.name}`);
  const adapters = adaptersOf(src.floors);
  const fields = unreportedFields(src.floors);
  return (
    `<h2>Caveats</h2><ul>` +
    `<li>Maps are estimates: inverse-distance weighting (IDW) between measured points. Walls, doors and other obstacles are not modelled, so an estimate can be wrong between two points on either side of a wall.</li>` +
    `<li>Nothing is drawn further than ${INFLUENCE_M} m from a measured point${unscaled.length ? ` (on plans without a scale: 15 % of the plan's longest side: ${esc(unscaled.join(", "))})` : ""}; unmapped areas are not covered or uncovered, just unknown.</li>` +
    `<li>Only dBm readings are mapped; a quality % is never converted to dBm. Where a network was not heard at a point, it counts as ${NOT_HEARD_DBM} dBm there.</li>` +
    `<li>Wi-Fi cards read differently (antennas, drivers): readings are from ${esc(adapters.join("; ") || "—")} and can differ by several dB from a client device.</li>` +
    (fields.length ? `<li>Not reported by the OS/driver: ${esc(fields.join("; "))}. Those fields are left out, not estimated.</li>` : "") +
    `<li>Channel overlap uses the channel centre and width where reported, else only the 20 MHz primary channel (said where it applies).</li>` +
    `</ul>`
  );
}

/** Build the whole report. */
export async function buildReport(src: ReportSource, deps: ReportDeps): Promise<string> {
  const total = stepsFor(src);
  let done = 0;
  const tick = (what: string) => deps.onProgress?.(++done, total, what);
  const floors: FloorReport[] = [];
  for (const f of src.floors) floors.push(await floorMaps(src, f, deps, tick));
  const radios = collectRadios(src);
  const derived = deriveFindings(src, floors, radios);

  const findings = derived
    .map(
      (d) =>
        `<div class="finding"><h3>${esc(d.title)}</h3><p>${esc(d.summary)}</p>${d.items.length ? `<ul>${d.items.map((i) => `<li>${esc(i)}</li>`).join("")}</ul>` : ""}<p class="basis">Based on ${esc(d.basis)}</p></div>`,
    )
    .join("");
  const title = `${src.project.name}: Wi-Fi site survey`;
  return (
    `<!doctype html><html lang="en"><head><meta charset="utf-8">` +
    `<meta http-equiv="Content-Security-Policy" content="${REPORT_CSP}">` +
    `<meta name="viewport" content="width=device-width, initial-scale=1"><meta name="generator" content="Fresnel ${esc(src.version)}">` +
    `<title>${esc(title)}</title><style>${STYLE}</style></head><body>` +
    coverPage(src, floors) +
    `<h2>Findings</h2><p class="small muted">Derived from the stored readings; each says what it is based on.</p>${findings}` +
    rogueSection(src) +
    apTable(src, radios) +
    requirementsSection(floors) +
    caveats(src, floors) +
    floors.map((fr) => floorSection(src, fr)).join("") +
    `</body></html>`
  );
}

/** The SSIDs to map by default: those broadcast by BSSIDs linked to placed APs, most widely heard first. */
export function defaultSsids(points: SurveyPoint[], aps: PlacedAp[]): string[] {
  const linked = new Set(aps.flatMap((a) => a.bssids));
  const count = new Map<string, number>();
  for (const p of points)
    for (const ssid of unique(p.samples.filter((s) => linked.has(s.bssid) && s.ssid).map((s) => s.ssid!)))
      count.set(ssid, (count.get(ssid) ?? 0) + 1);
  return [...count.entries()].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0])).map(([s]) => s);
}
