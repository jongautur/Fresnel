// Labels and summaries for active tests (tables, the Measure Here toggle).
import type { LinkSnapshot, PointTest, PointTestRole, TestSettings } from "../types/pointTests";
import { formatBitrate } from "./format";

export const ROLE_LABEL: Record<PointTestRole, string> = {
  gateway: "Gateway ping",
  extra_host: "Host ping",
  iperf3_upload: "iperf3 upload",
  iperf3_download: "iperf3 download",
};

export function formatThroughput(bitsPerSecond: number): string {
  const mbit = bitsPerSecond / 1e6;
  if (mbit >= 100) return `${mbit.toFixed(0)} Mbit/s`;
  if (mbit >= 1) return `${mbit.toFixed(1)} Mbit/s`;
  return `${(bitsPerSecond / 1e3).toFixed(0)} kbit/s`;
}

const ms = (v: number) => `${v < 10 ? v.toFixed(1) : v.toFixed(0)} ms`;

/** How it was measured, honestly: ICMP echo, or TCP connects to a port, or iperf3. */
export function methodLabel(t: PointTest): string {
  const r = t.results;
  switch (t.method) {
    case "icmp":
      return "ICMP echo";
    case "tcp_connect":
      return r?.type === "ping" && r.port != null ? `TCP connect :${r.port}` : "TCP connect";
    case "iperf3_tcp":
      return r?.type === "iperf3" ? `iperf3 TCP × ${r.streams}` : "iperf3 TCP";
  }
}

/** Why the method isn't ICMP, for a tooltip. */
export function methodNote(t: PointTest): string | undefined {
  const r = t.results;
  if (r?.type === "ping" && r.fallbackReason) return `Timed TCP connects instead of ping: ${r.fallbackReason}`;
  if (r?.type === "iperf3") {
    const by = r.measuredBy === "server" ? "the server's received bytes" : "bytes received here";
    return `${r.durationS} s measured after ${r.omitS} s omitted (slow start); average from ${by}.`;
  }
  return undefined;
}

/** "3.2 ms avg · jitter 0.8 ms · 0 % loss", or "245 Mbit/s · 12 retransmits". */
export function resultSummary(t: PointTest): string {
  const r = t.results;
  if (!r) return "—";
  if (r.type === "ping") {
    const parts = [];
    if (r.avgMs != null) parts.push(`${ms(r.avgMs)} avg`);
    if (r.jitterMs != null) {
      parts.push(`jitter ${ms(r.jitterMs)}${r.resolutionMs != null ? ` (${r.resolutionMs} ms clock)` : ""}`);
    }
    parts.push(`${r.lossPercent.toFixed(0)} % loss (${r.received}/${r.sent})`);
    return parts.join(" · ");
  }
  const parts = [formatThroughput(r.bitsPerSecond)];
  if (r.retransmits != null) parts.push(`${r.retransmits} retransmit${r.retransmits === 1 ? "" : "s"}`);
  return parts.join(" · ");
}

/** The link at test time: "−55 dBm · VHT MCS 9 · 2 SS · 866 Mbit/s". */
export function linkSummary(l: LinkSnapshot): string {
  if (!l.connected) return "not connected";
  const parts = [];
  if (l.signalDbm != null) parts.push(`${l.signalDbm.toFixed(0)} dBm`);
  if (l.phy && l.mcs != null) parts.push(`${l.phy} MCS ${l.mcs}`);
  if (l.nss != null) parts.push(`${l.nss} SS`);
  if (l.txKbps != null) parts.push(`TX ${formatBitrate(l.txKbps)}`);
  return parts.length ? parts.join(" · ") : "connected";
}

export function roamedLabel(t: PointTest): string {
  if (t.linkAfter && !t.linkAfter.connected) return "link lost";
  if (t.roamed == null) return "unknown";
  return t.roamed ? `yes → ${t.linkAfter?.bssid ?? "?"}` : "no";
}

