// Mirrors fresnel-core survey/models.rs (camelCase).
import type { AdapterId, Band, SecurityKind, Signal } from "./wifi";

export interface Building {
  id: number;
  projectId: number;
  name: string;
  createdAt: string;
  updatedAt: string;
}

export interface NewBuilding {
  projectId: number;
  name: string;
}

export interface FloorPlan {
  file: string;
  mime: string;
  /** Natural size in px: the coordinate space of points and the scale line. */
  width: number;
  height: number;
}

export interface FloorScale {
  x1: number;
  y1: number;
  x2: number;
  y2: number;
  lengthM: number;
}

export interface Floor {
  id: number;
  buildingId: number;
  name: string;
  level: number;
  plan: FloorPlan | null;
  scale: FloorScale | null;
  pointCount: number;
  createdAt: string;
  updatedAt: string;
}

export interface NewFloor {
  buildingId: number;
  name: string;
  level: number;
}

export interface MeasuringAdapter {
  id: AdapterId;
  provider: string;
  model: string | null;
  driver: string | null;
  hwId: string | null;
}

export interface Sample {
  bssid: string;
  ssid: string | null;
  ssidRaw: number[];
  frequencyMhz: number;
  channel: number | null;
  band: Band;
  channelWidthMhz: number | null;
  channelCenterMhz: number | null;
  signal: Signal;
  security: SecurityKind;
  phyType: string | null;
  wifiGeneration: number | null;
  noiseDbm: number | null;
  snrDb: number | null;
  channelUtilizationPct: number | null;
  stationCount: number | null;
  lastSeenAgeMs: number | null;
  isConnected: boolean;
}

export interface SurveyPoint {
  id: number;
  floorId: number;
  x: number;
  y: number;
  measuredAt: string;
  scanDurationMs: number;
  adapter: MeasuringAdapter;
  /** Strongest first. */
  samples: Sample[];
}

export interface MeasureRequest {
  floorId: number;
  x: number;
  y: number;
  adapterId: AdapterId;
  allowAdapterChange?: boolean;
}

export interface PlacedAp {
  id: number;
  floorId: number;
  name: string;
  x: number;
  y: number;
  model: string | null;
  notes: string | null;
  /** Uppercase colon-separated, sorted. */
  bssids: string[];
  createdAt: string;
  updatedAt: string;
}

export interface PlacedApInput {
  name: string;
  x: number;
  y: number;
  model: string | null;
  notes: string | null;
  bssids: string[];
}

export function pxPerMetre(s: FloorScale): number {
  return Math.hypot(s.x2 - s.x1, s.y2 - s.y1) / s.lengthM;
}
