// The only module that talks to the Rust backend.
import { invoke, isTauri } from "@tauri-apps/api/core";
import type {
  Adapter,
  AdapterId,
  AdapterListing,
  ApiError,
  ConnectionInfo,
  ScanRequest,
  ScanResult,
} from "../types/wifi";
import type { AppInfo, NewProject, Project } from "../types/project";
import type {
  Building,
  Floor,
  FloorScale,
  MeasureRequest,
  NewBuilding,
  NewFloor,
  PlacedAp,
  PlacedApInput,
  SurveyPoint,
} from "../types/survey";

function toApiError(e: unknown): ApiError {
  if (e && typeof e === "object" && "kind" in e && "message" in e) {
    return e as ApiError;
  }
  return { kind: "ipc", message: e instanceof Error ? e.message : String(e) };
}

async function call<T>(
  cmd: string,
  args?: Record<string, unknown> | Uint8Array,
  headers?: Record<string, string>,
): Promise<T> {
  if (!isTauri()) {
    throw {
      kind: "ipc",
      message: "Not running inside the Fresnel desktop app (start it with `npm run tauri dev`).",
    } satisfies ApiError;
  }
  try {
    return await invoke<T>(cmd, args, headers ? { headers } : undefined);
  } catch (e) {
    throw toApiError(e);
  }
}

/** An uncaught frontend error for the backend log (fields are length-capped there). */
export type FrontendErrorReport = {
  message: string;
  stack?: string;
  /** Where it was caught, e.g. "window.error app.js:12:3" or "unhandledrejection". */
  source: string;
};

export const api = {
  appInfo: () => call<AppInfo>("app_info"),
  /** Plain-text report for bug reports: versions, OS, adapters, DB state, recent log lines. */
  diagnosticsReport: () => call<string>("diagnostics_report"),
  logFrontendError: (report: FrontendErrorReport) => call<void>("log_frontend_error", report),

  listAdapters: () => call<AdapterListing>("list_adapters"),
  getAdapter: (adapterId: AdapterId) => call<Adapter>("get_adapter", { adapterId }),

  scan: (adapterId: AdapterId, request: ScanRequest = { trigger: true }) =>
    call<ScanResult>("scan", { adapterId, request }),
  lastScan: (adapterId: AdapterId) => call<ScanResult | null>("last_scan", { adapterId }),
  currentConnection: (adapterId: AdapterId) =>
    call<ConnectionInfo | null>("get_current_connection", { adapterId }),

  listProjects: () => call<Project[]>("list_projects"),
  createProject: (project: NewProject) => call<Project>("create_project", { project }),
  deleteProject: (id: number) => call<boolean>("delete_project", { id }),

  listBuildings: (projectId: number) => call<Building[]>("list_buildings", { projectId }),
  createBuilding: (building: NewBuilding) => call<Building>("create_building", { building }),
  deleteBuilding: (id: number) => call<boolean>("delete_building", { id }),

  listFloors: (buildingId: number) => call<Floor[]>("list_floors", { buildingId }),
  createFloor: (floor: NewFloor) => call<Floor>("create_floor", { floor }),
  deleteFloor: (id: number) => call<boolean>("delete_floor", { id }),
  /** `width`/`height`: the image's natural size as rendered here (the survey coordinate space). */
  importFloorPlan: (floorId: number, bytes: Uint8Array, width: number, height: number) =>
    call<Floor>("import_floor_plan", bytes, {
      "x-floor-id": String(floorId),
      "x-plan-width": String(width),
      "x-plan-height": String(height),
    }),
  floorPlanImage: (floorId: number) => call<ArrayBuffer>("floor_plan_image", { floorId }),
  setFloorScale: (floorId: number, scale: FloorScale | null) =>
    call<Floor>("set_floor_scale", { floorId, scale }),

  listSurveyPoints: (floorId: number) => call<SurveyPoint[]>("list_survey_points", { floorId }),
  measureHere: (request: MeasureRequest) => call<SurveyPoint>("measure_here", { request }),
  deleteSurveyPoint: (id: number) => call<boolean>("delete_survey_point", { id }),

  listBuildingAps: (buildingId: number) => call<PlacedAp[]>("list_building_aps", { buildingId }),
  createPlacedAp: (floorId: number, ap: PlacedApInput) => call<PlacedAp>("create_placed_ap", { floorId, ap }),
  updatePlacedAp: (id: number, ap: PlacedApInput) => call<PlacedAp>("update_placed_ap", { id, ap }),
  deletePlacedAp: (id: number) => call<boolean>("delete_placed_ap", { id }),
};

export function isApiError(e: unknown): e is ApiError {
  return !!e && typeof e === "object" && "kind" in e && "message" in e;
}

export function asApiError(e: unknown): ApiError {
  return toApiError(e);
}
