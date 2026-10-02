#!/usr/bin/env node
// Heatmap grid benchmark: every grid the app builds (signal, coverage,
// serving AP, requirements), with the spatial index off (each cell scans
// every point, as before the index existed) and on. Checks that both give
// identical grids, cell by cell, and reports the timings.
//
// Usage: node scripts/bench-heatmap.mjs [points...]   (default: 1000 3000)
// Exits non-zero if the outputs differ. Budget: 1,000 points < 200 ms.

import { loadTs } from "./load-ts.mjs";

const h = await loadTs(["src/lib/heatmap.ts", "src/lib/requirements.ts", "src/lib/heatmapJobs.ts"]);

const PLAN = { file: "bench.png", mime: "image/png", width: 10_000, height: 10_000 };
const PPM = 100; // 100 m × 100 m
const BUDGET_MS = 200;
const RUNS = 3;

/** Deterministic PRNG (mulberry32), so every run measures the same layout. */
function rng(seed) {
  return () => {
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function fixture(n) {
  const rand = rng(n);
  const aps = Array.from({ length: 8 }, (_, i) => ({
    id: i + 1,
    floorId: 1,
    name: `AP ${i + 1}`,
    x: rand() * PLAN.width,
    y: rand() * PLAN.height,
    model: null,
    notes: null,
    bssids: [`02:00:00:00:00:${String(i + 1).padStart(2, "0")}`],
    createdAt: "",
    updatedAt: "",
  }));
  const points = Array.from({ length: n }, (_, i) => {
    const x = rand() * PLAN.width;
    const y = rand() * PLAN.height;
    const samples = aps
      .map((ap) => ({ ap, d: Math.hypot(ap.x - x, ap.y - y) / PPM }))
      .filter(({ d }) => d < 45)
      .map(({ ap, d }) => ({
        bssid: ap.bssids[0],
        ssid: "Corp",
        ssidRaw: [],
        frequencyMhz: 5180,
        channel: 36,
        band: "5ghz",
        channelWidthMhz: 80,
        channelCenterMhz: 5210,
        signal: { dbm: Math.round(-35 - 20 * Math.log10(1 + d) - rand() * 6), qualityPercent: null },
        security: "wpa2_personal",
        phyType: null,
        wifiGeneration: null,
        noiseDbm: null,
        snrDb: null,
        channelUtilizationPct: null,
        stationCount: null,
        lastSeenAgeMs: 0,
        isConnected: false,
        detail: null,
      }));
    return {
      id: i + 1,
      floorId: 1,
      x,
      y,
      measuredAt: "",
      scanDurationMs: 0,
      adapter: { id: "fake:wlan0", provider: "fake", model: null, driver: null, hwId: null },
      adapterBands: null,
      samples,
    };
  });
  return { aps, points };
}

function jobs({ aps, points }) {
  const signal = { metric: "signal", network: { kind: "ssid", ssid: "Corp" }, band: "all", threshold: -67 };
  const pts = points.map((p) => ({ x: p.x, y: p.y, v: h.pointValue(p, signal) }));
  const colored = aps.map((ap, i) => ({ ap, color: h.apColor(i) }));
  const profile = { primaryMinDbm: -67, secondaryMinDbm: -72, cochannelMax: 2, cochannelLevelDbm: -82, requiredBands: [], minSnrDb: null, maxUtilPct: null };
  const evals = points.map((p, i) => ({
    pointId: p.id,
    outcome: "pass",
    primaryDbm: pts[i].v,
    secondaryDbm: pts[i].v - 6,
    cochannelCount: i % 4,
    rules: [],
  }));
  return [
    ["signal", { kind: "grid", plan: PLAN, ppm: PPM, pts, cfg: signal }],
    ["coverage", { kind: "grid", plan: PLAN, ppm: PPM, pts, cfg: { ...signal, metric: "coverage" } }],
    ["serving (8 APs)", { kind: "serving", plan: PLAN, ppm: PPM, inputs: h.servingInputs(points, colored, "all"), threshold: -67 }],
    ["requirements", { kind: "requirements", plan: PLAN, ppm: PPM, inputs: h.areaInputs(points, evals, profile) }],
  ];
}

function time(job) {
  let result = null;
  const ms = [];
  for (let i = 0; i < RUNS; i++) {
    const t = performance.now();
    result = h.runHeatJob(job);
    ms.push(performance.now() - t);
  }
  ms.sort((a, b) => a - b);
  return { result, ms: ms[Math.floor(RUNS / 2)] };
}

function same(a, b) {
  if (a === null || b === null) return a === b;
  if (a.cols !== b.cols || a.rows !== b.rows || a.passingFraction !== b.passingFraction) return false;
  if (a.rgba.length !== b.rgba.length) return false;
  for (let i = 0; i < a.rgba.length; i++) if (a.rgba[i] !== b.rgba[i]) return false;
  if ("shares" in a) {
    if (a.unserved !== b.unserved || a.shares.size !== b.shares.size) return false;
    for (const [k, v] of a.shares) if (b.shares.get(k) !== v) return false;
  }
  return true;
}

const counts = process.argv.slice(2).map(Number).filter((n) => n > 0);
let failed = false;
console.log(`Plan ${PLAN.width} × ${PLAN.height} px at ${PPM} px/m; median of ${RUNS} runs.\n`);
console.log("points  grid              cells     before (ms)  after (ms)  identical");
for (const n of counts.length ? counts : [1000, 3000]) {
  for (const [name, job] of jobs(fixture(n))) {
    h.setSpatialIndexing(false);
    const before = time(job);
    h.setSpatialIndexing(true);
    const after = time(job);
    const ok = same(before.result, after.result);
    failed ||= !ok;
    const cells = before.result ? before.result.cols * before.result.rows : 0;
    console.log(
      `${String(n).padStart(6)}  ${name.padEnd(16)}  ${String(cells).padStart(7)}  ${before.ms.toFixed(1).padStart(11)}  ${after.ms.toFixed(1).padStart(10)}  ${ok ? "yes" : "NO"}`,
    );
    if (n === 1000 && name === "signal" && after.ms >= BUDGET_MS) console.log(`        over the ${BUDGET_MS} ms budget`);
  }
}
if (failed) {
  console.error("\nThe indexed grids differ from the full scan.");
  process.exit(1);
}
