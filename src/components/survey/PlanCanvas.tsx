import { useCallback, useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent, type PointerEvent } from "react";
import type { FloorPlan, SurveyPoint } from "../../types/survey";
import type { HeatGrid } from "../../lib/heatmap";
import { formatTime } from "../../lib/format";
import { NotePinMarkers, PinTooltip, pinAt, type PinMarker } from "./NotePinLayer";

export type CanvasMode = "measure" | "aps" | "pins" | "scale";

export interface ApMarker {
  id: number;
  name: string;
  x: number;
  y: number;
  color: string;
}

export interface PlanXY {
  x: number;
  y: number;
}

export interface ScaleDraft {
  a: PlanXY | null;
  b: PlanXY | null;
  label: string | null;
}

interface View {
  k: number;
  tx: number;
  ty: number;
}

const POINT_R = 9;
const AP_HALF = 11;
const HIT_PX = 12;
const DRAG_PX = 4;
const MAX_ZOOM = 8;
/** Wheel deltas given in lines or pages, converted to pixels. */
const WHEEL_LINE_PX = 40;

/**
 * Client coordinates → coordinates inside the canvas's padding box, which is
 * what the absolutely positioned plan and overlay are laid out in (the
 * bounding rect includes the 1 px border).
 */
function toLocal(el: HTMLElement, clientX: number, clientY: number) {
  const r = el.getBoundingClientRect();
  return { sx: clientX - r.left - el.clientLeft, sy: clientY - r.top - el.clientTop };
}

/** Nice scale-bar length (m) that renders between ~60 and ~150 px. */
function scaleBarMetres(pxPerScreenMetre: number): number | null {
  if (!(pxPerScreenMetre > 0)) return null;
  for (const m of [0.5, 1, 2, 5, 10, 20, 50, 100, 200, 500, 1000]) {
    if (m * pxPerScreenMetre >= 60) return m;
  }
  return null;
}

function useSize<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  const [size, setSize] = useState({ w: 0, h: 0 });
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(([e]) =>
      setSize({ w: Math.floor(e!.contentRect.width), h: Math.floor(e!.contentRect.height) }),
    );
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  return [ref, size] as const;
}

/**
 * Floor plan with pan (drag), zoom (wheel, +/−, buttons) and survey overlays.
 * Overlays are drawn in screen space so markers keep their size at any zoom.
 * All coordinates passed in and out are plan pixels.
 */
