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
  /** Minimum gap between triggered scans on this adapter; 0 = none. Null if not reported. */
  scanSpacingMs: number | null;
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

/** A bare number is an unrecognised suite selector (OUI << 8 | type). */
export type Akm =
  | "psk"
  | "psk_sha256"
  | "psk_sha384"
  | "ft_psk"
  | "ft_psk_sha384"
  | "sae"
  | "sae_ext_key"
  | "ft_sae"
  | "ft_sae_ext_key"
  | "ieee8021x"
  | "ieee8021x_sha256"
  | "ieee8021x_sha384"
  | "ft_ieee8021x"
  | "suite_b"
  | "suite_b192"
  | "ft_suite_b192"
  | "fils_sha256"
  | "fils_sha384"
  | "ft_fils_sha256"
  | "ft_fils_sha384"
  | "owe"
  | "owe_transition"
  | number;
/** A bare number is an unrecognised suite selector (OUI << 8 | type). */
export type Cipher =
  | "wep40"
  | "wep104"
  | "tkip"
  | "ccmp"
  | "ccmp256"
  | "gcmp"
  | "gcmp256"
  | "bip_cmac128"
  | "bip_cmac256"
  | "bip_gmac128"
  | "bip_gmac256"
  | number;

/** Protected Management Frames (802.11w). */
export type Pmf = "disabled" | "capable" | "required";

export interface Security {
  kind: SecurityKind;
  privacy: boolean;
  wpa: boolean;
  rsn: boolean;
  akms: Akm[];
  pairwiseCiphers: Cipher[];
  groupCiphers: Cipher[];
  /** Group management (BIP) cipher; RSN element only. */
  groupMgmtCipher: Cipher | null;
  /** null when the source doesn't say (NetworkManager flags, no RSN capabilities). */
  pmf: Pmf | null;
}

export interface AccessPointObservation {
  timestamp: string;
  adapterId: AdapterId;
  bssid: string;
  /** Wi-Fi 7 MLD MAC (Basic Multi-Link element), same form as bssid. */
  mldAddress: string | null;
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
  /** null when the provider doesn't report the guard interval */
  shortGi: boolean | null;
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
  /** Platform-specific advice from the provider (e.g. polkit, Location settings). */
  hint?: string | null;
}
