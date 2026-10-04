#!/usr/bin/env node
// Report safety test: builds a report for a fixture project (two floors made
// from the recorded scans in crates/fresnel-core/src/adapters/fixtures, plus
// hostile SSIDs, names and notes) with the real report code, then checks
// that the HTML is inert and that untrusted text is escaped.
//
// Usage: npm run test:report [-- --out report.html]   (--out keeps the HTML to look at)

import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { deflateSync } from "node:zlib";
import { loadTs, root } from "./load-ts.mjs";

const r = await loadTs(["src/lib/report.ts", "src/lib/heatmapJobs.ts"]);

// --- A real PNG encoder (the app uses a canvas) ------------------------------

const CRC = new Int32Array(256).map((_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c;
});
function crc32(buf) {
  let c = -1;
  for (const b of buf) c = CRC[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ -1) >>> 0;
}
function chunk(type, data) {
  const out = Buffer.alloc(12 + data.length);
  out.writeUInt32BE(data.length, 0);
  out.write(type, 4, "ascii");
  data.copy(out, 8);
  out.writeUInt32BE(crc32(out.subarray(4, 8 + data.length)), 8 + data.length);
  return out;
}
function png(width, height, rgba) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr.set([8, 6, 0, 0, 0], 8);
  const raw = Buffer.alloc((width * 4 + 1) * height);
  for (let y = 0; y < height; y++) Buffer.from(rgba.buffer, rgba.byteOffset + y * width * 4, width * 4).copy(raw, y * (width * 4 + 1) + 1);
  const bytes = Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), chunk("IHDR", ihdr), chunk("IDAT", deflateSync(raw)), chunk("IEND", Buffer.alloc(0))]);
  return `data:image/png;base64,${bytes.toString("base64")}`;
}

// --- Fixture --------------------------------------------------------------------

const HOSTILE_SSID = `<script>alert("ssid")</script>`;
const URL_SSID = `"><img src=https://evil.example/x.png onerror=alert(1)>`;
const FORMULA_SSID = `=HYPERLINK("https://evil.example","click")`;
const fixture = (name) => JSON.parse(readFileSync(join(root, "crates/fresnel-core/src/adapters/fixtures", `${name}.json`), "utf8"));

function sample(ap, overrides = {}) {
  return {
    bssid: ap.bssid,
    ssid: ap.ssid,
    ssidRaw: ap.ssidRaw,
    frequencyMhz: ap.frequencyMhz,
    channel: ap.channel,
    band: ap.band,
    channelWidthMhz: ap.channelWidthMhz ?? null,
    channelCenterMhz: ap.channelCenterMhz ?? null,
    signal: { ...ap.signal },
    security: ap.security.kind,
    phyType: ap.phyType ?? null,
    wifiGeneration: ap.wifiGeneration ?? null,
    noiseDbm: ap.noiseDbm ?? null,
    snrDb: ap.snrDb ?? null,
    channelUtilizationPct: ap.channelUtilizationPct ?? null,
    stationCount: ap.stationCount ?? null,
    lastSeenAgeMs: ap.lastSeenAgeMs ?? null,
    isConnected: ap.isConnected,
    detail: null,
    ...overrides,
  };
}

const PLAN = { file: "plan-test.png", mime: "image/png", width: 1600, height: 1000 };
const SCALE = { x1: 0, y1: 0, x2: 1000, y2: 0, lengthM: 20 }; // 50 px/m
let nextId = 1;

function floorFrom(scanName, floorId, extra) {
  const scan = fixture(scanName);
  const points = [];
  for (let gy = 0; gy < 3; gy++)
    for (let gx = 0; gx < 4; gx++) {
      const x = 200 + gx * 400;
      const y = 200 + gy * 300;
      // Weaker further from the plan's top-left corner, deterministically.
      const loss = Math.round(gx * 6 + gy * 5);
      const samples = scan.accessPoints.map((ap) => sample(ap, { signal: { dbm: ap.signal.dbm - loss, qualityPercent: ap.signal.qualityPercent } }));
      points.push({
        id: nextId++,
        floorId,
        x,
        y,
        measuredAt: `2026-09-${28 + (floorId - 1)}T09:${String(10 + points.length).padStart(2, "0")}:00Z`,
        scanDurationMs: 3150,
        adapter: { id: scan.adapterId, provider: scan.provider, model: "Intel Wi-Fi 6E AX211", driver: "iwlwifi", hwId: "8086:51f1" },
        adapterBands: null,
        samples,
      });
    }
  extra(points, scan);
  return points;
}