export function PlanCanvas({
  plan,
  imageUrl,
  mode,
  points,
  selectedId,
  pending,
  measuring,
  scale,
  pxPerMetre,
  heat,
  pointLabels,
  highlightIds = null,
  hoverInfo,
  aps = [],
  selectedApId = null,
  pendingAp = null,
  onApClick,
  onApMove,
  pins = [],
  selectedPinId = null,
  pendingPin = null,
  onPinClick,
  onPlanClick,
  onPointClick,
}: {
  plan: FloorPlan;
  imageUrl: string | null;
  mode: CanvasMode;
  points: SurveyPoint[];
  selectedId: number | null;
  pending: PlanXY | null;
  measuring: boolean;
  scale: ScaleDraft | null;
  pxPerMetre: number | null;
  /** Heatmap raster covering the whole plan, drawn above the image. */
  heat?: HeatGrid | null;
  /** Value shown beside each point marker (heatmap mode). */
  pointLabels?: Map<number, string>;
  /** Points to emphasise (others are dimmed), e.g. where a finding was heard. */
  highlightIds?: Set<number> | null;
  /** Extra hover readout for a plan position (e.g. the estimated value). */
  hoverInfo?: (p: PlanXY) => string | null;
  /** Access points placed on this floor. */
  aps?: ApMarker[];
  selectedApId?: number | null;
  /** Position of an AP being placed. */
  pendingAp?: PlanXY | null;
  onApClick?: (id: number) => void;
  /** Drag-to-move finished (Place APs mode). */
  onApMove?: (id: number, p: PlanXY) => void;
  /** Note pins on this floor (hover shows their text). */
  pins?: PinMarker[];
  selectedPinId?: number | null;
  /** Position of a note pin being placed. */
  pendingPin?: PlanXY | null;
  /** A pin was clicked (Notes mode). */
  onPinClick?: (id: number) => void;
  onPlanClick: (p: PlanXY) => void;
  onPointClick: (id: number) => void;
}) {
  const [ref, size] = useSize<HTMLDivElement>();
  const [view, setView] = useState<View | null>(null);
  const [hover, setHover] = useState<{ sx: number; sy: number; plan: PlanXY } | null>(null);
  const [hoverPoint, setHoverPoint] = useState<number | null>(null);
  const [hoverPin, setHoverPin] = useState<number | null>(null);
  const [panning, setPanning] = useState(false);
  const drag = useRef<{ sx: number; sy: number; view: View; moved: boolean; id: number; apId?: number } | null>(null);
  const [apDrag, setApDrag] = useState<{ id: number; x: number; y: number } | null>(null);
  const heatCanvas = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const c = heatCanvas.current;
    if (!c || !heat) return;
    c.width = heat.cols;
    c.height = heat.rows;
    c.getContext("2d")?.putImageData(new ImageData(new Uint8ClampedArray(heat.rgba), heat.cols, heat.rows), 0, 0);
  }, [heat, view != null, imageUrl]); // eslint-disable-line react-hooks/exhaustive-deps
  const userMoved = useRef(false);

  const fitView = useCallback((): View | null => {
    if (size.w === 0 || size.h === 0) return null;
    const k = Math.min(size.w / plan.width, size.h / plan.height) * 0.94;
    return { k, tx: (size.w - plan.width * k) / 2, ty: (size.h - plan.height * k) / 2 };
  }, [size.w, size.h, plan.width, plan.height]);

  // Fit on a new plan; on resize too, unless the user has panned/zoomed.
  const planKey = `${plan.file}`;
  useEffect(() => {
    userMoved.current = false;
  }, [planKey]);
  useEffect(() => {
    if (!userMoved.current || view == null) setView(fitView());
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [fitView, planKey]);

  const minZoom = (fitView()?.k ?? 0.01) / 4;

  const zoomAt = useCallback(
    (sx: number, sy: number, factor: number) => {
      setView((v) => {
        if (!v) return v;
        const k = Math.min(MAX_ZOOM, Math.max(minZoom, v.k * factor));
        const f = k / v.k;
        return { k, tx: sx - (sx - v.tx) * f, ty: sy - (sy - v.ty) * f };
      });
      userMoved.current = true;
    },
    [minZoom],
  );

  // Wheel needs a non-passive listener to stop the page scrolling.
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const { sx, sy } = toLocal(el, e.clientX, e.clientY);
      const dy =
        e.deltaMode === WheelEvent.DOM_DELTA_LINE
          ? e.deltaY * WHEEL_LINE_PX
          : e.deltaMode === WheelEvent.DOM_DELTA_PAGE
            ? e.deltaY * el.clientHeight
            : e.deltaY;
      zoomAt(sx, sy, Math.exp(-dy * 0.0015));
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, [ref, zoomAt]);

  if (!view) {
    return <div className={`plan-canvas mode-${mode}`} ref={ref} />;
  }

  const toScreen = (p: PlanXY) => ({ x: p.x * view.k + view.tx, y: p.y * view.k + view.ty });
  const toPlan = (sx: number, sy: number): PlanXY => ({ x: (sx - view.tx) / view.k, y: (sy - view.ty) / view.k });
  const onPlan = (p: PlanXY) => p.x >= 0 && p.y >= 0 && p.x <= plan.width && p.y <= plan.height;

  const local = (e: PointerEvent<HTMLDivElement>) => toLocal(e.currentTarget, e.clientX, e.clientY);

  const pointAt = (sx: number, sy: number): number | null => {
    let best: number | null = null;
    let bestD = HIT_PX;
    for (const p of points) {
      const s = toScreen(p);
      const d = Math.hypot(s.x - sx, s.y - sy);
      if (d <= bestD) {
        bestD = d;
        best = p.id;
      }
    }
    return best;
  };

  const apAt = (sx: number, sy: number): number | null => {
    for (const a of [...aps].reverse()) {
      const s = toScreen(a);
      if (Math.abs(s.x - sx) <= AP_HALF + 3 && Math.abs(s.y - sy) <= AP_HALF + 3) return a.id;
    }
    return null;
  };

  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    if (e.button !== 0 && e.button !== 1) return;
    const { sx, sy } = local(e);
    e.currentTarget.setPointerCapture(e.pointerId);
    const apId = e.button === 0 && mode === "aps" ? apAt(sx, sy) : null;
    drag.current = { sx, sy, view, moved: e.button === 1, id: e.pointerId, apId: apId ?? undefined };
    if (e.button === 1) setPanning(true);
  };

  const onPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    const { sx, sy } = local(e);
    const d = drag.current;
    if (d) {
      if (!d.moved && Math.hypot(sx - d.sx, sy - d.sy) > DRAG_PX) {
        d.moved = true;
        if (d.apId == null) setPanning(true);
      }
      if (d.moved && d.apId != null) {
        const p = toPlan(sx, sy);
        setApDrag({
          id: d.apId,
          x: Math.min(plan.width, Math.max(0, p.x)),
          y: Math.min(plan.height, Math.max(0, p.y)),
        });
        return;
      }
      if (d.moved) {
        userMoved.current = true;
        setView({ ...d.view, tx: d.view.tx + sx - d.sx, ty: d.view.ty + sy - d.sy });
        return;
      }
    }
    const p = toPlan(sx, sy);
    setHover(onPlan(p) ? { sx, sy, plan: p } : null);
    setHoverPoint(pointAt(sx, sy));
    setHoverPin(mode !== "scale" ? pinAt(pins, toScreen, sx, sy) : null);
  };

  const onPointerUp = (e: PointerEvent<HTMLDivElement>) => {
    const d = drag.current;
    drag.current = null;
    setPanning(false);
    if (d?.apId != null) {
      if (d.moved && apDrag) onApMove?.(d.apId, { x: apDrag.x, y: apDrag.y });
      else if (!d.moved) onApClick?.(d.apId);
      setApDrag(null);
      return;
    }
    if (!d || d.moved || e.button !== 0) return;
    const { sx, sy } = local(e);
    const hit = mode === "measure" ? pointAt(sx, sy) : null;
    if (hit != null) {
      onPointClick(hit);
      return;
    }
    const pinHit = mode === "pins" ? pinAt(pins, toScreen, sx, sy) : null;
    if (pinHit != null) {
      onPinClick?.(pinHit);
      return;
    }
    const p = toPlan(sx, sy);
    if (onPlan(p)) onPlanClick(p);
  };

  // The gesture was taken away (touch cancelled, capture lost): drop it without clicking or moving anything.
  const cancelDrag = () => {
    if (!drag.current) return;
    drag.current = null;
    setPanning(false);
    setApDrag(null);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const step = 60;
    const pan = (dx: number, dy: number) => {
      userMoved.current = true;
      setView({ ...view, tx: view.tx + dx, ty: view.ty + dy });
    };
    switch (e.key) {
      case "+":
      case "=":
        zoomAt(size.w / 2, size.h / 2, 1.25);
        break;
      case "-":
        zoomAt(size.w / 2, size.h / 2, 0.8);
        break;
      case "0":
        userMoved.current = false;
        setView(fitView());
        break;
      case "ArrowLeft":
        pan(step, 0);
        break;
      case "ArrowRight":
        pan(-step, 0);
        break;
      case "ArrowUp":
        pan(0, step);
        break;
      case "ArrowDown":
        pan(0, -step);
        break;
      default:
        return;
    }
    e.preventDefault();
  };

  const barM = pxPerMetre ? scaleBarMetres(pxPerMetre * view.k) : null;
  const hovered = hoverPoint != null ? points.find((p) => p.id === hoverPoint) : undefined;
  const hoveredIndex = hovered ? points.indexOf(hovered) + 1 : 0;

  return (
    <div
      ref={ref}
      className={`plan-canvas mode-${mode} ${panning ? "panning" : ""}`}
      tabIndex={0}
      role="application"
      aria-label="Floor plan. Drag to pan, scroll or +/− to zoom, 0 to fit."
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={cancelDrag}
      onLostPointerCapture={cancelDrag}
      onPointerLeave={() => {
        setHover(null);
        setHoverPoint(null);
        setHoverPin(null);
      }}
      onKeyDown={onKeyDown}
      onContextMenu={(e) => e.preventDefault()}
    >
      {imageUrl ? (
        <img
          src={imageUrl}
          alt=""
          draggable={false}
          style={{
            width: plan.width,
            height: plan.height,
            transform: `translate(${view.tx}px, ${view.ty}px) scale(${view.k})`,
          }}
        />
      ) : (
        <div className="plan-loading">Loading floor plan…</div>
      )}
      {heat && imageUrl && (
        <canvas
          ref={heatCanvas}
          className="plan-heat"
          style={{
            width: plan.width,
            height: plan.height,
            transform: `translate(${view.tx}px, ${view.ty}px) scale(${view.k})`,
          }}
        />
      )}

      <svg className="plan-overlay" width={size.w} height={size.h}>
        {/* Scale reference line */}
        {scale?.a && (
          <g className="plan-scale">
            {(() => {
              const a = toScreen(scale.a);
              const b = scale.b ? toScreen(scale.b) : hover && mode === "scale" ? { x: hover.sx, y: hover.sy } : null;
              return (
                <>
                  {b && <line className="shadow" x1={a.x} y1={a.y} x2={b.x} y2={b.y} />}
                  {b && <line x1={a.x} y1={a.y} x2={b.x} y2={b.y} />}
                  <circle cx={a.x} cy={a.y} r={5} />
                  {scale.b && b && <circle cx={b.x} cy={b.y} r={5} />}
                  {scale.label && b && (
                    <text x={(a.x + b.x) / 2} y={(a.y + b.y) / 2 - 10} textAnchor="middle">
                      {scale.label}
                    </text>
                  )}
                </>
              );
            })()}
          </g>
        )}

        {/* Survey points (numbered in measurement order) */}
        {mode !== "scale" &&
          points.map((p, i) => {
            const s = toScreen(p);
            const flag = highlightIds ? (highlightIds.has(p.id) ? "flagged" : "dim") : "";
            const cls = `plan-point ${heat ? "heat" : ""} ${mode === "aps" ? "dim" : flag} ${p.id === selectedId ? "selected" : ""} ${p.id === hoverPoint && mode === "measure" ? "hovered" : ""}`;
            const label = pointLabels?.get(p.id);
            return (
              <g key={p.id} className={cls}>
                <circle className="halo" cx={s.x} cy={s.y} r={POINT_R + 2} />
                <circle cx={s.x} cy={s.y} r={POINT_R} />
                <text x={s.x} y={s.y} dy="0.35em" textAnchor="middle">
                  {i + 1}
                </text>
                {label && (
                  <text className="plan-point-value" x={s.x + POINT_R + 4} y={s.y} dy="0.35em">
                    {label}
                  </text>
                )}
              </g>
            );
          })}

        {/* Placed access points */}
        {mode !== "scale" &&
          aps.map((a) => {
            const pos = apDrag?.id === a.id ? apDrag : a;
            const s = toScreen(pos);
            return (
              <g key={`ap-${a.id}`} className={`plan-ap ${a.id === selectedApId ? "selected" : ""} ${mode === "aps" ? "editable" : ""}`}>
                <rect className="halo" x={s.x - AP_HALF - 2} y={s.y - AP_HALF - 2} width={AP_HALF * 2 + 4} height={AP_HALF * 2 + 4} rx={7} />
                <rect x={s.x - AP_HALF} y={s.y - AP_HALF} width={AP_HALF * 2} height={AP_HALF * 2} rx={5} style={{ fill: a.color }} />
                <path
                  className="glyph"
                  d={`M${s.x - 6},${s.y - 1.5} a8.5,8.5 0 0 1 12,0 M${s.x - 3.5},${s.y + 1.5} a5,5 0 0 1 7,0`}
                />
                <circle className="glyph-dot" cx={s.x} cy={s.y + 5} r={1.6} />
                <text className="plan-ap-label" x={s.x} y={s.y + AP_HALF + 13} textAnchor="middle">
                  {a.name}
                </text>
              </g>
            );
          })}
        {mode === "aps" && pendingAp && (
          <g className="plan-ap pending">
            {(() => {
              const s = toScreen(pendingAp);
              return <rect x={s.x - AP_HALF} y={s.y - AP_HALF} width={AP_HALF * 2} height={AP_HALF * 2} rx={5} />;
            })()}
          </g>
        )}

        {/* Note pins */}
        {mode !== "scale" && (
          <NotePinMarkers
            pins={pins}
            toScreen={toScreen}
            selectedId={selectedPinId}
            hoveredId={hoverPin}
            pending={mode === "pins" ? pendingPin : null}
          />
        )}

        {/* Position awaiting "Measure here" */}
        {mode === "measure" && pending && (
          <g className={`plan-pending ${measuring ? "measuring" : ""}`}>
            {(() => {
              const s = toScreen(pending);
              return (
                <>
                  <circle className="ring" cx={s.x} cy={s.y} r={14} />
                  <line className="cross" x1={s.x - 6} y1={s.y} x2={s.x + 6} y2={s.y} />
                  <line className="cross" x1={s.x} y1={s.y - 6} x2={s.x} y2={s.y + 6} />
                </>
              );
            })()}
          </g>
        )}
      </svg>

      {hoverPin != null && !hovered && pins.find((p) => p.id === hoverPin) && (
        <PinTooltip pin={pins.find((p) => p.id === hoverPin)!} toScreen={toScreen} />
      )}

      {hovered && (
        <div
          className="plan-tooltip"
          style={{ left: toScreen(hovered).x + 14, top: toScreen(hovered).y - 12 }}
        >
          <strong>Point {hoveredIndex}</strong> <span className="muted">· {formatTime(hovered.measuredAt)}</span>
          <br />
          <span className="muted">
            {hovered.samples.length} BSSID{hovered.samples.length === 1 ? "" : "s"} heard
            {hovered.samples[0]?.signal.dbm != null && ` · strongest ${Math.round(hovered.samples[0].signal.dbm)} dBm`}
          </span>
        </div>
      )}

      <div className="plan-hud plan-hud-bl">
        {barM != null && pxPerMetre && (
          <>
            <span className="scale-bar" style={{ width: barM * pxPerMetre * view.k }} />
            <span>{barM} m</span>
            <span className="sep">·</span>
          </>
        )}
        {hover && hoverInfo?.(hover.plan) && (
          <>
            <span className="plan-hud-value">{hoverInfo(hover.plan)}</span>
            <span className="sep">·</span>
          </>
        )}
        {hover ? (
          pxPerMetre ? (
            <span>
              x {(hover.plan.x / pxPerMetre).toFixed(1)} m · y {(hover.plan.y / pxPerMetre).toFixed(1)} m
            </span>
          ) : (
            <span>
              x {Math.round(hover.plan.x)} · y {Math.round(hover.plan.y)} px
            </span>
          )
        ) : (
          <span>{pxPerMetre ? `${(plan.width / pxPerMetre).toFixed(1)} × ${(plan.height / pxPerMetre).toFixed(1)} m` : "not scaled"}</span>
        )}
      </div>

      <div className="plan-zoom" onPointerDown={(e) => e.stopPropagation()} onPointerUp={(e) => e.stopPropagation()}>
        <button type="button" className="btn" title="Zoom in (+)" onClick={() => zoomAt(size.w / 2, size.h / 2, 1.25)}>
          +
        </button>
        <button type="button" className="btn" title="Zoom out (−)" onClick={() => zoomAt(size.w / 2, size.h / 2, 0.8)}>
          −
        </button>
        <button
          type="button"
          className="btn"
          title="Fit plan (0)"
          onClick={() => {
            userMoved.current = false;
            setView(fitView());
          }}
        >
          ⤢
        </button>
      </div>
    </div>
  );
}
