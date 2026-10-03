// Helpers for the Tools page: run IDs, remembered forms, labels.
import type { LinkSnapshot } from "../types/pointTests";
import type { ToolKind, ToolRunStatus } from "../types/tools";

export const TOOL_LABEL: Record<ToolKind, string> = {
  ping: "Ping",
  traceroute: "Traceroute",
  dns: "DNS lookup",
  port_check: "Port check",
  iperf3: "iperf3",
};

export const RUN_STATUS_LABEL: Record<ToolRunStatus, string> = { ok: "OK", failed: "Failed", stopped: "Stopped" };
export const RUN_STATUS_CLASS: Record<ToolRunStatus, string> = {
  ok: "status-ok",
  failed: "status-bad",
  stopped: "status-idle",
};

let counter = 0;
/** A run ID for `cancelTool`: unique within this window. */
export function newRunId(kind: ToolKind): string {
  counter += 1;
  return `${kind}-${Date.now().toString(36)}-${counter}`;
}

export const fmtMs = (v: number | null | undefined): string =>
  v == null ? "—" : `${v < 10 ? v.toFixed(1) : v.toFixed(0)} ms`;

/** "−55 dBm · VHT MCS 9 · 2 SS" — the Wi-Fi link a run went over. */
export function shortLink(l: LinkSnapshot | null): string | null {
  if (!l) return null;
  if (!l.connected) return "Wi-Fi not connected";
  const parts = [];
  if (l.signalDbm != null) parts.push(`${l.signalDbm.toFixed(0)} dBm`);
  if (l.phy && l.mcs != null) parts.push(`${l.phy} MCS ${l.mcs}`);
  if (l.nss != null) parts.push(`${l.nss} SS`);
  if (l.bssid) parts.push(l.bssid);
  return parts.length ? parts.join(" · ") : "Wi-Fi connected";
}

/** A form remembered per tool in this browser profile (non-essential: falls back to defaults). */
export function loadForm<T extends object>(key: string, defaults: T): T {
  try {
    const raw = localStorage.getItem(`fresnel.tools.${key}`);
    if (!raw) return defaults;
    const saved = JSON.parse(raw) as Partial<T>;
    // Only known keys of the right type survive.
    const out = { ...defaults };
    for (const k of Object.keys(defaults) as (keyof T)[]) {
      const v = saved[k];
      if (v !== undefined && (typeof v === typeof defaults[k] || defaults[k] === null || v === null)) {
        out[k] = v as T[keyof T];
      }
    }
    return out;
  } catch {
    return defaults;
  }
}

export function saveForm<T>(key: string, form: T) {
  try {
    localStorage.setItem(`fresnel.tools.${key}`, JSON.stringify(form));
  } catch {
    /* non-essential */
  }
}

export function formatBps(bps: number): string {
  if (bps >= 1e9) return `${(bps / 1e9).toFixed(2)} Gbit/s`;
  if (bps >= 1e8) return `${(bps / 1e6).toFixed(0)} Mbit/s`;
  if (bps >= 1e6) return `${(bps / 1e6).toFixed(1)} Mbit/s`;
  return `${(bps / 1e3).toFixed(0)} kbit/s`;
}
