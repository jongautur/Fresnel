// Mirrors fresnel-core survey/requirements.rs and database/requirements.rs (camelCase).
import type { Band, Capability } from "./wifi";
import type { MeasuringAdapter } from "./survey";

export type Preset = "office_data" | "voice_video" | "warehouse_basic" | "custom";

/** What the measuring card could receive; null on older points (not recorded). */
export interface AdapterBands {
  band2ghz: Capability;
  band5ghz: Capability;
  band6ghz: Capability;
}

/** `null` switches a rule off. */
export interface RequirementValues {
  primaryMinDbm: number;
  secondaryMinDbm: number | null;
  /** Other radios overlapping the primary's channel, at or above `cochannelLevelDbm`. */
  cochannelMax: number | null;
  cochannelLevelDbm: number | null;
  requiredBands: Band[];
  minSnrDb: number | null;
  /** Channel load the AP advertises (BSS Load). */
  maxUtilPct: number | null;
}

export type RequirementTarget = { kind: "ssid"; ssidRaw: number[] } | { kind: "ap"; apId: number };

export type TargetInfo = RequirementTarget & {
  /** The SSID or the AP's name. */
  label: string;
  bssids: string[];
};

export interface RequirementProfile extends RequirementValues {
  id: number;
  projectId: number;
  name: string;
  /** "custom" once the values differ from the preset's. */
  preset: Preset;
  isDefault: boolean;
  targets: TargetInfo[];
  createdAt: string;
  updatedAt: string;
}

export interface RequirementProfileInput extends RequirementValues {
  name: string;
  preset: Preset;
  isDefault: boolean;
}

export interface PresetInfo {
  preset: Preset;
  label: string;
  values: RequirementValues | null;
}

export interface TargetOptions {
  ssids: { ssidRaw: number[]; label: string; points: number }[];
  aps: { apId: number; name: string; buildingName: string; floorName: string; bssidCount: number }[];
}

export type Outcome = "pass" | "fail" | "not_evaluated";
export type Rule = "primary_signal" | "secondary_signal" | "co_channel" | "required_band" | "snr" | "utilisation";

export interface RuleResult {
  rule: Rule;
  band: Band | null;
  outcome: Outcome;
  measured: number | null;
  limit: number | null;
  detail: string;
  /** Physical APs were told apart by the BSSID heuristic, not placed APs. */
  heuristic: boolean;
}

export interface PointEvaluation {
  pointId: number;
  outcome: Outcome;
  reason: string | null;
  rules: RuleResult[];
  primaryBssid: string | null;
  primaryDbm: number | null;
  secondaryDbm: number | null;
  cochannelCount: number | null;
  widthUnknown: boolean;
}

export interface RuleCount {
  rule: Rule;
  band: Band | null;
  count: number;
}

/** Over survey POINTS; the share of mapped area is estimated in the UI. */
export interface FloorSummary {
  points: number;
  passed: number;
  failed: number;
  notEvaluated: number;
  passFractionOfPoints: number | null;
  failuresByRule: RuleCount[];
  rulesNotEvaluated: RuleCount[];
  notEvaluatedReasons: { reason: string; count: number }[];
  adapters: MeasuringAdapter[];
  usesHeuristic: boolean;
  widthUnknownPoints: number;
}

export type ProfileSource = "floor_override" | "project_default";

export interface FloorRequirements {
  floorId: number;
  projectId: number;
  overrideProfileId: number | null;
  profile: RequirementProfile | null;
  source: ProfileSource | null;
  points: PointEvaluation[];
  summary: FloorSummary | null;
}