const floor1Points = floorFrom("office", 1, (points, scan) => {
  const corp5 = scan.accessPoints[0];
  const printer = scan.accessPoints[5];
  // Hostile neighbours: on AP 1's 80 MHz channel (width unknown), on 2.4 GHz channel 3, a CSV formula.
  points[1].samples.push(sample(corp5, { bssid: "66:66:66:00:00:01", ssid: HOSTILE_SSID, channel: 40, frequencyMhz: 5200, channelWidthMhz: null, channelCenterMhz: null, signal: { dbm: -63, qualityPercent: null } }));
  points[2].samples.push(sample(printer, { bssid: "66:66:66:00:00:02", ssid: URL_SSID, channel: 3, frequencyMhz: 2422, channelCenterMhz: 2422, signal: { dbm: -70, qualityPercent: null } }));
  points[3].samples.push(sample(printer, { bssid: "66:66:66:00:00:03", ssid: FORMULA_SSID, signal: { dbm: -77, qualityPercent: null } }));
  // An own AP on channel 44 / 20 MHz: inside AP 1's 80 MHz channel.
  for (const p of points.slice(4, 8)) p.samples.push(sample(corp5, { bssid: "F0:9F:C2:7A:55:B1", channel: 44, frequencyMhz: 5220, channelWidthMhz: 20, channelCenterMhz: 5220, signal: { dbm: -60, qualityPercent: null } }));
  // A dead zone.
  points[11].samples = [];
});
const floor2Points = floorFrom("stale", 2, () => {});

const now = "2026-10-01T12:00:00Z";
const project = { id: 1, name: `Acme <b>HQ</b>`, customerName: `Acme & Sons <script>x</script>`, createdAt: now, updatedAt: now };
const building = { id: 1, projectId: 1, name: "Main building", createdAt: now, updatedAt: now };
const floors = [
  { id: 1, buildingId: 1, name: "Ground floor", level: 0, plan: PLAN, scale: SCALE, pointCount: floor1Points.length, createdAt: now, updatedAt: now },
  { id: 2, buildingId: 1, name: `First <i>floor</i>`, level: 1, plan: PLAN, scale: null, pointCount: floor2Points.length, createdAt: now, updatedAt: now },
];
const ap = (id, floorId, name, x, y, bssids, notes = null) => ({ id, floorId, name, x, y, model: "Vendor AP-650", notes, bssids, createdAt: now, updatedAt: now });
const aps = [
  ap(1, 1, "AP 1", 250, 220, ["F0:9F:C2:7A:10:20", "F0:9F:C2:7A:10:21", "F2:9F:C2:7A:10:21"], `Ceiling <script>alert("ap")</script>`),
  ap(2, 1, "AP 2", 1350, 800, ["F0:9F:C2:7A:44:A0", "F0:9F:C2:7A:44:A1"]),
  ap(3, 1, `AP <img src=x onerror=alert(3)>`, 800, 500, ["F0:9F:C2:7A:55:B1"]),
];

const profile = {
  id: 1, projectId: 1, name: `Office <script>`, preset: "office_data", isDefault: true, targets: [],
  primaryMinDbm: -67, secondaryMinDbm: null, cochannelMax: null, cochannelLevelDbm: null, requiredBands: [], minSnrDb: null, maxUtilPct: null,
  createdAt: now, updatedAt: now,
};
const evals = floor1Points.map((p) => {
  const best = Math.max(-100, ...p.samples.filter((s) => s.ssid === "Corp").map((s) => s.signal.dbm));
  return { pointId: p.id, outcome: best >= -67 ? "pass" : "fail", reason: null, rules: [], primaryBssid: null, primaryDbm: best, secondaryDbm: null, cochannelCount: null, widthUnknown: false };
});
const requirements = {
  floorId: 1, projectId: 1, overrideProfileId: null, profile, source: "project_default", points: evals,
  summary: { points: evals.length, passed: evals.filter((e) => e.outcome === "pass").length, failed: evals.filter((e) => e.outcome === "fail").length, notEvaluated: 0, passFractionOfPoints: null, failuresByRule: [{ rule: "primary_signal", band: null, count: 1 }], rulesNotEvaluated: [], notEvaluatedReasons: [], adapters: [], usesHeuristic: false, widthUnknownPoints: 0 },
};

