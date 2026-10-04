import { useEffect, useMemo, useRef, useState } from "react";
import { api, asApiError } from "../../api/tauri";
import type { ApiError } from "../../types/wifi";
import type { Project } from "../../types/project";
import type { Building, Floor, PlacedAp } from "../../types/survey";
import type { HeatGrid } from "../../lib/heatmap";
import { floorNetworks } from "../../lib/heatmap";
import { computeHeat } from "../../lib/heatmapWorker";
import {
  REPORT_SIZE_LIMIT,
  buildReport,
  defaultSsids,
  rawCsv,
  rawJson,
  type RawFloor,
  type ReportAp,
  type ReportFloor,
  type ReportScope,
} from "../../lib/report";
import { ErrorBanner, NoticeBanner } from "../ErrorBanner";

type ScopeKind = ReportScope["kind"];
type Format = "html" | "csv" | "json";

const SCOPES: { id: ScopeKind; label: string }[] = [
  { id: "floor", label: "This floor" },
  { id: "building", label: "This building" },
  { id: "project", label: "Whole project" },
];

const MB = 1024 * 1024;

function base64(bytes: Uint8Array): string {
  let text = "";
  for (let i = 0; i < bytes.length; i += 0x8000) text += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(text);
}

const dataUri = (mime: string, buf: ArrayBuffer) => `data:${mime};base64,${base64(new Uint8Array(buf))}`;

/** Grid → small PNG (scaled up by the report's CSS/SVG). */
function encodePng(grid: HeatGrid): string {
  const c = document.createElement("canvas");
  c.width = grid.cols;
  c.height = grid.rows;
  c.getContext("2d")!.putImageData(new ImageData(new Uint8ClampedArray(grid.rgba), grid.cols, grid.rows), 0, 0);
  return c.toDataURL("image/png");
}

/** An error with what was being done when it happened. */
function during(what: string, e: unknown): ApiError {
  const err = asApiError(e);
  return { ...err, message: `${what}: ${err.message}` };
}

interface Loaded {
  /** Floors in scope with their points, in building then floor order. */
  floors: RawFloor[];
  /** Every placed AP in the project. */
  aps: ReportAp[];
}

/** Floors in scope (with points) and the project's APs. */
async function loadScope(project: Project, building: Building, floor: Floor, scope: ScopeKind): Promise<Loaded> {
  const buildings = await api.listBuildings(project.id);
  const floorsByBuilding = new Map<number, Floor[]>();
  await Promise.all(buildings.map(async (b) => floorsByBuilding.set(b.id, await api.listFloors(b.id))));
  const apLists = await Promise.all(buildings.map((b) => api.listBuildingAps(b.id)));
  const floorName = new Map([...floorsByBuilding.values()].flat().map((f) => [f.id, f.name]));
  const aps: ReportAp[] = buildings.flatMap((b, i) =>
    apLists[i]!.map((ap: PlacedAp) => ({ ap, buildingId: b.id, floorName: floorName.get(ap.floorId) ?? "?" })),
  );
  const inScope: { building: Building; floor: Floor }[] =
    scope === "floor"
      ? [{ building, floor }]
      : buildings
          .filter((b) => scope === "project" || b.id === building.id)
          .flatMap((b) => [...(floorsByBuilding.get(b.id) ?? [])].sort((x, y) => x.level - y.level || x.id - y.id).map((f) => ({ building: b, floor: f })));
  const floors = await Promise.all(
    inScope.map(async ({ building, floor }) => {
      try {
        return { building, floor, points: await api.listSurveyPoints(floor.id) };
      } catch (e) {
        throw during(`Could not load the points of ${building.name} / ${floor.name}`, e);
      }
    }),
  );
  return { floors, aps };
}

/**
 * Export: scope (floor, building, project), SSIDs to map, format. Builds
 * the report here (grids on the heatmap worker) and hands the bytes to Rust,
 * which asks where to save and writes the file.
 */