/** What "+ run tests" will do, for the toggle. */
export function plannedTests(s: TestSettings): string {
  const parts = ["gateway ping"];
  if (s.extraHost) parts.push(`ping ${s.extraHost}`);
  if (s.iperf3Server && s.iperf3InPointTests) {
    const dirs = s.iperf3Directions === "both" ? "↑↓" : s.iperf3Directions === "upload" ? "↑" : "↓";
    const runs = s.iperf3Directions === "both" ? 2 : 1;
    parts.push(`iperf3 ${dirs} ${s.iperf3Server} (≈ ${runs * (s.iperf3DurationS + s.iperf3OmitS)} s)`);
  }
  return parts.join(", ");
}

// ---------------------------------------------------------------------------
// Speed per point (the floor table and the plan's Speed view)
// ---------------------------------------------------------------------------

/** A throughput at a point: measured, the test failed to run, or no test. */
export type Throughput = { mbps: number; retransmits: number | null } | "failed" | null;
export type GatewayPing = { avgMs: number | null; jitterMs: number | null; lossPercent: number } | "failed" | null;

export interface PointSpeed {
  down: Throughput;
  up: Throughput;
  ping: GatewayPing;
  /** The link when the point's tests started (the gateway ping's, else the first test's). */
  link: LinkSnapshot | null;
  tests: number;
}

/** The latest finished test of a role (cancelled ones don't count), as the requirement rules use. */
function latest(tests: PointTest[], role: PointTestRole): PointTest | undefined {
  let best: PointTest | undefined;
  for (const t of tests) {
    if (t.role !== role || t.status === "cancelled") continue;
    if (!best || t.startedAt > best.startedAt || (t.startedAt === best.startedAt && t.id > best.id)) best = t;
  }
  return best;
}

function throughput(t: PointTest | undefined): Throughput {
  if (!t) return null;
  const r = t.results;
  if (t.status !== "ok" || r?.type !== "iperf3") return "failed";
  return { mbps: r.bitsPerSecond / 1e6, retransmits: r.retransmits };
}

/** Point id → its latest download, upload and gateway ping. */
export function pointSpeeds(tests: PointTest[]): Map<number, PointSpeed> {
  const byPoint = new Map<number, PointTest[]>();
  for (const t of tests) byPoint.set(t.pointId, [...(byPoint.get(t.pointId) ?? []), t]);
  const out = new Map<number, PointSpeed>();
  for (const [id, list] of byPoint) {
    const gw = latest(list, "gateway");
    const r = gw?.results;
    out.set(id, {
      down: throughput(latest(list, "iperf3_download")),
      up: throughput(latest(list, "iperf3_upload")),
      ping: !gw
        ? null
        : r?.type === "ping"
          ? { avgMs: r.avgMs, jitterMs: r.jitterMs, lossPercent: r.lossPercent }
          : "failed",
      link: (gw ?? list[0])?.link ?? null,
      tests: list.length,
    });
  }
  return out;
}

export const formatMbps = (mbps: number) => (mbps >= 100 ? mbps.toFixed(0) : mbps.toFixed(1));

export type SpeedRule = "download" | "upload" | "latency" | "loss";

/**
 * "↓ 1207  ↑ 1111" and "2.4 ms": what the plan shows under a point. Values
 * that miss the profile's target get a ✗, so the colour isn't the only sign.
 */
export function speedLines(s: PointSpeed | undefined, failing: ReadonlySet<SpeedRule> = new Set()): string[] {
  if (!s) return ["no tests"];
  const x = (r: SpeedRule) => (failing.has(r) ? " ✗" : "");
  const tp = (arrow: string, v: Throughput, r: SpeedRule) =>
    v == null ? null : v === "failed" ? `${arrow} failed` : `${arrow} ${formatMbps(v.mbps)}${x(r)}`;
  const first = [tp("↓", s.down, "download"), tp("↑", s.up, "upload")].filter(Boolean).join("  ");
  const p = s.ping;
  const second =
    p == null
      ? null
      : p === "failed"
        ? "ping failed"
        : p.avgMs == null
          ? "no ping replies"
          : `${p.avgMs < 10 ? p.avgMs.toFixed(1) : p.avgMs.toFixed(0)} ms${x("latency")}${p.lossPercent > 0 || failing.has("loss") ? ` · ${p.lossPercent.toFixed(0)} % loss${x("loss")}` : ""}`;
  const lines = [first, second].filter((l): l is string => !!l);
  return lines.length ? lines : ["no speed tests"];
}