const tiny = png(2, 2, new Uint8Array(16).fill(200));
const annotations = (floorId, points) => ({
  floorId,
  notes: `Floor note with a link https://evil.example and <script>alert("note")</script>\nsecond line`,
  pointNotes: [{ id: points[0].id, name: null, notes: `<iframe src="//evil.example">` }],
  apNotes: floorId === 1 ? [{ id: 1, name: "AP 1", notes: "<b>bold?</b>" }] : [],
  pins: [{ id: floorId, floorId, x: 600, y: 400, text: `<svg onload=alert(1)>`, category: "obstruction", createdAt: now, updatedAt: now }],
  photos: [{ id: floorId, target: { kind: "point", id: points[0].id }, reportFile: "photo-x-report.jpg", caption: `<script>caption</script>`, takenAt: "2026-09-28T09:00:00" }],
});

const findings = {
  projectId: 1, scope: { kind: "project", id: 1 }, generatedAt: now, points: floor1Points.length + floor2Points.length, linkedBssids: 6,
  projectSsids: ["Corp", "Corp-Guest"],
  findings: [{
    key: "k", kind: "unknown_transmitter", severity: "warning", title: `Unknown transmitter ${HOSTILE_SSID}`, explanation: "Uses a project SSID.",
    bssid: "66:66:66:00:00:01", ssid: HOSTILE_SSID, band: "5ghz", ownership: "unknown", ap: null,
    heard: [{ pointId: floor1Points[1].id, floorId: 1, floorName: "Ground floor", buildingName: "Main building", pointNumber: 2, x: 0, y: 0, measuredAt: now, bssid: "66:66:66:00:00:01", ssid: HOSTILE_SSID, frequencyMhz: 5200, channel: 40, band: "5ghz", signal: { dbm: -63, qualityPercent: null }, security: null, note: null }],
    channels: [40], firstSeen: now, lastSeen: now, comparedWith: [], coarse: false,
  }],
  marks: [], limits: [],
};

// Active tests at two points of floor 1: speeds, and a hostile link PHY string.
const link = { connected: true, iface: "wlan0", bssid: "AA:BB:CC:00:00:01", frequencyMhz: 5180, signalDbm: -55, txKbps: 866700, rxKbps: null, phy: "VHT<b>", mcs: 9, nss: 2, widthMhz: 80 };
const pointTest = (id, pointId, role, results, status = "ok") => ({
  id, pointId, kind: role === "gateway" ? "ping" : "iperf3", target: "192.168.1.10", role,
  method: role === "gateway" ? "icmp" : "iperf3_tcp", status, startedAt: now, durationMs: 1000,
  adapterId: "linux:wlan0", link, linkAfter: null, roamed: null, results, error: status === "ok" ? null : "busy", errorHint: null,
});
const iperf = (direction, mbps) => ({ type: "iperf3", version: 1, direction, server: "192.168.1.10:5201", streams: 4, durationS: 5, omitS: 1, bitsPerSecond: mbps * 1e6, receiverBytes: 0, receiverSeconds: 5, measuredBy: "server", senderBytes: null, retransmits: 12, retransmitsSource: "server", intervals: [] });
const ping = { type: "ping", version: 1, method: "icmp", port: null, sent: 10, received: 10, lossPercent: 0, minMs: 2, avgMs: 2.4, maxMs: 3, jitterMs: 0.5, resolutionMs: null, fallbackReason: null, probes: [] };
const floor1Tests = [
  pointTest(1, floor1Points[0].id, "gateway", ping),
  pointTest(2, floor1Points[0].id, "iperf3_download", iperf("download", 1207)),
  pointTest(3, floor1Points[0].id, "iperf3_upload", iperf("upload", 263.4)),
  pointTest(4, floor1Points[1].id, "iperf3_download", null, "failed"),
];