export function ExportDialog({
  project,
  building,
  floor,
  onClose,
}: {
  project: Project;
  building: Building;
  floor: Floor;
  onClose: () => void;
}) {
  const [scope, setScope] = useState<ScopeKind>("floor");
  const [format, setFormat] = useState<Format>("html");
  const [loaded, setLoaded] = useState<Loaded | null>(null);
  const [loadError, setLoadError] = useState<ApiError | null>(null);
  const [ssids, setSsids] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState<{ done: number; total: number; what: string } | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  /** A generated report over the size limit, waiting for "Save anyway". */
  const [oversize, setOversize] = useState<Uint8Array | null>(null);
  const [saved, setSaved] = useState<string | null>(null);
  const cancelled = useRef(false);

  useEffect(() => {
    let stale = false;
    setLoaded(null);
    setLoadError(null);
    setOversize(null);
    setSaved(null);
    loadScope(project, building, floor, scope).then(
      (l) => !stale && setLoaded(l),
      (e) => !stale && setLoadError(asApiError(e)),
    );
    return () => {
      stale = true;
    };
  }, [project, building, floor, scope]);

  const allPoints = useMemo(() => loaded?.floors.flatMap((f) => f.points) ?? [], [loaded]);
  const networks = useMemo(() => floorNetworks(allPoints).ssids, [allPoints]);
  const linkedDefault = useMemo(() => {
    if (!loaded) return [];
    const buildings = new Set(loaded.floors.map((f) => f.building.id));
    return defaultSsids(allPoints, loaded.aps.filter((a) => buildings.has(a.buildingId)).map((a) => a.ap));
  }, [loaded, allPoints]);
  // Default selection: SSIDs linked to placed APs, else the most widely heard one.
  useEffect(() => {
    setSsids(linkedDefault.length ? linkedDefault : networks.slice(0, 1).map((n) => n.ssid));
  }, [linkedDefault, networks]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !busy) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [busy, onClose]);

  const fileName = `${project.name} survey${scope === "floor" ? ` ${floor.name}` : scope === "building" ? ` ${building.name}` : ""}`;

  const save = async (kind: Format, bytes: Uint8Array) => {
    setProgress({ done: 1, total: 1, what: "Saving…" });
    const path = await api.saveExport(kind, bytes, fileName);
    if (path) setSaved(path);
  };

  const generateHtml = async (l: Loaded): Promise<Uint8Array | null> => {
    const step = (what: string, done = 0, total = 0) => setProgress({ done, total, what });
    const [app, branding, findings] = await Promise.all([
      api.appInfo(),
      api.getBranding(),
      api.surveyFindings({ kind: scope, id: scope === "floor" ? floor.id : scope === "building" ? building.id : project.id }),
    ]).catch((e) => {
      throw during("Could not load the project data", e);
    });
    const logoData = branding.logo ? dataUri(branding.logo.mime, await api.brandingLogo()) : null;
    const floors: ReportFloor[] = [];
    for (const [i, f] of l.floors.entries()) {
      if (cancelled.current) return null;
      const label = `${f.building.name} / ${f.floor.name}`;
      step(`Loading ${label} (${i + 1} of ${l.floors.length})…`, i, l.floors.length);
      try {
        const [annotations, requirements, plan, pointTests] = await Promise.all([
          api.floorAnnotations(f.floor.id),
          api.evaluateFloorRequirements(f.floor.id),
          f.floor.plan ? api.floorPlanImage(f.floor.id) : Promise.resolve(null),
          api.listFloorPointTests(f.floor.id),
        ]);
        const photoData = new Map<number, string>();
        for (const p of annotations.photos) photoData.set(p.id, dataUri("image/jpeg", await api.photoReportImage(p.id)));
        floors.push({
          ...f,
          annotations,
          requirements,
          pointTests,
          planData: plan && f.floor.plan ? dataUri(f.floor.plan.mime, plan) : null,
          photoData,
        });
      } catch (e) {
        throw during(`Could not load ${label}`, e);
      }
    }
    const html = await buildReport(
      {
        project,
        scope: { kind: scope, id: scope === "floor" ? floor.id : scope === "building" ? building.id : project.id },
        floors,
        aps: l.aps,
        findings,
        ssids,
        branding: { technicianName: branding.technicianName, companyName: branding.companyName, logoData },
        version: app.version,
        generatedAt: new Date(),
      },
      {
        compute: computeHeat,
        encodePng,
        onProgress: (done, total, what) => {
          if (cancelled.current) throw new Error("cancelled");
          step(`Maps: ${what}`, done, total);
        },
      },
    );
    return new TextEncoder().encode(html);
  };

  const run = async () => {
    if (!loaded || busy) return;
    setBusy(true);
    setError(null);
    setSaved(null);
    setOversize(null);
    cancelled.current = false;
    try {
      if (format === "csv") await save("csv", rawCsv(loaded.floors));
      else if (format === "json") {
        const app = await api.appInfo();
        await save("json", rawJson({ project, floors: loaded.floors, aps: loaded.aps.map((a) => a.ap), version: app.version, exportedAt: new Date().toISOString() }));
      } else {
        const bytes = await generateHtml(loaded);
        if (!bytes) return;
        if (bytes.length > REPORT_SIZE_LIMIT) setOversize(bytes);
        else await save("html", bytes);
      }
    } catch (e) {
      if (!cancelled.current) setError(e instanceof Error && !("kind" in e) ? { kind: "ipc", message: e.message } : asApiError(e));
    } finally {
      setBusy(false);
      setProgress(null);
    }
  };

  const saveOversize = async () => {
    if (!oversize) return;
    setBusy(true);
    setError(null);
    try {
      await save("html", oversize);
      setOversize(null);
    } catch (e) {
      setError(asApiError(e));
    } finally {
      setBusy(false);
      setProgress(null);
    }
  };

  const toggleSsid = (s: string) => setSsids((cur) => (cur.includes(s) ? cur.filter((x) => x !== s) : [...cur, s]));
  const pointsInScope = allPoints.length;
  const floorsWithoutPlan = loaded?.floors.filter((f) => !f.floor.plan).length ?? 0;

  return (
    <div className="modal-overlay" role="dialog" aria-modal="true" aria-label="Export" onClick={() => !busy && onClose()}>
      <section className="card export-dialog" onClick={(e) => e.stopPropagation()}>
        <header className="card-header">
          <h2>Export</h2>
          <button type="button" className="btn btn-small" onClick={onClose} disabled={busy}>
            Close
          </button>
        </header>
        <div className="panel-section">
          <span className="field-label">Scope</span>
          <div className="segmented">
            {SCOPES.map((s) => (
              <button key={s.id} type="button" className={scope === s.id ? "active" : ""} disabled={busy} onClick={() => setScope(s.id)}>
                {s.label}
              </button>
            ))}
          </div>
          <p className="muted small">
            {loaded
              ? `${loaded.floors.length} floor${loaded.floors.length === 1 ? "" : "s"}, ${pointsInScope} point${pointsInScope === 1 ? "" : "s"}.` +
                (floorsWithoutPlan ? ` ${floorsWithoutPlan} without a plan (no maps for those).` : "")
              : loadError
                ? ""
                : "Loading…"}
          </p>
          {loadError && <ErrorBanner error={loadError} compact />}

          <span className="field-label">Format</span>
          <div className="segmented">
            {(
              [
                ["html", "Report (HTML)"],
                ["csv", "Raw data (CSV)"],
                ["json", "Raw data (JSON)"],
              ] as [Format, string][]
            ).map(([id, label]) => (
              <button key={id} type="button" className={format === id ? "active" : ""} disabled={busy} onClick={() => setFormat(id)}>
                {label}
              </button>
            ))}
          </div>
          <p className="muted small">
            {format === "html"
              ? "One self-contained file: open it in your browser and use Print → Save as PDF (A4)."
              : "Every point and every reading, with the adapter that took it."}
          </p>

          {format === "html" && (
            <>
              <span className="field-label">SSIDs to map</span>
              {networks.length === 0 ? (
                <p className="muted small">No networks were heard in this scope.</p>
              ) : (
                <div className="export-ssids">
                  {networks.map((n) => (
                    <label key={n.ssid} className="checkbox">
                      <input type="checkbox" checked={ssids.includes(n.ssid)} disabled={busy} onChange={() => toggleSsid(n.ssid)} />
                      <span className="grow">{n.ssid}</span>
                      <span className="muted small">
                        {linkedDefault.includes(n.ssid) ? "placed AP · " : ""}
                        {n.points} pt{n.points === 1 ? "" : "s"}
                      </span>
                    </label>
                  ))}
                </div>
              )}
              <p className="muted small">
                {linkedDefault.length
                  ? "Preselected: the SSIDs broadcast by BSSIDs linked to placed APs. Each SSID adds a signal and a coverage map per floor."
                  : "No placed AP has linked BSSIDs here, so the most widely heard SSID is preselected."}
              </p>
            </>
          )}

          {progress && (
            <div className="export-progress" role="status">
              <progress value={progress.total ? progress.done : undefined} max={progress.total || undefined} />
              <span className="muted small">{progress.what}</span>
            </div>
          )}
          {error && <ErrorBanner error={error} />}
          {oversize && (
            <NoticeBanner>
              This report is {(oversize.length / MB).toFixed(1)} MB, over the {REPORT_SIZE_LIMIT / MB} MB that opens and prints
              comfortably. Fewer SSIDs, a smaller scope or fewer photos in the report make it smaller.
              <div className="panel-actions">
                <button type="button" className="btn" disabled={busy} onClick={() => void saveOversize()}>
                  Save anyway
                </button>
                <button type="button" className="btn" disabled={busy} onClick={() => setOversize(null)}>
                  Don't save
                </button>
              </div>
            </NoticeBanner>
          )}
          {saved && (
            <NoticeBanner>
              Saved to <span className="mono">{saved}</span>
              <div className="panel-actions">
                <button type="button" className="btn" onClick={() => api.openExport().catch((e) => setError(asApiError(e)))}>
                  {format === "html" ? "Open in browser" : "Open"}
                </button>
              </div>
            </NoticeBanner>
          )}

          <div className="panel-actions">
            <button
              type="button"
              className="btn btn-primary"
              disabled={busy || !loaded || pointsInScope === 0 || (format === "html" && ssids.length === 0 && networks.length > 0)}
              onClick={() => void run()}
            >
              {busy ? "Working…" : format === "html" ? "Create report…" : "Export…"}
            </button>
            {busy && (
              <button type="button" className="btn" onClick={() => (cancelled.current = true)}>
                Cancel
              </button>
            )}
          </div>
        </div>
      </section>
    </div>
  );
}
