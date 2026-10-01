import type {
  Adapter,
  AdapterStatus,
  Band,
  Capability,
  SecurityKind,
  Signal,
} from "../types/wifi";

export const BAND_LABEL: Record<Band, string> = {
  "2.4ghz": "2.4 GHz",
  "5ghz": "5 GHz",
  "6ghz": "6 GHz",
  "60ghz": "60 GHz",
  unknown: "?",
};

export const SECURITY_LABEL: Record<SecurityKind, string> = {
  open: "Open",
  owe: "OWE",
  wep: "WEP",
  wpa_personal: "WPA-PSK",
  wpa2_personal: "WPA2-PSK",
  wpa3_personal: "WPA3-SAE",
  wpa2_wpa3_personal: "WPA2/WPA3",
  wpa_enterprise: "WPA-EAP",
  wpa2_enterprise: "WPA2-EAP",
  wpa3_enterprise: "WPA3-EAP-192",
  unknown: "Unknown",
};

export const STATUS_LABEL: Record<AdapterStatus, string> = {
  connected: "Connected",
  connecting: "Connecting",
  disconnecting: "Disconnecting",
  disconnected: "Disconnected",
  unavailable: "Unavailable",
  unmanaged: "Unmanaged",
  radio_off: "Radio off",
  failed: "Failed",
  unknown: "Unknown",
};

export type Level = "excellent" | "good" | "fair" | "poor" | "none";

export interface SignalView {
  /** e.g. "-62" or "77" */
  value: string;
  /** "dBm" or "%" — always shown, never implied */
  unit: "dBm" | "%" | "";
  level: Level;
  /** 0–100 bar fill */
  fill: number;
}

/** dBm thresholds follow common enterprise design targets (-67 dBm for voice). */
export function signalView(s: Signal): SignalView {
  if (s.dbm != null) {
    const d = s.dbm;
    const level: Level = d >= -60 ? "excellent" : d >= -67 ? "good" : d >= -75 ? "fair" : "poor";
    const fill = Math.max(0, Math.min(100, ((d + 95) / 65) * 100));
    return { value: d.toFixed(0), unit: "dBm", level, fill };
  }
  if (s.qualityPercent != null) {
    const q = s.qualityPercent;
    const level: Level = q >= 75 ? "excellent" : q >= 55 ? "good" : q >= 35 ? "fair" : "poor";
    return { value: String(q), unit: "%", level, fill: q };
  }
  return { value: "—", unit: "", level: "none", fill: 0 };
}

/** Comparable number, stronger = larger. dBm and % are not mixed in one scan. */
export function signalSortValue(s: Signal): number {
  if (s.dbm != null) return 1000 + s.dbm;
  if (s.qualityPercent != null) return s.qualityPercent;
  return -Infinity;
}

export function formatBitrate(kbps: number | null | undefined): string {
  if (kbps == null) return "—";
  if (kbps >= 1000) return `${(kbps / 1000).toFixed(kbps >= 100_000 ? 0 : 1)} Mbit/s`;
  return `${kbps} kbit/s`;
}

export function formatAge(ms: number | null | undefined): string {
  if (ms == null) return "—";
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  return `${m}m${String(s % 60).padStart(2, "0")}s`;
}

export function formatTime(iso: string): string {
  return new Date(iso).toLocaleTimeString(undefined, { hour12: false });
}

export function formatMhz(mhz: number | null | undefined): string {
  return mhz == null ? "—" : `${mhz} MHz`;
}

/** "2.4 / 5 / 6? GHz" — trailing ? marks capabilities the provider can't detect. */
export function bandsSummary(a: Adapter): string {
  const parts: string[] = [];
  const add = (cap: Capability, label: string) => {
    if (cap === "supported") parts.push(label);
    else if (cap === "unknown") parts.push(`${label}?`);
  };
  add(a.capabilities.band2ghz, "2.4");
  add(a.capabilities.band5ghz, "5");
  add(a.capabilities.band6ghz, "6");
  return parts.length ? `${parts.join(" / ")} GHz` : "bands unknown";
}

export function busSummary(a: Adapter): string | null {
  const b = a.bus;
  if (!b) return null;
  const ids = b.vendorId && b.productId ? ` ${b.vendorId}:${b.productId}` : "";
  return `${b.kind.toUpperCase()}${ids}`;
}