const src = {
  project,
  scope: { kind: "project", id: 1 },
  floors: [
    { building, floor: floors[0], points: floor1Points, annotations: annotations(1, floor1Points), requirements, pointTests: floor1Tests, planData: tiny, photoData: new Map([[1, tiny]]) },
    { building, floor: floors[1], points: floor2Points, annotations: annotations(2, floor2Points), requirements: null, pointTests: [], planData: tiny, photoData: new Map([[2, tiny]]) },
  ],
  aps: aps.map((a) => ({ ap: a, buildingId: 1, floorName: "Ground floor" })),
  findings,
  ssids: ["Corp", HOSTILE_SSID],
  branding: { technicianName: `Tech <script>`, companyName: `Co "quoted"`, logoData: tiny },
  version: "0.0.0-test",
  generatedAt: new Date(now),
};

let progress = 0;
const html = await r.buildReport(src, {
  compute: async (job) => r.runHeatJob(job),
  encodePng: (g) => png(g.cols, g.rows, g.rgba),
  onProgress: (done) => (progress = done),
});

const out = process.argv.indexOf("--out");
if (out > 0 && process.argv[out + 1]) writeFileSync(process.argv[out + 1], html);

// --- Checks ---------------------------------------------------------------------

let checks = 0;
function check(name, fn) {
  try {
    fn();
    checks++;
  } catch (e) {
    console.error(`FAIL ${name}\n  ${e.message}`);
    process.exitCode = 1;
  }
}

const tags = html.match(/<[^>]*>/g) ?? [];
const attrs = (tag) => [...tag.matchAll(/\s([a-zA-Z:-]+)\s*=\s*("[^"]*"|'[^']*'|[^\s>]+)/g)].map((m) => ({ name: m[1].toLowerCase(), value: m[2].replace(/^["']|["']$/g, "") }));
const textOnly = html.replace(/<[^>]*>/g, "");

check("CSP meta is in <head>, before anything that could load", () => {
  const head = html.slice(0, html.indexOf("</head>"));
  const csp = `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data:; style-src 'unsafe-inline'">`;
  assert.ok(head.includes(csp), "CSP meta missing");
  assert.ok(head.indexOf(csp) < head.indexOf("<title>"), "CSP must come first");
  assert.equal(r.REPORT_CSP, "default-src 'none'; img-src data:; style-src 'unsafe-inline'");
});

