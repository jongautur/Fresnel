import { useEffect, useMemo, useRef, useState, type ChangeEvent } from "react";
import { api, asApiError } from "../../api/tauri";
import { useWifi } from "../../state/WifiContext";
import type { ApiError, Band } from "../../types/wifi";
import type { FloorRequirements } from "../../types/requirements";
import type { Project } from "../../types/project";
import type { Building } from "../../types/survey";
import type { PointTest, TestSettings } from "../../types/pointTests";
import {
  pxPerMetre,
  type Floor,
  type FloorScale,
  type PlacedAp,
  type PlacedApInput,
  type SurveyPoint,
} from "../../types/survey";
import { AdapterSelector } from "../AdapterSelector";
import { ErrorBanner, NoticeBanner } from "../ErrorBanner";
import { IconRadar } from "../Icons";
import { PlanCanvas, type ApMarker, type CanvasMode, type PlanXY } from "./PlanCanvas";
import { ApEditor } from "./ApEditor";
import { NotesPanel } from "./NotesPanel";
import type { NotePin } from "../../types/notes";
import { FindingsPanel } from "./FindingsPanel";
import { ExportDialog } from "./ExportDialog";
import { PointDetails } from "./PointDetails";
import { PointTestsTable } from "./PointTestsTable";
import { plannedTests } from "../../lib/pointTests";
import { BANDS, HeatmapControls, METRICS, THRESHOLD_MAX, THRESHOLD_MIN } from "./HeatmapControls";
import { PointRequirements, RequirementsLegend, RequirementsPanel } from "./RequirementsPanel";
import {
  NOT_HEARD_DBM,
  apColor,
  servingAt,
  servingInputs,
  estimateAt,
  floorNetworks,
  influenceRadiusPx,
  pointValue,
  type HeatGrid,
  type HeatMetric,
  type ServingGrid,
  type HeatPoint,
  type HeatmapConfig,
  type NetworkFilter,
} from "../../lib/heatmap";
import { areaInputs, outcomeMark, requirementsAt } from "../../lib/requirements";
import type { HeatJob } from "../../lib/heatmapJobs";
import { useHeatGrid } from "../../lib/heatmapWorker";

// ---------------------------------------------------------------------------
// Plan file reading
// ---------------------------------------------------------------------------

/** Below this, SVG coordinate spaces are scaled up so the plan stays sharp when zoomed. */
const SVG_MIN_SIDE = 2000;
/** The backend's import limit. */
const MAX_PLAN_BYTES = 64 * 1024 * 1024;
/** Beyond these the webview can run out of memory drawing and zooming the plan. */
const MAX_PLAN_SIDE = 16384;
const MAX_PLAN_PIXELS = 100_000_000;

function loadImage(url: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error("The file could not be read as an image."));
    img.src = url;
  });
}

function svgViewBox(text: string): [number, number] | null {
  const tag = /<svg\b[^>]*>/i.exec(text)?.[0];
  const vb = tag && /viewBox\s*=\s*["']([^"']+)["']/i.exec(tag)?.[1];
  if (!vb) return null;
  const n = vb.trim().split(/[\s,]+/).map(Number);
  return n.length === 4 && n[2]! > 0 && n[3]! > 0 ? [n[2]!, n[3]!] : null;
}

/**
 * Read a plan file and measure it the way this webview renders it (EXIF
 * rotation, SVG sizing). That rendered size becomes the survey coordinate space.
 */
async function readPlanFile(file: File): Promise<{ bytes: Uint8Array; width: number; height: number }> {
  if (file.size > MAX_PLAN_BYTES) {
    throw new Error(
      `The plan file is ${Math.round(file.size / (1024 * 1024))} MB; the limit is ${MAX_PLAN_BYTES / (1024 * 1024)} MB. Export it at a lower resolution or as JPEG.`,
    );
  }
  const bytes = new Uint8Array(await file.arrayBuffer());
  const isSvg = file.type === "image/svg+xml" || /\.svg$/i.test(file.name);
  const url = URL.createObjectURL(new Blob([bytes], { type: isSvg ? "image/svg+xml" : file.type }));
  try {
    const img = await loadImage(url);
    let w = img.naturalWidth;
    let h = img.naturalHeight;
    if (isSvg) {
      const viewBox = svgViewBox(new TextDecoder().decode(bytes.subarray(0, 65536)));
      if ((!w || !h) && viewBox) [w, h] = viewBox;
      // Only an SVG with a viewBox scales its content to the box it's drawn in.
      const long = Math.max(w, h);
      if (viewBox && long > 0 && long < SVG_MIN_SIDE) {
        w *= SVG_MIN_SIDE / long;
        h *= SVG_MIN_SIDE / long;
      } else if (viewBox && w > 0 && h > 0) {
        const f = Math.min(1, MAX_PLAN_SIDE / long, Math.sqrt(MAX_PLAN_PIXELS / (w * h)));
        w *= f;
        h *= f;
      }
    }
    if (!w || !h) throw new Error("Could not determine the image size (an SVG needs width/height or a viewBox).");
    if (Math.round(w) > MAX_PLAN_SIDE || Math.round(h) > MAX_PLAN_SIDE || w * h > MAX_PLAN_PIXELS) {
      throw new Error(
        `The plan is ${Math.round(w)} × ${Math.round(h)} px (${Math.round((w * h) / 1e6)} MP). Plans can be at most ${MAX_PLAN_SIDE} px on a side and ${MAX_PLAN_PIXELS / 1e6} MP. Scale it down in an image editor and import it again.`,
      );
    }
    return { bytes, width: Math.round(w), height: Math.round(h) };
  } finally {
    URL.revokeObjectURL(url);
  }
}

