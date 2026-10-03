// Mirrors fresnel-core survey/models.rs (PointTest) and nettools (camelCase).
import type { AdapterId } from "./wifi";

export type PointTestKind = "ping" | "iperf3";
export type PointTestRole = "gateway" | "extra_host" | "iperf3_upload" | "iperf3_download";
/** icmp: real echo. tcp_connect: timed TCP connects (ICMP not allowed). */
export type PointTestMethod = "icmp" | "tcp_connect" | "iperf3_tcp";
export type PointTestStatus = "ok" | "failed" | "cancelled";

export interface LinkSnapshot {
  connected: boolean;
  iface: string | null;
  bssid: string | null;
  frequencyMhz: number | null;
  signalDbm: number | null;
  txKbps: number | null;
  rxKbps: number | null;
  phy: string | null;
  mcs: number | null;
  nss: number | null;
  widthMhz: number | null;
}

export type ProbeOutcome =
  | { outcome: "reply"; rttMs: number }
  /** rttMs null on Windows: its retries after a reset make the time meaningless. */
  | { outcome: "refused"; rttMs: number | null }
  | { outcome: "timeout" }
  | { outcome: "unreachable"; detail: string }
  | { outcome: "error"; detail: string };

export interface PingResult {
  type: "ping";
  version: number;
  method: "icmp" | "tcp_connect";
  port: number | null;
  sent: number;
  received: number;
  lossPercent: number;
  minMs: number | null;
  avgMs: number | null;
  maxMs: number | null;
  jitterMs: number | null;
  /** Coarse RTT clock (Windows ICMP: 1 ms). */
  resolutionMs: number | null;
  fallbackReason: string | null;
  probes: ProbeOutcome[];
}

export interface Iperf3Result {
  type: "iperf3";
  version: number;
  direction: "upload" | "download";
  server: string;
  streams: number;
  durationS: number;
  omitS: number;
  /** Receiver average over the measured (non-omitted) time. */
  bitsPerSecond: number;
  receiverBytes: number;
  receiverSeconds: number;
  measuredBy: "server" | "client";
  senderBytes: number | null;
  retransmits: number | null;
  retransmitsSource: "client_tcp_info" | "server" | null;
}

export interface PointTest {
  id: number;
  pointId: number;
  kind: PointTestKind;
  target: string;
  role: PointTestRole;
  method: PointTestMethod;
  status: PointTestStatus;
  startedAt: string;
  durationMs: number;
  adapterId: AdapterId;
  link: LinkSnapshot;
  linkAfter: LinkSnapshot | null;
  /** null: unknown. */
  roamed: boolean | null;
  results: PingResult | Iperf3Result | null;
  error: string | null;
  errorHint: string | null;
}

export type Iperf3Directions = "upload" | "download" | "both";

export interface TestSettings {
  version: number;
  pingCount: number;
  /** Timed when ICMP isn't allowed. */
  tcpPort: number;
  extraHost: string | null;
  iperf3Server: string | null;
  iperf3Port: number;
  iperf3Streams: number;
  iperf3DurationS: number;
  iperf3OmitS: number;
  iperf3Directions: Iperf3Directions;
}