check("no script, frames, objects, links, bases, forms or refresh", () => {
  assert.ok(!/<script/i.test(html), "<script found");
  for (const t of ["iframe", "frame", "object", "embed", "link", "base", "form", "audio", "video", "source", "foreignobject"])
    assert.ok(!new RegExp(`<${t}[\\s>/]`, "i").test(html), `<${t}> found`);
  assert.ok(!/http-equiv\s*=\s*["']?refresh/i.test(html), "meta refresh found");
});

check("no URLs in markup: http(s), protocol-relative, url()", () => {
  for (const tag of tags) {
    assert.ok(!/https?:/i.test(tag), `URL in tag: ${tag.slice(0, 120)}`);
    for (const a of attrs(tag)) assert.ok(!a.value.startsWith("//"), `protocol-relative URL: ${tag.slice(0, 120)}`);
  }
  assert.ok(!/url\s*\(/i.test(html), "url() found");
  assert.ok(!/@import/i.test(html), "@import found");
});

check("no event handler attributes", () => {
  for (const tag of tags) for (const a of attrs(tag)) assert.ok(!a.name.startsWith("on"), `${a.name} in ${tag.slice(0, 120)}`);
});

check("every image is a data: URI", () => {
  const imgs = tags.filter((t) => /^<img\s/i.test(t));
  assert.ok(imgs.length >= 3, "expected the logo and photos as <img>");
  for (const t of imgs) assert.match(attrs(t).find((a) => a.name === "src")?.value ?? "", /^data:image\/(png|jpeg|svg\+xml);base64,/, t.slice(0, 120));
  const images = tags.filter((t) => /^<image\s/i.test(t));
  assert.ok(images.length >= 2, "expected plan and heatmap <image>s");
  for (const t of images) assert.match(attrs(t).find((a) => a.name === "href")?.value ?? "", /^data:image\/(png|jpeg|svg\+xml);base64,/, t.slice(0, 120));
  for (const t of tags.filter((x) => /^<use\s/i.test(x))) assert.match(attrs(t).find((a) => a.name === "href")?.value ?? "", /^#plan-\d+$/);
  for (const t of tags) for (const a of attrs(t)) if (["src", "href", "xlink:href", "srcset", "poster", "action"].includes(a.name)) assert.match(a.value, /^(data:|#)/, t.slice(0, 120));
});

check("speed per point: map labels and table, only where there are tests", () => {
  assert.equal(html.split("<h3>Speed per point</h3>").length - 1, 1, "one floor has tests");
  assert.ok(textOnly.includes("↓ 1207  ↑ 263"), "plan label with both speeds");
  assert.ok(textOnly.includes("2.4 ms"), "gateway ping");
  assert.ok(html.includes("<td>failed</td>"), "a download that didn't run says so");
  assert.ok(html.includes("VHT&lt;b&gt; MCS 9"), "link text escaped");
  assert.ok(!html.includes("VHT<b>"), "raw link text");
});

check("untrusted text is escaped", () => {
  assert.ok(html.includes("&lt;script&gt;alert(&quot;ssid&quot;)&lt;/script&gt;"), "hostile SSID not found escaped");
  assert.ok(html.includes("&quot;&gt;&lt;img src=https://evil.example/x.png onerror=alert(1)&gt;"), "URL SSID not found escaped");
  assert.ok(html.includes("Acme &lt;b&gt;HQ&lt;/b&gt;"), "project name not escaped");
  assert.ok(html.includes("AP &lt;img src=x onerror=alert(3)&gt;"), "AP name not escaped");
  assert.ok(html.includes("&lt;iframe src=&quot;//evil.example&quot;&gt;"), "point note not escaped");
  assert.ok(!html.includes("<b>HQ</b>") && !html.includes("<i>floor</i>"), "markup from names survived");
  // Text only ever holds URLs as escaped text, never markup.
  assert.ok(textOnly.includes("https://evil.example"), "expected the URL as text");
});

check("report content: cover, maps, findings, caveats", () => {
  assert.ok(html.includes("Linux (NetworkManager)"), "OS / provider missing");
  assert.ok(html.includes("Intel Wi-Fi 6E AX211"), "adapter model missing");
  assert.ok(html.includes("Tech &lt;script&gt;") && html.includes("Co &quot;quoted&quot;"), "branding missing");
  assert.ok(html.includes("of the mapped area reaches -67 dBm"), "coverage share missing");
  assert.ok(html.includes("Serving access point"), "serving map missing");
  assert.ok(html.includes("Requirements: Office &lt;script&gt;"), "requirements map missing");
  for (const t of ["Areas below the coverage target", "Own access points sharing or overlapping a channel", "Neighbouring networks on own channels", "2.4 GHz networks not on channel 1, 6 or 11", "Points where nothing was heard"])
    assert.ok(html.includes(t), `finding "${t}" missing`);
  assert.ok(html.includes("AP 1 (ch 36, 80 MHz wide) and AP &lt;img src=x onerror=alert(3)&gt; (ch 44, 20 MHz wide): overlapping channels in 5 GHz"), "own overlap missing");
  assert.ok(/66:66:66:00:00:02 \(ch 3, 20 MHz wide\)[^<]*overlaps AP 1, AP 2/.test(html), "2.4 GHz neighbour overlap missing");
  assert.ok(html.includes("width not reported: primary channel only"), "unknown width not stated");
  assert.ok(html.includes("on channel 3"), "2.4 GHz channel 3 not reported");
  assert.ok(/Ground floor: point 12\./.test(html), "dead zone missing");
  assert.equal((html.match(/Based on /g) ?? []).length >= 5, true, "findings must say what they're based on");
  assert.ok(html.includes("Caveats") && html.includes("walls"), "caveats missing");
  assert.ok(progress > 0, "no progress reported");
});

check("CSV: BOM, numbers kept, formulas defused", () => {
  const csv = new TextDecoder("utf-8", { ignoreBOM: true }).decode(r.rawCsv(src.floors));
  assert.ok(csv.startsWith("﻿"), "BOM missing");
  assert.ok(csv.includes(`"'=HYPERLINK(""https://evil.example"",""click"")"`), "formula cell not defused");
  assert.ok(/,-54,/.test(csv), "negative dBm should stay a number");
  assert.ok(csv.includes("\r\n"), "CRLF rows expected");
});

console.log(`${checks} report checks passed${process.exitCode ? ", some FAILED" : ""} (report ${(html.length / 1024).toFixed(0)} KiB).`);
