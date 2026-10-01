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

function toApiError(e: unknown): ApiError {
  if (e && typeof e === "object" && "kind" in e && "message" in e) {
    return e as ApiError;
  }
  return { kind: "ipc", message: e instanceof Error ? e.message : String(e) };
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri()) {
    throw {
      kind: "ipc",
      message: "Not running inside the Fresnel desktop app (start it with `npm run tauri dev`).",
    } satisfies ApiError;
  }
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    throw toApiError(e);
  }
}

export const api = {
  appInfo: () => call<AppInfo>("app_info"),

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
};

export function isApiError(e: unknown): e is ApiError {
  return !!e && typeof e === "object" && "kind" in e && "message" in e;
}

export function asApiError(e: unknown): ApiError {
  return toApiError(e);
}
