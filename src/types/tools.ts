// Mirrors fresnel-core tools (ToolRun, run params, ToolEvent) and the
// nettools results it stores (camelCase).
import type { AdapterId } from "./wifi";
import type { LinkSnapshot, ProbeOutcome } from "./pointTests";

export type ToolKind = "ping" | "traceroute" | "dns" | "port_check" | "iperf3";
/** stopped: the user ended it; results cover what ran until then. */
export type ToolRunStatus = "ok" | "failed" | "stopped";
export type IpFamily = "any" | "v4" | "v6";

/** The system route, or bound to one Wi-Fi adapter. */
export type Via = { mode: "system" } | { mode: "wifi"; adapterId: AdapterId };

export interface ToolRun {
  id: number;
  kind: ToolKind;
  target: string;
  resolvedIp: string | null;
  /** The settings the run used (the params sent), for "run again". */
  params: unknown;
  status: ToolRunStatus;
  startedAt: string;
  durationMs: number;
  routeIface: string | null;
  adapterId: AdapterId | null;
  link: LinkSnapshot | null;
  linkAfter: LinkSnapshot | null;
  roamed: boolean | null;
  summary: string | null;
  /** Versioned JSON; null in listings (fetch the run for it). */
  results: unknown;
  error: string | null;
  errorHint: string | null;
  pointId: number | null;
}

export interface ResolvedTarget {
  input: string;
  ip: string;
  addresses: string[];
  resolveMs: number | null;
}

// ---- params ------------------------------------------------------------

export interface PingParams {
  target: string;
  family: IpFamily;
  via: Via;
  /** null: until stopped. */
  count: number | null;
  intervalMs: number;
  timeoutMs: number;
  payloadLen: number;
  method: "icmp" | "tcp_connect";
  tcpPort: number;
}

export type TraceMethod = "icmp" | "udp";

export interface TraceParams {
  target: string;
  family: IpFamily;
  via: Via;
  maxHops: number;
  timeoutMs: number;
  /** null: until stopped (MTR). */
  rounds: number | null;
  intervalMs: number;
  method: TraceMethod;
  names: boolean;
}

export type DnsTransport = "udp" | "tcp";

export interface DnsParams {
  name: string;
  recordType: string;
  /** "system" or an address; several to compare. */
  servers: string[];
  transport: DnsTransport;
  timeoutMs: number;
  via: Via;
}

export interface PortCheckParams {
  target: string;
  family: IpFamily;
  via: Via;
  ports: string;
  timeoutMs: number;
}

export type Iperf3Directions = "upload" | "download" | "both";
export type Iperf3Direction = "upload" | "download";

export interface Iperf3Params {
  server: string;
  port: number;
  family: IpFamily;
  via: Via;
  streams: number;
  durationS: number;
  omitS: number;
  directions: Iperf3Directions;
}

// ---- results -----------------------------------------------------------

export interface PingRunResult {
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
  resolutionMs: number | null;
  fallbackReason: string | null;
  payloadLen: number | null;
  stopped: boolean;
  /** Probes before probes[0] not kept (long continuous runs). */
  probesDropped: number;
  probes: ProbeOutcome[];
}

export type HopReply = { kind: "time_exceeded" } | { kind: "reached" } | { kind: "unreachable"; detail: string };

export interface HopProbe {
  round: number;
  ttl: number;
  from: string | null;
  rttMs: number | null;
  reply: HopReply | null;
}

export interface HopAddress {
  address: string;
  name: string | null;
  replies: number;
}

export interface HopStats {
  ttl: number;
  addresses: HopAddress[];
  sent: number;
  received: number;
  lossPercent: number;
  lastMs: number | null;
  bestMs: number | null;
  avgMs: number | null;
  worstMs: number | null;
  stdevMs: number | null;
  unreachable: string | null;
}

export interface TraceResult {
  version: number;
  method: TraceMethod;
  fallbackReason: string | null;
  maxHops: number;
  rounds: number;
  destinationTtl: number | null;
  resolutionMs: number | null;
  hops: HopStats[];
  stopped: boolean;
}

export type DnsSection = "answer" | "authority" | "additional";

export interface DnsRecord {
  section: DnsSection;
  name: string;
  recordType: string;
  ttl: number;
  data: string;
}

export interface DnsResult {
  version: number;
  queryName: string;
  recordType: string;
  server: string;
  transport: DnsTransport;
  retriedOverTcp: boolean;
  responseCode: string;
  authoritative: boolean;
  recursionAvailable: boolean;
  authenticData: boolean;
  truncated: boolean;
  queryMs: number;
  responseBytes: number;
  records: DnsRecord[];
}

export interface DnsLookup {
  server: string;
  source: string;
  result: DnsResult | null;
  error: string | null;
  errorHint: string | null;
}

export interface DnsRunResults {
  version: number;
  lookups: DnsLookup[];
}

export interface SystemDnsServer {
  address: string;
  source: string;
}

export type PortState = "open" | "closed" | "filtered" | "unreachable" | "error";

export interface PortOutcome {
  port: number;
  state: PortState;
  connectMs: number | null;
  detail: string | null;
}

export interface PortCheckResult {
  version: number;
  timeoutMs: number;
  ports: PortOutcome[];
  open: number;
  closed: number;
  filtered: number;
  other: number;
  requested: number;
  stopped: boolean;
}

export interface Iperf3Interval {
  startS: number;
  endS: number;
  bytes: number;
  bitsPerSecond: number;
  omitted: boolean;
  retransmits: number | null;
}

export interface Iperf3TcpResult {
  version: number;
  direction: Iperf3Direction;
  server: string;
  streams: number;
  durationS: number;
  omitS: number;
  bitsPerSecond: number;
  receiverBytes: number;
  receiverSeconds: number;
  measuredBy: "server" | "client";
  senderBytes: number | null;
  retransmits: number | null;
  retransmitsSource: "client_tcp_info" | "server" | null;
  intervals: Iperf3Interval[];
}

export interface Iperf3Test {
  direction: Iperf3Direction;
  result: Iperf3TcpResult | null;
  intervals: Iperf3Interval[];
  error: string | null;
  errorHint: string | null;
  stopped: boolean;
}

export interface Iperf3RunResults {
  version: number;
  tests: Iperf3Test[];
}

// ---- live events -------------------------------------------------------

export type TraceEvent =
  | ({ type: "probe" } & HopProbe)
  | { type: "name"; address: string; name: string };

export type ToolEvent =
  | {
      event: "started";
      target: ResolvedTarget | null;
      routeIface: string | null;
      adapterId: AdapterId | null;
      link: LinkSnapshot | null;
    }
  | { event: "probe"; seq: number; outcome: ProbeOutcome }
  | ({ event: "trace" } & TraceEvent)
  | ({ event: "port" } & PortOutcome)
  | { event: "iperf3_test"; direction: Iperf3Direction }
  | ({ event: "interval"; direction: Iperf3Direction } & Iperf3Interval);
