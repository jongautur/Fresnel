import { useEffect, useMemo, useRef, useState, type ChangeEvent } from "react";
import { api, asApiError } from "../../api/tauri";
import { useWifi } from "../../state/WifiContext";
import type { ApiError, Band } from "../../types/wifi";
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
import { PointDetails } from "./PointDetails";
import { HeatmapControls } from "./HeatmapControls";
import {
  NOT_HEARD_DBM,
  apColor,
  buildGrid,
  buildServingGrid,
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

// ---------------------------------------------------------------------------
// Plan file reading
// ---------------------------------------------------------------------------

/** Below this, SVG coordinate spaces are scaled up so the plan stays sharp when zoomed. */
const SVG_MIN_SIDE = 2000;

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
      }
    }
    if (!w || !h) throw new Error("Could not determine the image size (an SVG needs width/height or a viewBox).");
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

function loadHeatPrefs(): HeatPrefs {
  const d: HeatPrefs = { on: false, metric: "signal", band: "all", coverage: -67, overlap: -75 };
  try {
    return { ...d, ...(JSON.parse(localStorage.getItem(HEAT_KEY) ?? "{}") as Partial<HeatPrefs>) };
  } catch {
    return d;
  }
}

const isTyping = (t: EventTarget | null) =>
  t instanceof HTMLElement && (t.isContentEditable || /^(INPUT|SELECT|TEXTAREA|BUTTON)$/.test(t.tagName));

// ---------------------------------------------------------------------------
// Workspace
// ---------------------------------------------------------------------------

export function FloorWorkspace({ floor, onFloorChange }: { floor: Floor; onFloorChange: (f: Floor) => void }) {
  const { selectedAdapter } = useWifi();
  const [points, setPoints] = useState<SurveyPoint[]>([]);
  const [imageUrl, setImageUrl] = useState<string | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const [mode, setMode] = useState<CanvasMode>("measure");
  const [pending, setPending] = useState<PlanXY | null>(null);
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [measuring, setMeasuring] = useState(false);
  const [mismatch, setMismatch] = useState<string | null>(null);
  const [importing, setImporting] = useState(false);
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

  // Access points: the whole building's (names resolve across floors)
  useEffect(() => {
    api
      .listBuildingAps(floor.buildingId)
      .then(setAllAps)
      .catch((e) => setError(asApiError(e)));
  }, [floor.buildingId]);

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
  const metric: HeatMetric = heatPrefs.metric === "serving" && servable.length === 0 ? "signal" : heatPrefs.metric;
  const heatCfg: HeatmapConfig = {
    metric,
    network: resolvedNetwork,
    band: heatPrefs.band,
    threshold: metric === "overlap" || metric === "serving" ? heatPrefs.overlap : heatPrefs.coverage,
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
  const heatGrid = useMemo((): HeatGrid | ServingGrid | null => {
    if (!heatActive || !plan) return null;
    return metric === "serving"
      ? buildServingGrid(plan, ppm, serving, heatCfg.threshold)
      : buildGrid(plan, ppm, heatPoints, heatCfg);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [heatActive, plan, ppm, heatPoints, serving, cfgKey]);
  const fmtValue = (v: number) =>
    heatCfg.metric === "overlap"
      ? `${Math.round(v)} AP${Math.round(v) === 1 ? "" : "s"}`
      : v <= NOT_HEARD_DBM
        ? "not heard"
        : `${Math.round(v)} dBm`;
  const pointLabels = useMemo(() => {
    if (!heatActive) return undefined;
    const m = new Map<number, string>();
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
  }, [heatActive, heatPoints, cfgKey]);
  const hoverInfo = (p: PlanXY) => {
    if (!heatActive || !plan) return null;
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
      onFloorChange({ ...floor, pointCount: points.length + 1 });
    } catch (e) {
      const err = asApiError(e);
      if (err.kind === "adapter_mismatch") setMismatch(err.message);
      else setError(err);
    } finally {
      setMeasuring(false);
    }
  };

  const removePoint = async (id: number) => {
    try {
      await api.deleteSurveyPoint(id);
      setPoints((ps) => ps.filter((p) => p.id !== id));
      setSelectedId(null);
      onFloorChange({ ...floor, pointCount: Math.max(0, points.length - 1) });
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
      onFloorChange(updated);
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
      onFloorChange(await api.setFloorScale(floor.id, scale));
      setMode("measure");
      setError(null);
    } catch (e) {
      setError(asApiError(e));
    }
  };

  const clearScale = async () => {
    try {
      onFloorChange(await api.setFloorScale(floor.id, null));
      setScaleA(null);
      setScaleB(null);
      setLengthText("");
    } catch (e) {
      setError(asApiError(e));
    }
  };

  const onPlanClick = (p: PlanXY) => {
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

  // Enter = measure, Escape = cancel (unless typing in a field)
  const measureRef = useRef(measure);
  measureRef.current = measure;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (isTyping(e.target)) return;
      if (e.key === "Enter" && mode === "measure") {
        e.preventDefault();
        void measureRef.current();
      } else if (e.key === "Escape") {
        setPendingAp(null);
        setSelectedApId(null);
        setPending(null);
        setSelectedId(null);
        setMismatch(null);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [mode]);

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
        disabled={importing || (plan != null && (floor.pointCount > 0 || floorAps.length > 0))}
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
        <div className="segmented" role="tablist" aria-label="Tool">
          <button
            type="button"
            role="tab"
            aria-selected={mode === "measure"}
            className={mode === "measure" ? "active" : ""}
            onClick={() => setMode("measure")}
          >
            Measure
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={mode === "aps"}
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
            aria-selected={mode === "scale"}
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
          pointLabels={pointLabels}
          aps={apMarkers}
          selectedApId={selectedApId}
          pendingAp={pendingAp}
          onApClick={(id) => {
            setSelectedApId(id);
            setPendingAp(null);
          }}
          onApMove={moveAp}
          hoverInfo={heatActive ? hoverInfo : undefined}
          onPlanClick={onPlanClick}
          onPointClick={(id) => {
            setSelectedId(id);
            setPending(null);
            setMismatch(null);
          }}
        />

        <aside className="survey-panel">
          {mode === "aps" ? (
            pendingAp || selectedAp ? (
              <ApEditor
                key={selectedAp ? `ap-${selectedAp.id}` : `new-${pendingAp!.x}-${pendingAp!.y}`}
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
                  if (ok) setSelectedApId(null);
                  return ok;
                }}
                onDelete={selectedAp ? () => void removeAp(selectedAp.id) : undefined}
                onCancel={() => {
                  setPendingAp(null);
                  setSelectedApId(null);
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
                  setThreshold={(t) => setHeat(metric === "coverage" ? { coverage: t } : { overlap: t })}
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
                      <button
                        type="button"
                        className="btn btn-primary btn-measure"
                        onClick={() => void measure()}
                        disabled={measuring || !selectedAdapter}
                      >
                        <IconRadar className={measuring ? "spin" : ""} />
                        {measuring ? "Scanning…" : "Measure here"}
                      </button>
                    )}
                    <p className="muted small">
                      Stand still while it scans: usually 3–5 s, longer right after another scan. Only networks heard
                      during this scan are saved. Enter measures, Esc cancels.
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
            </>
          )}
        </aside>
      </div>
    </>
  );
}
