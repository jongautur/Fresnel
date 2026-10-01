import type { SignalUnit } from "../state/Preferences";
import type {
  Adapter,
  LinkRate,
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

/** The value to show for `unit`: the preferred one if measured, otherwise
 *  the other (always labelled with its own unit — never converted). */
function pick(s: Signal, unit: SignalUnit): "dbm" | "percent" | null {
  const order: ("dbm" | "percent")[] = unit === "dbm" ? ["dbm", "percent"] : ["percent", "dbm"];
  for (const u of order) {
    if (u === "dbm" && s.dbm != null) return "dbm";
    if (u === "percent" && s.qualityPercent != null) return "percent";
  }
  return null;
}

/** dBm thresholds follow common enterprise design targets (-67 dBm for voice). */
export function signalView(s: Signal, unit: SignalUnit = "dbm"): SignalView {
  const which = pick(s, unit);
  if (which === "dbm" && s.dbm != null) {
    const d = s.dbm;
    const level: Level = d >= -60 ? "excellent" : d >= -67 ? "good" : d >= -75 ? "fair" : "poor";
    const fill = Math.max(0, Math.min(100, ((d + 95) / 65) * 100));
    return { value: d.toFixed(0), unit: "dBm", level, fill };
  }
  if (which === "percent" && s.qualityPercent != null) {
    const q = s.qualityPercent;
    const level: Level = q >= 75 ? "excellent" : q >= 55 ? "good" : q >= 35 ? "fair" : "poor";
    return { value: String(q), unit: "%", level, fill: q };
  }
  return { value: "—", unit: "", level: "none", fill: 0 };
}

/** Comparable number, stronger = larger. Readings in the preferred unit rank
 *  above readings that only exist in the other unit. */
export function signalSortValue(s: Signal, unit: SignalUnit = "dbm"): number {
  const which = pick(s, unit);
  const preferred = which === unit;
  const base = preferred ? 1000 : 0;
  if (which === "dbm" && s.dbm != null) return base + 200 + s.dbm;
  if (which === "percent" && s.qualityPercent != null) return base + s.qualityPercent;
  return -Infinity;
}

export function formatBitrate(kbps: number | null | undefined): string {
  if (kbps == null) return "—";
  if (kbps >= 1000) return `${(kbps / 1000).toFixed(kbps >= 100_000 ? 0 : 1)} Mbit/s`;
  return `${kbps} kbit/s`;
}

/** "1733 Mbit/s · VHT MCS 9 · 2 SS · 160 MHz · SGI" */
export function formatLinkRate(r: LinkRate | null | undefined): string {
  if (!r) return "—";
  const parts = [formatBitrate(r.bitrateKbps)];
  if (r.phy && r.mcs != null) parts.push(`${r.phy} MCS ${r.mcs}`);
  if (r.nss != null) parts.push(`${r.nss} SS`);
  if (r.widthMhz != null) parts.push(`${r.widthMhz} MHz`);
  if (r.shortGi) parts.push("SGI");
  return parts.join(" · ");
}

/** "802.11ax" → "ax", with the Wi-Fi generation for tooltips. */
export function phyShort(phy: string | null): string {
  return phy ? phy.replace(/^802\.11/, "") : "—";
}

export function formatAge(ms: number | null | undefined): string {
  if (ms == null) return "—";
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  return `${m}m${String(s % 60).padStart(2, "0")}s`;
}

const pad = (n: number) => String(n).padStart(2, "0");

/** Local time, 24 h: `20:35:46`. */
export function formatTime(iso: string): string {
  const d = new Date(iso);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

/** Local date and time, ISO order, 24 h: `2026-10-01 20:35`. */
export function formatDateTime(iso: string): string {
  const d = new Date(iso);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function formatMhz(mhz: number | null | undefined): string {
  return mhz == null ? "—" : `${mhz} MHz`;
}

/** "2.4 / 5 GHz" — only bands known to be supported. Undetectable bands
 *  are shown separately as capability chips. */
export function bandsSummary(a: Adapter): string {
  const caps: [Capability, string][] = [
    [a.capabilities.band2ghz, "2.4"],
    [a.capabilities.band5ghz, "5"],
    [a.capabilities.band6ghz, "6"],
  ];
  const parts = caps.filter(([c]) => c === "supported").map(([, label]) => label);
  return parts.length ? `${parts.join(" / ")} GHz` : "bands unknown";
}

export function busSummary(a: Adapter): string | null {
  const b = a.bus;
  if (!b) return null;
  const ids = b.vendorId && b.productId ? ` ${b.vendorId}:${b.productId}` : "";
  return `${b.kind.toUpperCase()}${ids}`;
}
