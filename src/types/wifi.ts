// Mirror of crates/fresnel-core/src/wifi/models.rs (camelCase JSON).
// Keep in sync when the Rust model changes.

export type AdapterId = string;

export type Capability = "supported" | "unsupported" | "unknown";

export interface AdapterCapabilities {
  band2ghz: Capability;
  band5ghz: Capability;
  band6ghz: Capability;
  activeScan: Capability;
  passiveScan: Capability;
  monitorMode: Capability;
  packetCapture: Capability;
  apMode: Capability;
  signalDbm: Capability;
  signalQuality: Capability;
}

export type AdapterStatus =
  | "connected"
  | "connecting"
  | "disconnecting"
  | "disconnected"
  | "unavailable"
  | "unmanaged"
  | "radio_off"
  | "failed"
  | "unknown";

export type BusKind = "pci" | "usb" | "sdio" | "other";

export interface BusInfo {
  kind: BusKind;
  vendorId: string | null;
  productId: string | null;
  vendorName: string | null;
  productName: string | null;
}

export interface Adapter {
  id: AdapterId;
  provider: string;
  /** Backends actually supplying data, e.g. ["networkmanager", "nl80211"]. */
  dataSources: string[];
  interfaceName: string | null;
  displayName: string;
  driver: string | null;
  hwAddress: string | null;
  permanentHwAddress: string | null;
  bus: BusInfo | null;
  capabilities: AdapterCapabilities;
  status: AdapterStatus;
  statusDetail: string | null;
  connectedSsid: string | null;
}

export interface ProviderIssue {
  provider: string;
  error: ApiError;
}

export interface AdapterListing {
  adapters: Adapter[];
  issues: ProviderIssue[];
}

export type Band = "2.4ghz" | "5ghz" | "6ghz" | "60ghz" | "unknown";

/** dBm and quality % are independent; a provider fills what it measured. */
export interface Signal {
  dbm: number | null;
  qualityPercent: number | null;
}

export type WifiMode = "infrastructure" | "ad_hoc" | "mesh" | "access_point" | "unknown";

export type SecurityKind =
  | "open"
  | "owe"
  | "wep"
  | "wpa_personal"
  | "wpa2_personal"
  | "wpa3_personal"
  | "wpa2_wpa3_personal"
  | "wpa_enterprise"
  | "wpa2_enterprise"
  | "wpa3_enterprise"
  | "unknown";

export type Akm = "psk" | "sae" | "ieee8021x" | "owe" | "owe_transition" | "suite_b192";
export type Cipher = "wep40" | "wep104" | "tkip" | "ccmp";

export interface Security {
  kind: SecurityKind;
  privacy: boolean;
  wpa: boolean;
  rsn: boolean;
  akms: Akm[];
  pairwiseCiphers: Cipher[];
  groupCiphers: Cipher[];
}

export interface AccessPointObservation {
  timestamp: string;
  adapterId: AdapterId;
  bssid: string;
  ssid: string | null;
  ssidRaw: number[];
  hidden: boolean;
  frequencyMhz: number;
  channel: number | null;
  band: Band;
  channelWidthMhz: number | null;
  /** Centre of the whole occupied channel; differs from the primary for ≥40 MHz. */
  channelCenterMhz: number | null;
  signal: Signal;
  security: Security;
  mode: WifiMode;
  maxBitrateKbps: number | null;
  lastSeenAgeMs: number | null;
  isConnected: boolean;
  noiseDbm: number | null;
  snrDb: number | null;
  channelUtilizationPct: number | null;
  stationCount: number | null;
  beaconIntervalTu: number | null;
  phyType: string | null;
  wifiGeneration: number | null;
}

export interface ScanRequest {
  trigger: boolean;
  ssids?: string[];
}

export interface ScanResult {
  adapterId: AdapterId;
  provider: string;
  startedAt: string;
  completedAt: string;
  scanTriggered: boolean;
  notice: string | null;
  accessPoints: AccessPointObservation[];
}

export interface ConnectionInfo {
  adapterId: AdapterId;
  interfaceName: string | null;
  ssid: string | null;
  bssid: string | null;
  frequencyMhz: number | null;
  channel: number | null;
  band: Band | null;
  channelWidthMhz: number | null;
  signal: Signal;
  bitrateKbps: number | null;
  txRate: LinkRate | null;
  rxRate: LinkRate | null;
  security: Security | null;
  ipv4Addresses: string[];
  ipv4Gateway: string | null;
}

export interface LinkRate {
  bitrateKbps: number | null;
  /** "HT" | "VHT" | "HE" | "EHT"; null for legacy rates */
  phy: string | null;
  mcs: number | null;
  nss: number | null;
  widthMhz: number | null;
  shortGi: boolean;
}

// Mirror of fresnel-core error.rs `WifiError::kind()`.
export type ErrorKind =
  | "service_unavailable"
  | "no_adapters"
  | "adapter_not_found"
  | "adapter_unavailable"
  | "radio_disabled"
  | "permission_denied"
  | "scan_rejected"
  | "timeout"
  | "unsupported"
  | "database"
  | "invalid_input"
  | "adapter_mismatch"
  | "backend"
  | "ipc"; // frontend-only: invoke itself failed

export interface ApiError {
  kind: ErrorKind;
  message: string;
}
