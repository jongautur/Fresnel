// Mirrors fresnel-core survey/findings.rs (camelCase): rogue / evil-twin detection.
import type { Akm, Band, Cipher, Pmf, SecurityKind, Signal } from "./wifi";
import type { PlacedAp } from "./survey";

export type FindingScope = { kind: "project" | "building" | "floor"; id: number };

export type MarkStatus = "ours_unplaced" | "neighbour" | "ignored";

export interface BssidMark {
  bssid: string;
  status: MarkStatus;
  note: string | null;
  updatedAt: string;
}

export type FindingKind =
  | "unknown_transmitter"
  | "security_mismatch"
  | "multi_frequency"
  | "bssid_inconsistent"
  | "unlinked_radio";

export type Severity = "warning" | "info";
export type Ownership = "linked" | "probable" | "marked_ours" | "unknown";
export type MatchBasis = "linked" | "same_mld" | "heuristic";

export interface ApMatch {
  apId: number;
  apName: string;
  floorId: number;
  basis: MatchBasis;
  viaBssid: string | null;
}

export interface SecurityProfile {
  kind: SecurityKind;
  /** false: measured before security detail was recorded; only `kind` is known. */
  recorded: boolean;
  akms: Akm[];
  pairwiseCiphers: Cipher[];
  groupCiphers: Cipher[];
  groupMgmtCipher: Cipher | null;
  pmf: Pmf | null;
  summary: string;
}

export interface Heard {
  pointId: number;
  floorId: number;
  floorName: string;
  buildingName: string;
  /** 1-based, in measurement order on its floor (as on the plan). */
  pointNumber: number;
  x: number;
  y: number;
  measuredAt: string;
  bssid: string;
  ssid: string | null;
  frequencyMhz: number;
  channel: number | null;
  band: Band;
  signal: Signal;
  /** null: a reading that was set aside, not stored as a sample. */
  security: SecurityProfile | null;
  note: string | null;
}

export interface Reference {
  bssid: string;
  apName: string | null;
  band: Band;
  security: SecurityProfile;
}

export interface Finding {
  key: string;
  kind: FindingKind;
  severity: Severity;
  title: string;
  explanation: string;
  bssid: string;
  ssid: string | null;
  band: Band | null;
  ownership: Ownership;
  ap: ApMatch | null;
  /** In scope, strongest first. */
  heard: Heard[];
  channels: number[];
  firstSeen: string | null;
  lastSeen: string | null;
  comparedWith: Reference[];
  /** Compared by security type only: some readings predate the detail. */
  coarse: boolean;
}

export interface Findings {
  projectId: number;
  scope: FindingScope;
  generatedAt: string;
  points: number;
  linkedBssids: number;
  projectSsids: string[];
  /** Warnings first. */
  findings: Finding[];
  marks: BssidMark[];
  limits: string[];
}

export interface FloorRef {
  id: number;
  name: string;
  buildingId: number;
  buildingName: string;
}

export interface LinkOptions {
  bssid: string;
  ssids: string[];
  /** Unlinked BSSIDs that look like other radios of the same AP, including `bssid`. */
  relatedBssids: string[];
  probableAp: ApMatch | null;
  /** Strongest reading per floor, strongest first. */
  strongestPerFloor: Heard[];
  /** Every placed AP in the project, oldest first. */
  aps: PlacedAp[];
  floors: FloorRef[];
}