function parseMetres(s: string): number | null {
  const v = Number(s.trim().replace(",", "."));
  return Number.isFinite(v) && v > 0 ? v : null;
}

const HEAT_KEY = "fresnel.survey.heatmap";

interface HeatPrefs {
  on: boolean;
  metric: HeatMetric;
  band: Band | "all";
  coverage: number;
  overlap: number;
}

/** Stored prefs are checked field by field; anything unknown falls back to the default. */
function loadHeatPrefs(): HeatPrefs {
  const d: HeatPrefs = { on: false, metric: "signal", band: "all", coverage: -67, overlap: -75 };
  let s: Record<string, unknown>;
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(HEAT_KEY) ?? "{}");
    if (!parsed || typeof parsed !== "object") return d;
    s = parsed as Record<string, unknown>;
  } catch {
    return d;
  }
  const threshold = (v: unknown, fallback: number) =>
    typeof v === "number" && Number.isFinite(v)
      ? Math.min(THRESHOLD_MAX, Math.max(THRESHOLD_MIN, Math.round(v)))
      : fallback;
  return {
    on: typeof s.on === "boolean" ? s.on : d.on,
    metric: METRICS.find((m) => m.id === s.metric)?.id ?? d.metric,
    band: BANDS.find((b) => b === s.band) ?? d.band,
    coverage: threshold(s.coverage, d.coverage),
    overlap: threshold(s.overlap, d.overlap),
  };
}

const isTyping = (t: EventTarget | null) =>
  t instanceof HTMLElement && (t.isContentEditable || /^(INPUT|SELECT|TEXTAREA|BUTTON)$/.test(t.tagName));

// ---------------------------------------------------------------------------
// Workspace
// ---------------------------------------------------------------------------

const RUN_TESTS_KEY = "fresnel.runTests";

function loadRunTests(): boolean {
  try {
    return localStorage.getItem(RUN_TESTS_KEY) === "1";
  } catch {
    return false;
  }
}

export function FloorWorkspace({
  floor,
  project,
  building,
  onFloorChange,
}: {
  floor: Floor;
  project: Project;
  building: Building;
  /** Updater form, so a result that arrives late (after a scan) applies to the current floor, not a snapshot. */
  onFloorChange: (update: (f: Floor) => Floor) => void;
}) {
  const { selectedAdapter, holdAutoScan } = useWifi();
  const [points, setPoints] = useState<SurveyPoint[]>([]);
  const [imageUrl, setImageUrl] = useState<string | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const [mode, setMode] = useState<CanvasMode>("measure");
  const [pending, setPending] = useState<PlanXY | null>(null);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [measuring, setMeasuring] = useState(false);
  const [mismatch, setMismatch] = useState<string | null>(null);
  // Active tests: "+ run tests" toggle, the floor's results, the run in progress.
  const [runTests, setRunTestsState] = useState(loadRunTests);
  const [testSettings, setTestSettings] = useState<TestSettings | null>(null);
  const [pointTests, setPointTests] = useState<PointTest[]>([]);
  const [testing, setTesting] = useState<{ testId: string; pointId: number } | null>(null);
  const [testError, setTestError] = useState<ApiError | null>(null);
  const floorIdRef = useRef(floor.id);
  floorIdRef.current = floor.id;
  const testingRef = useRef<string | null>(null);
  const [importing, setImporting] = useState(false);
  const [exportOpen, setExportOpen] = useState(false);
  const [scaleA, setScaleA] = useState<PlanXY | null>(null);
  const [scaleB, setScaleB] = useState<PlanXY | null>(null);
  const [lengthText, setLengthText] = useState("");
  const fileInput = useRef<HTMLInputElement>(null);
  const lengthInput = useRef<HTMLInputElement>(null);
  const [heatPrefs, setHeatPrefsState] = useState<HeatPrefs>(loadHeatPrefs);
  const [network, setNetwork] = useState<NetworkFilter | null>(null);
  const [allAps, setAllAps] = useState<PlacedAp[]>([]);
  const [selectedApId, setSelectedApId] = useState<number | null>(null);
  const [pendingAp, setPendingAp] = useState<PlanXY | null>(null);
  const [requirements, setRequirements] = useState<FloorRequirements | null>(null);
  const [requirementsRev, setRequirementsRev] = useState(0);
  const [profileLevels, setProfileLevels] = useState<{ coverage: number; overlap: number } | null>(null);
  const [pins, setPins] = useState<NotePin[]>([]);
  const [selectedPinId, setSelectedPinId] = useState<number | null>(null);
  const [pendingPin, setPendingPin] = useState<PlanXY | null>(null);
  // Findings: side panel, points to highlight, BSSIDs to prefill in the AP editor.
  const [showFindings, setShowFindings] = useState(false);
  const [flagged, setFlagged] = useState<Map<number, string> | null>(null);
  const [apPrefill, setApPrefill] = useState<string[]>([]);
  const setHeat = (patch: Partial<HeatPrefs>) =>
    setHeatPrefsState((h) => {
      const next = { ...h, ...patch };
      try {
        localStorage.setItem(HEAT_KEY, JSON.stringify(next));
      } catch {
        /* non-essential */
      }
      return next;
    });

  const plan = floor.plan;
  const ppm = floor.scale ? pxPerMetre(floor.scale) : null;

  // Points
  useEffect(() => {
    api
      .listSurveyPoints(floor.id)
      .then(setPoints)
      .catch((e) => setError(asApiError(e)));
  }, [floor.id]);

  // Active tests
  useEffect(() => {
    setPointTests([]);
    setTestError(null);
    api
      .listFloorPointTests(floor.id)
      .then(setPointTests)
      .catch((e) => setError(asApiError(e)));
  }, [floor.id]);
  useEffect(() => {
    if (!runTests) return;
    api.getTestSettings().then(setTestSettings, (e) => setTestError(asApiError(e)));
  }, [runTests]);
  // Leaving the workspace stops a run: its results would describe a spot nobody is at.
  useEffect(
    () => () => {
      if (testingRef.current) void api.cancelActiveTest(testingRef.current).catch(() => {});
    },
    [],
  );
  const setRunTests = (on: boolean) => {
    setRunTestsState(on);
    try {
      localStorage.setItem(RUN_TESTS_KEY, on ? "1" : "0");
    } catch {
      /* non-essential */
    }
  };

  // Note pins
  useEffect(() => {
    setSelectedPinId(null);
    setPendingPin(null);
    api
      .listNotePins(floor.id)
      .then(setPins)
      .catch((e) => setError(asApiError(e)));
  }, [floor.id]);

  // Access points: the whole building's (names resolve across floors)
  useEffect(() => {
    api
      .listBuildingAps(floor.buildingId)
      .then(setAllAps)
      .catch((e) => setError(asApiError(e)));
  }, [floor.buildingId]);

  // Requirements: re-evaluated in Rust whenever points, APs or profiles change.
  useEffect(() => {
    let cancelled = false;
    api.evaluateFloorRequirements(floor.id).then(
      (r) => !cancelled && setRequirements(r),
      (e) => !cancelled && setError(asApiError(e)),
    );
    return () => {
      cancelled = true;
    };
  }, [floor.id, points, allAps, requirementsRev]);
  const reqProfile = requirements?.profile ?? null;
  // Coverage and overlap levels start at the profile's while one applies.
  const profileKey = reqProfile ? `${reqProfile.id}@${reqProfile.updatedAt}` : null;
  useEffect(() => {
    setProfileLevels(
      reqProfile
        ? { coverage: reqProfile.primaryMinDbm, overlap: reqProfile.secondaryMinDbm ?? reqProfile.primaryMinDbm }
        : null,
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [profileKey]);

  // Plan image → blob URL
  const planFile = plan?.file;
  const planMime = plan?.mime;
  useEffect(() => {
    setImageUrl(null);
    if (!planFile) return;
    let url: string | null = null;
    let cancelled = false;
    api
      .floorPlanImage(floor.id)
      .then((buf) => {
        if (cancelled) return;
        url = URL.createObjectURL(new Blob([buf], { type: planMime }));
        setImageUrl(url);
      })
      .catch((e) => !cancelled && setError(asApiError(e)));
    return () => {
      cancelled = true;
      if (url) URL.revokeObjectURL(url);
    };
  }, [floor.id, planFile, planMime]);

  const floorAdapters = useMemo(() => {
    const seen = new Map<string, SurveyPoint["adapter"]>();
    for (const p of points) seen.set(`${p.adapter.id}|${p.adapter.hwId ?? ""}`, p.adapter);
    return [...seen.values()];
  }, [points]);
  const adapterDiffers =
    selectedAdapter != null && floorAdapters.length > 0 && !floorAdapters.some((a) => a.id === selectedAdapter.id);

  const selected = points.find((p) => p.id === selectedId) ?? null;

  // --- Access points ---------------------------------------------------------

  // Colour follows the AP (building order by id), never its rank.
  const coloredAps = useMemo(
    () => [...allAps].sort((a, b) => a.id - b.id).map((ap, i) => ({ ap, color: apColor(i) })),
    [allAps],
  );
  const floorAps = coloredAps.filter(({ ap }) => ap.floorId === floor.id);
  const apMarkers: ApMarker[] = floorAps.map(({ ap, color }) => ({ id: ap.id, name: ap.name, x: ap.x, y: ap.y, color }));
  const apNameByBssid = useMemo(() => {
    const m = new Map<string, string>();
    for (const ap of allAps) for (const b of ap.bssids) m.set(b, ap.name);
    return m;
  }, [allAps]);
  const selectedAp = allAps.find((a) => a.id === selectedApId) ?? null;

  const saveAp = async (input: PlacedApInput, id: number | null): Promise<boolean> => {
    try {
      const saved = id == null ? await api.createPlacedAp(floor.id, input) : await api.updatePlacedAp(id, input);
      setAllAps((aps) => (id == null ? [...aps, saved] : aps.map((a) => (a.id === id ? saved : a))));
      if (id == null) setPendingAp(null);
      setError(null);
      return true;
    } catch (e) {
      setError(asApiError(e));
      return false;
    }
  };

  const moveAp = (id: number, p: PlanXY) => {
    const ap = allAps.find((a) => a.id === id);
    if (!ap) return;
    void saveAp({ name: ap.name, x: p.x, y: p.y, model: ap.model, notes: ap.notes, bssids: ap.bssids }, id);
  };

  const removeAp = async (id: number) => {
    try {
      await api.deletePlacedAp(id);
      setAllAps((aps) => aps.filter((a) => a.id !== id));
      setSelectedApId(null);
    } catch (e) {
      setError(asApiError(e));
    }
  };

  // Reload findings whenever points or AP links change.
  const findingsRev = useMemo(
    () => JSON.stringify([points.map((p) => p.id), allAps.map((a) => [a.id, a.bssids])]),
    [points, allAps],
  );

  // --- Heatmap -------------------------------------------------------------

  const networks = useMemo(() => floorNetworks(points), [points]);
  // Default: the network the surveyor was connected to, else the most widely heard.
  const defaultNetwork = useMemo((): NetworkFilter => {
    const connected = points.flatMap((p) => p.samples).find((s) => s.isConnected && s.ssid != null)?.ssid;
    const ssid = connected ?? networks.ssids[0]?.ssid;
    return ssid != null ? { kind: "ssid", ssid } : { kind: "any" };
  }, [points, networks]);
  // An AP filter always uses the AP's current BSSIDs (and falls back if it was deleted).
  const resolvedNetwork = ((): NetworkFilter => {
    if (network?.kind !== "ap") return network ?? defaultNetwork;
    const ap = allAps.find((a) => a.id === network.apId);
    return ap ? { kind: "ap", apId: ap.id, bssids: ap.bssids } : defaultNetwork;
  })();
  const servable = coloredAps.filter(({ ap }) => ap.bssids.length > 0);
  const metric: HeatMetric =
    (heatPrefs.metric === "serving" && servable.length === 0) || (heatPrefs.metric === "requirements" && !reqProfile)
      ? "signal"
      : heatPrefs.metric;
  const heatCfg: HeatmapConfig = {
    metric,
    network: resolvedNetwork,
    band: heatPrefs.band,
    threshold:
      metric === "overlap" || metric === "serving"
        ? (profileLevels?.overlap ?? heatPrefs.overlap)
        : (profileLevels?.coverage ?? heatPrefs.coverage),
  };
  const heatActive = heatPrefs.on && mode !== "scale" && plan != null && points.length > 0;
  const cfgKey = JSON.stringify(heatCfg);
  const heatPoints = useMemo(() => {
    const out: (HeatPoint & { id: number })[] = [];
    for (const p of points) {
      const v = pointValue(p, heatCfg);
      if (v != null) out.push({ id: p.id, x: p.x, y: p.y, v });
    }
    return out;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [points, cfgKey]);
  const serving = useMemo(
    () => (metric === "serving" ? servingInputs(points, servable, heatPrefs.band) : []),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [metric, points, coloredAps, heatPrefs.band],
  );
  // Requirements area estimate: also feeds the floor summary, so built whether or not it is shown.
  const reqInputs = useMemo(
    () => (requirements?.profile ? areaInputs(points, requirements.points, requirements.profile) : null),
    [requirements, points],
  );
  // Grids are built on a worker; each keeps showing the previous one until the new one is ready.
  const reqGrid = useHeatGrid(
    (): HeatJob | null => (plan && reqInputs ? { kind: "requirements", plan, ppm, inputs: reqInputs } : null),
    [plan, ppm, reqInputs],
  ).grid;
  const reqEval = (id: number) => requirements?.points.find((e) => e.pointId === id);
  const viewGrid = useHeatGrid((): HeatJob | null => {
    if (!heatActive || !plan || metric === "requirements") return null;
    return metric === "serving"
      ? { kind: "serving", plan, ppm, inputs: serving, threshold: heatCfg.threshold }
      : { kind: "grid", plan, ppm, pts: heatPoints, cfg: heatCfg };
  }, [heatActive, plan, ppm, heatPoints, serving, cfgKey]).grid;
  const heatGrid: HeatGrid | ServingGrid | null = !heatActive ? null : metric === "requirements" ? reqGrid : viewGrid;
  const fmtValue = (v: number) =>
    heatCfg.metric === "overlap"
      ? `${Math.round(v)} AP${Math.round(v) === 1 ? "" : "s"}`
      : v <= NOT_HEARD_DBM
        ? "not heard"
        : `${Math.round(v)} dBm`;
  const pointLabels = useMemo(() => {
    if (!heatActive) return undefined;
    const m = new Map<number, string>();
    if (metric === "requirements") {
      for (const p of points) m.set(p.id, outcomeMark(reqEval(p.id)));
      return m;
    }
    if (metric === "serving") {
      // The strongest reading from any placed AP at each point.
      const placed = new Set(servable.flatMap(({ ap }) => ap.bssids));
      for (const p of points) {
        const d = p.samples
          .filter((s) => placed.has(s.bssid) && s.signal.dbm != null && (heatPrefs.band === "all" || s.band === heatPrefs.band))
          .map((s) => s.signal.dbm!);
        m.set(p.id, d.length ? String(Math.round(Math.max(...d))) : "—");
      }
      return m;
    }
    for (const h of heatPoints)
      m.set(h.id, heatCfg.metric === "overlap" ? String(h.v) : h.v <= NOT_HEARD_DBM ? "—" : String(Math.round(h.v)));
    return m;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [heatActive, heatPoints, requirements, cfgKey]);
  const hoverInfo = (p: PlanXY) => {
    if (!heatActive || !plan) return null;
    if (metric === "requirements") {
      const e = reqInputs && requirementsAt(reqInputs, p.x, p.y, influenceRadiusPx(plan, ppm));
      if (!e) return "no estimate (too far from points)";
      return e.pass ? "≈ meets the profile (estimate)" : `≈ fails: ${e.failing.join(", ")} (estimate)`;
    }
    if (metric === "serving") {
      const e = servingAt(serving, p.x, p.y, influenceRadiusPx(plan, ppm));
      if (!e) return "no estimate (too far from points)";
      const [first, second] = e.ranked;
      if (first!.value < heatCfg.threshold) return `no AP at ${heatCfg.threshold} dBm (best ${first!.ap.name} ≈ ${fmtValue(first!.value)})`;
      return `${first!.ap.name} ≈ ${fmtValue(first!.value)}${second ? ` · next ${second.ap.name} ≈ ${fmtValue(second.value)}` : ""}`;
    }
    const e = estimateAt(heatPoints, p.x, p.y, influenceRadiusPx(plan, ppm));
    if (!e) return "no estimate (too far from points)";
    const nearestNo = points.findIndex((q) => q.id === (e.nearest as HeatPoint & { id: number }).id) + 1;
    const dist = ppm ? `${(e.nearestPx / ppm).toFixed(1)} m` : `${Math.round(e.nearestPx)} px`;
    return `≈ ${fmtValue(e.value)} (point ${nearestNo}: ${fmtValue(e.nearest.v)}, ${dist} away)`;
  };

  // --- Actions -------------------------------------------------------------

  const measure = async (allowAdapterChange = false) => {
    if (!pending || !selectedAdapter || measuring) return;
    setMeasuring(true);
    setError(null);
    setMismatch(null);
    const releaseAutoScan = holdAutoScan();
    try {
      const point = await api.measureHere({
        floorId: floor.id,
        x: pending.x,
        y: pending.y,
        adapterId: selectedAdapter.id,
        allowAdapterChange,
      });
      setPoints((ps) => [...ps, point]);
      setSelectedId(point.id);
      setPending(null);
      onFloorChange((f) => ({ ...f, pointCount: f.pointCount + 1 }));
      // After the scan, so the tests' traffic can't disturb it.
      if (runTests) void runTestsAt(point.id);
    } catch (e) {
      const err = asApiError(e);
      if (err.kind === "adapter_mismatch") setMismatch(err.message);
      else setError(err);
    } finally {
      releaseAutoScan();
      setMeasuring(false);
    }
  };

  const runTestsAt = async (pointId: number) => {
    const testId = `tests-${pointId}-${Date.now()}`;
    const floorId = floor.id;
    testingRef.current = testId;
    setTesting({ testId, pointId });
    setTestError(null);
    // No background scans while tests run: they'd disturb throughput and latency.
    const releaseAutoScan = holdAutoScan();
    try {
      const done = await api.runPointTests(pointId, testId);
      if (floorIdRef.current === floorId) setPointTests((ts) => [...ts, ...done]);
    } catch (e) {
      if (floorIdRef.current === floorId) setTestError(asApiError(e));
    } finally {
      releaseAutoScan();
      testingRef.current = null;
      setTesting(null);
    }
  };

  const cancelTests = () => {
    if (testing) void api.cancelActiveTest(testing.testId).catch((e) => setTestError(asApiError(e)));
  };

  const removePoint = async (id: number) => {
    try {
      await api.deleteSurveyPoint(id);
      setPoints((ps) => ps.filter((p) => p.id !== id));
      setPointTests((ts) => ts.filter((t) => t.pointId !== id));
      setSelectedId(null);
      onFloorChange((f) => ({ ...f, pointCount: Math.max(0, f.pointCount - 1) }));
    } catch (e) {
      setError(asApiError(e));
    }
  };

  const importPlan = async (e: ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    e.target.value = "";
    if (!file) return;
    setImporting(true);
    setError(null);
    try {
      const { bytes, width, height } = await readPlanFile(file);
      const updated = await api.importFloorPlan(floor.id, bytes, width, height);
      setPending(null);
      setMode("measure");
      onFloorChange(() => updated);
    } catch (err) {
      setError(err instanceof Error ? { kind: "invalid_input", message: err.message } : asApiError(err));
    } finally {
      setImporting(false);
    }
  };

  const enterScaleMode = () => {
    setMode("scale");
    setPending(null);
    setSelectedId(null);
    setScaleA(floor.scale ? { x: floor.scale.x1, y: floor.scale.y1 } : null);
    setScaleB(floor.scale ? { x: floor.scale.x2, y: floor.scale.y2 } : null);
    setLengthText(floor.scale ? String(floor.scale.lengthM) : "");
  };

  const saveScale = async () => {
    const lengthM = parseMetres(lengthText);
    if (!scaleA || !scaleB || lengthM == null) return;
    const scale: FloorScale = { x1: scaleA.x, y1: scaleA.y, x2: scaleB.x, y2: scaleB.y, lengthM };
    try {
      const updated = await api.setFloorScale(floor.id, scale);
      onFloorChange(() => updated);
      setMode("measure");
      setError(null);
    } catch (e) {
      setError(asApiError(e));
    }
  };

  const clearScale = async () => {
    try {
      const updated = await api.setFloorScale(floor.id, null);
      onFloorChange(() => updated);
      setScaleA(null);
      setScaleB(null);
      setLengthText("");
    } catch (e) {
      setError(asApiError(e));
    }
  };

  const onPlanClick = (p: PlanXY) => {
    if (mode === "pins") {
      setPendingPin(p);
      setSelectedPinId(null);
      return;
    }
    if (showFindings && mode === "measure") return;
    if (mode === "aps") {
      setPendingAp(p);
      setSelectedApId(null);
      return;
    }
    if (mode === "scale") {
      if (!scaleA || scaleB) {
        setScaleA(p);
        setScaleB(null);
      } else {
        setScaleB(p);
      }
      return;
    }
    if (measuring) return;
    setPending(p);
    setSelectedId(null);
    setMismatch(null);
  };

  // Second scale point placed → straight to typing the length.
  useEffect(() => {
    if (mode === "scale" && scaleB) lengthInput.current?.focus();
  }, [mode, scaleB]);

  // Enter = measure, Escape = cancel (unless typing in a field). A running
  // measurement can't be cancelled, so Escape does nothing until it is saved.
  const measureRef = useRef(measure);
  measureRef.current = measure;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (isTyping(e.target)) return;
      if (e.key === "Enter" && mode === "measure") {
        e.preventDefault();
        void measureRef.current();
      } else if (e.key === "Escape" && !measuring) {
        setPendingPin(null);
        setSelectedPinId(null);
        setPendingAp(null);
        setSelectedApId(null);
        setPending(null);
        setSelectedId(null);
        setMismatch(null);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [mode, measuring]);

  // --- Render --------------------------------------------------------------

  const fileButton = (
    <>
      <input
        ref={fileInput}
        type="file"
        accept=".png,.jpg,.jpeg,.svg,image/png,image/jpeg,image/svg+xml"
        hidden
        onChange={(e) => void importPlan(e)}
      />
      <button
        type="button"
        className={plan ? "btn" : "btn btn-primary"}
        onClick={() => fileInput.current?.click()}
        disabled={importing || measuring || (plan != null && (floor.pointCount > 0 || floorAps.length > 0))}
        title={
          plan && (floor.pointCount > 0 || floorAps.length > 0)
            ? "Points or access points are placed on the current plan. Delete them or create a new floor to use another plan."
            : "PNG, JPEG or SVG"
        }
      >
        {importing ? "Importing…" : plan ? "Replace plan" : "Import floor plan"}
      </button>
    </>
  );

  if (!plan) {
    return (
      <>
        {error && <ErrorBanner error={error} />}
        <section className="card survey-empty">
          <h2>{floor.name} has no floor plan yet</h2>
          <p className="muted">
            Import a PNG, JPEG or SVG. The image is copied into Fresnel's data folder, so moving or deleting the
            original later doesn't affect the project. For a PDF, export the page as PNG first.
          </p>
          {fileButton}
        </section>
      </>
    );
  }

  const scaleLength = parseMetres(lengthText);
  const scaleLabel =
    scaleA && scaleB
      ? scaleLength != null
        ? `${scaleLength} m`
        : `${Math.round(Math.hypot(scaleB.x - scaleA.x, scaleB.y - scaleA.y))} px`
      : null;
  const iface = (id: string) => id.split(":").slice(1).join(":") || id;

  return (
    <>
      <div className="workspace-toolbar">
        <AdapterSelector />
        <div
          className="segmented"
          role="tablist"
          aria-label="Tool"
          title={measuring ? "Wait for the measurement to finish" : undefined}
        >
          <button
            type="button"
            role="tab"
            aria-selected={mode === "measure"}
            disabled={measuring}
            className={mode === "measure" ? "active" : ""}
            onClick={() => setMode("measure")}
          >
            Measure
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={mode === "aps"}
            disabled={measuring}
            className={mode === "aps" ? "active" : ""}
            onClick={() => {
              setMode("aps");
              setPending(null);
              setSelectedId(null);
            }}
          >
            Access points
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={mode === "pins"}
            disabled={measuring}
            className={mode === "pins" ? "active" : ""}
            onClick={() => {
              setMode("pins");
              setPending(null);
              setSelectedId(null);
            }}
          >
            Notes
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={mode === "scale"}
            disabled={measuring}
            className={mode === "scale" ? "active" : ""}
            onClick={enterScaleMode}
          >
            Set scale
          </button>
        </div>
        <button
          type="button"
          className={`btn ${heatPrefs.on ? "btn-toggled" : ""}`}
          aria-pressed={heatPrefs.on}
          onClick={() => {
            setHeat({ on: !heatPrefs.on });
            setMode("measure");
          }}
          disabled={points.length === 0}
          title={points.length === 0 ? "Measure some points first" : "Show an interpolated heatmap over the plan"}
        >
          Heatmap
        </button>
        <button
          type="button"
          className={`btn ${showFindings ? "btn-toggled" : ""}`}
          aria-pressed={showFindings}
          onClick={() => {
            setShowFindings(!showFindings);
            setMode("measure");
            setPending(null);
          }}
          title="Unknown transmitters, security mismatches and other rogue-AP findings"
        >
          Findings
        </button>
        <button
          type="button"
          className="btn"
          onClick={() => setExportOpen(true)}
          title="Survey report (HTML, prints to PDF) and raw data (CSV, JSON)"
        >
          Export…
        </button>
        {fileButton}
        <div className="workspace-meta">
          {points.length} point{points.length === 1 ? "" : "s"}
          <span className="sep">·</span>
          {ppm ? `${ppm.toFixed(1)} px/m` : "not scaled"}
        </div>
      </div>

      {error && <ErrorBanner error={error} />}

      <div className="workspace">
        <PlanCanvas
          plan={plan}
          imageUrl={imageUrl}
          mode={mode}
          points={points}
          selectedId={selectedId}
          pending={pending}
          measuring={measuring}
          scale={mode === "scale" ? { a: scaleA, b: scaleB, label: scaleLabel } : null}
          pxPerMetre={ppm}
          heat={heatGrid}
          pointLabels={showFindings && mode === "measure" && flagged ? flagged : pointLabels}
          highlightIds={showFindings && mode === "measure" && flagged ? new Set(flagged.keys()) : null}
          aps={apMarkers}
          selectedApId={selectedApId}
          pendingAp={pendingAp}
          onApClick={(id) => {
            setSelectedApId(id);
            setPendingAp(null);
            setApPrefill([]);
          }}
          onApMove={moveAp}
          pins={pins}
          selectedPinId={selectedPinId}
          pendingPin={pendingPin}
          onPinClick={(id) => {
            setSelectedPinId(id);
            setPendingPin(null);
          }}
          hoverInfo={heatActive ? hoverInfo : undefined}
          onPlanClick={onPlanClick}
          onPointClick={(id) => {
            if (measuring) return;
            setSelectedId(id);
            setPending(null);
            setMismatch(null);
          }}
        />

        <aside className="survey-panel">
          {mode === "pins" ? (
            <NotesPanel
              floor={floor}
              pins={pins}
              setPins={setPins}
              selectedPinId={selectedPinId}
              setSelectedPinId={setSelectedPinId}
              pendingPin={pendingPin}
              setPendingPin={setPendingPin}
            />
          ) : mode === "aps" ? (
            pendingAp || selectedAp ? (
              <ApEditor
                key={`${selectedAp ? `ap-${selectedAp.id}` : `new-${pendingAp!.x}-${pendingAp!.y}`}-${apPrefill.join()}`}
                addBssids={apPrefill}
                ap={selectedAp}
                position={selectedAp ? { x: selectedAp.x, y: selectedAp.y } : pendingAp!}
                points={points}
                otherAps={allAps.filter((a) => a.id !== selectedAp?.id)}
                suggestRadiusPx={ppm ? 4 * ppm : Math.max(plan.width, plan.height) * 0.15}
                defaultName={`AP ${allAps.length + 1}`}
                color={
                  selectedAp
                    ? (coloredAps.find((c) => c.ap.id === selectedAp.id)?.color ?? apColor(-1))
                    : apColor(allAps.length)
                }
                onSave={async (input) => {
                  const ok = await saveAp(input, selectedAp?.id ?? null);
                  if (ok) {
                    setSelectedApId(null);
                    setApPrefill([]);
                  }
                  return ok;
                }}
                onDelete={selectedAp ? () => void removeAp(selectedAp.id) : undefined}
                onNotesSaved={(id, notes) => setAllAps((aps) => aps.map((a) => (a.id === id ? { ...a, notes } : a)))}
                onCancel={() => {
                  setPendingAp(null);
                  setSelectedApId(null);
                  setApPrefill([]);
                }}
              />
            ) : (
              <section className="card">
                <header className="card-header">
                  <h2>
                    Access points <span className="count">{floorAps.length}</span>
                  </h2>
                </header>
                <div className="panel-section">
                  <p>Click the plan where an access point is mounted.</p>
                  {apPrefill.length > 0 && (
                    <p className="small">
                      From Findings: <span className="mono">{apPrefill.join(", ")}</span> will be linked to it.
                    </p>
                  )}
                  <p className="muted small">
                    Link the BSSIDs it broadcasts, and its name shows up in readings and the heatmap (per-AP and
                    serving-AP views). Drag a marker to move it.
                  </p>
                </div>
                {floorAps.length > 0 && (
                  <ul className="ap-list">
                    {floorAps.map(({ ap, color }) => (
                      <li key={ap.id}>
                        <button type="button" onClick={() => setSelectedApId(ap.id)}>
                          <span className="ap-swatch" style={{ background: color }} />
                          <span className="grow">{ap.name}</span>
                          <span className="muted mono small">
                            {ap.bssids.length} BSSID{ap.bssids.length === 1 ? "" : "s"}
                          </span>
                        </button>
                      </li>
                    ))}
                  </ul>
                )}
              </section>
            )
          ) : mode === "scale" ? (
            <section className="card">
              <header className="card-header">
                <h2>Set scale</h2>
                {floor.scale && (
                  <button type="button" className="btn btn-small" onClick={() => void clearScale()}>
                    Clear scale
                  </button>
                )}
              </header>
              <div className="panel-section">
                <ol className="panel-steps">
                  <li className={scaleA ? "done" : ""}>Click one end of a distance you know (a wall, a corridor).</li>
                  <li className={scaleB ? "done" : ""}>Click the other end.</li>
                  <li>Enter its real length.</li>
                </ol>
                <form
                  className="length-input"
                  onSubmit={(e) => {
                    e.preventDefault();
                    void saveScale();
                  }}
                >
                  <input
                    ref={lengthInput}
                    className="input"
                    inputMode="decimal"
                    placeholder="e.g. 12.5"
                    value={lengthText}
                    onChange={(e) => setLengthText(e.target.value)}
                    disabled={!scaleB}
                  />
                  <span className="muted">m</span>
                  <button className="btn btn-primary" type="submit" disabled={!scaleA || !scaleB || scaleLength == null}>
                    Save scale
                  </button>
                </form>
                {scaleA && scaleB && scaleLength != null && (
                  <p className="muted small mono">
                    {(Math.hypot(scaleB.x - scaleA.x, scaleB.y - scaleA.y) / scaleLength).toFixed(1)} px/m · plan{" "}
                    {(plan.width / (Math.hypot(scaleB.x - scaleA.x, scaleB.y - scaleA.y) / scaleLength)).toFixed(1)} m
                    wide
                  </p>
                )}
                <div className="panel-actions">
                  <button type="button" className="btn" onClick={() => setMode("measure")}>
                    Cancel
                  </button>
                </div>
              </div>
            </section>
          ) : showFindings ? (
            <FindingsPanel
              floor={floor}
              revision={findingsRev}
              floorAps={floorAps.map(({ ap }) => ap)}
              onHighlight={setFlagged}
              onPlaceAp={(bssids, at) => {
                setApPrefill(bssids);
                setSelectedApId(null);
                setPendingAp(at);
                setMode("aps");
              }}
              onEditAp={(id, bssids) => {
                setApPrefill(bssids);
                setPendingAp(null);
                setSelectedApId(id);
                setMode("aps");
              }}
            />
          ) : (
            <>
              {heatActive && (
                <HeatmapControls
                  metric={metric}
                  setMetric={(metric) => setHeat({ metric })}
                  network={heatCfg.network}
                  setNetwork={setNetwork}
                  band={heatPrefs.band}
                  setBand={(band) => setHeat({ band })}
                  threshold={heatCfg.threshold}
                  setThreshold={(t) => {
                    const patch = metric === "coverage" ? { coverage: t } : { overlap: t };
                    if (profileLevels) setProfileLevels({ ...profileLevels, ...patch });
                    else setHeat(patch);
                  }}
                  networks={networks}
                  passingFraction={heatGrid?.passingFraction ?? null}
                  usedPoints={heatPoints.length}
                  excludedPoints={points.length - heatPoints.length}
                  scaled={ppm != null}
                  aps={coloredAps}
                  servingShares={
                    heatGrid && "shares" in heatGrid ? { shares: heatGrid.shares, unserved: heatGrid.unserved } : null
                  }
                  apNameByBssid={apNameByBssid}
                  requirementsLegend={
                    reqProfile ? (
                      <RequirementsLegend
                        profile={reqProfile}
                        areaFraction={reqGrid?.passingFraction ?? null}
                        pointFraction={requirements?.summary?.passFractionOfPoints ?? null}
                      />
                    ) : null
                  }
                />
              )}
              {heatActive && heatPoints.length === 0 && (
                <NoticeBanner>None of this floor's points have dBm readings, so there is nothing to interpolate.</NoticeBanner>
              )}
              {!selectedAdapter && <NoticeBanner>Select a Wi-Fi adapter to measure.</NoticeBanner>}
              {adapterDiffers && !mismatch && (
                <NoticeBanner>
                  This floor was measured with{" "}
                  {floorAdapters.map((a) => `${a.model ?? iface(a.id)} (${iface(a.id)})`).join(", ")}. Readings from
                  different cards aren't directly comparable.
                </NoticeBanner>
              )}

              {testing && (
                <section className="card">
                  <header className="card-header">
                    <h2>
                      Testing at point{" "}
                      <span className="count">{points.findIndex((p) => p.id === testing.pointId) + 1}</span>
                    </h2>
                    <button type="button" className="btn btn-small" onClick={cancelTests}>
                      Cancel
                    </button>
                  </header>
                  <div className="panel-section">
                    <p className="small">
                      <IconRadar className="spin" /> {testSettings ? plannedTests(testSettings) : "Running the tests"}…
                    </p>
                    <p className="muted small">Stay at the point until they finish.</p>
                  </div>
                </section>
              )}
              {testError && <ErrorBanner error={testError} compact />}

              {pending ? (
                <section className="card">
                  <header className="card-header">
                    <h2>New point</h2>
                    <span className="mono muted">
                      {ppm
                        ? `${(pending.x / ppm).toFixed(1)} m, ${(pending.y / ppm).toFixed(1)} m`
                        : `${Math.round(pending.x)}, ${Math.round(pending.y)} px`}
                    </span>
                  </header>
                  <div className="panel-section">
                    {mismatch ? (
                      <>
                        <div className="banner banner-notice" role="alert">
                          <div className="banner-message">{mismatch}</div>
                        </div>
                        <div className="panel-actions">
                          <button type="button" className="btn btn-primary" onClick={() => void measure(true)} disabled={measuring}>
                            Measure anyway
                          </button>
                          <button type="button" className="btn" onClick={() => setMismatch(null)}>
                            Cancel
                          </button>
                        </div>
                      </>
                    ) : (
                      <>
                        <button
                          type="button"
                          className="btn btn-primary btn-measure"
                          onClick={() => void measure()}
                          disabled={measuring || !selectedAdapter || testing != null}
                        >
                          <IconRadar className={measuring ? "spin" : ""} />
                          {measuring ? "Scanning…" : "Measure here"}
                        </button>
                        <label className="checkbox">
                          <input
                            type="checkbox"
                            checked={runTests}
                            onChange={(e) => setRunTests(e.target.checked)}
                            disabled={measuring}
                          />
                          + run tests after the scan
                        </label>
                        {runTests && (
                          <p className="muted small">
                            {testSettings ? `Then: ${plannedTests(testSettings)}.` : "Then the configured tests."} Set
                            the targets in Settings → Active tests.
                          </p>
                        )}
                      </>
                    )}
                    <p className="muted small">
                      Stand still while it scans: usually 3–5 s, longer right after another scan. Only networks heard
                      during this scan are saved.{" "}
                      {measuring
                        ? "A running scan can't be cancelled; the point is saved when it finishes."
                        : "Enter measures, Esc cancels."}
                    </p>
                  </div>
                </section>
              ) : selected ? (
                <PointDetails
                  apNameByBssid={apNameByBssid}
                  point={selected}
                  number={points.indexOf(selected) + 1}
                  pxPerMetre={ppm}
                  onDelete={() => void removePoint(selected.id)}
                  tests={pointTests.filter((t) => t.pointId === selected.id)}
                  requirements={
                    reqProfile && reqEval(selected.id) ? (
                      <PointRequirements evaluation={reqEval(selected.id)!} profileName={reqProfile.name} />
                    ) : undefined
                  }
                />
              ) : (
                <section className="card">
                  <header className="card-header">
                    <h2>Measure</h2>
                  </header>
                  <div className="panel-section">
                    <p>Click the plan where you are standing, then press Measure here.</p>
                    <p className="muted small">
                      Click a numbered point to see its readings. Drag to pan, scroll to zoom.
                    </p>
                    {!floor.scale && (
                      <p className="muted small">
                        Tip: use <strong>Set scale</strong> so positions and distances are shown in metres. Heatmaps
                        will need it.
                      </p>
                    )}
                  </div>
                </section>
              )}
              <RequirementsPanel
                requirements={requirements}
                areaFraction={reqGrid?.passingFraction ?? null}
                onRequirements={setRequirements}
                onProfilesChanged={() => setRequirementsRev((n) => n + 1)}
              />
              {pointTests.length > 0 && (
                <section className="card">
                  <header className="card-header">
                    <h2>
                      Active tests <span className="count">{pointTests.length}</span>
                    </h2>
                  </header>
                  <PointTestsTable tests={pointTests} pointNumbers={new Map(points.map((p, i) => [p.id, i + 1]))} />
                </section>
              )}
            </>
          )}
        </aside>
      </div>
      {exportOpen && (
        <ExportDialog project={project} building={building} floor={floor} onClose={() => setExportOpen(false)} />
      )}
    </>
  );
}
