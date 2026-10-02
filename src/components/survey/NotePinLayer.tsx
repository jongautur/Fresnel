import { pinCategoryLabel } from "../../types/notes";
import "../../styles/notes.css";

export interface PinMarker {
  id: number;
  x: number;
  y: number;
  text: string;
  category: string | null;
}

interface XY {
  x: number;
  y: number;
}

/** Marker size: a small teardrop whose tip is the pinned spot. */
const PIN_W = 7;
const PIN_H = 18;
const HIT_PX = 10;

/** Centre of the marker's head, in screen space, for a pin at `s`. */
const head = (s: XY) => ({ x: s.x, y: s.y - PIN_H + PIN_W });

/** The pin under a screen position, topmost first. */
export function pinAt(pins: PinMarker[], toScreen: (p: XY) => XY, sx: number, sy: number): number | null {
  for (const p of [...pins].reverse()) {
    const h = head(toScreen(p));
    if (Math.hypot(h.x - sx, h.y - sy) <= HIT_PX) return p.id;
  }
  return null;
}

function teardrop(s: XY): string {
  const h = head(s);
  // Arc over the top of the head, straight sides down to the tip.
  return `M${s.x},${s.y} L${h.x - PIN_W * 0.87},${h.y + PIN_W * 0.5} A${PIN_W},${PIN_W} 0 1 1 ${h.x + PIN_W * 0.87},${h.y + PIN_W * 0.5} Z`;
}

/** Note pins as SVG, in screen space (drawn inside PlanCanvas's overlay). */
export function NotePinMarkers({
  pins,
  toScreen,
  selectedId,
  hoveredId,
  pending,
}: {
  pins: PinMarker[];
  toScreen: (p: XY) => XY;
  selectedId: number | null;
  hoveredId: number | null;
  /** Position of a pin being placed. */
  pending: XY | null;
}) {
  return (
    <g className="plan-pins">
      {pins.map((p) => {
        const s = toScreen(p);
        const h = head(s);
        return (
          <g
            key={`pin-${p.id}`}
            className={`plan-pin ${p.id === selectedId ? "selected" : ""} ${p.id === hoveredId ? "hovered" : ""}`}
          >
            <path d={teardrop(s)} />
            <circle className="plan-pin-dot" cx={h.x} cy={h.y} r={2.2} />
          </g>
        );
      })}
      {pending && (
        <g className="plan-pin pending">
          <path d={teardrop(toScreen(pending))} />
        </g>
      )}
    </g>
  );
}

/** Hover readout for a pin. */
export function PinTooltip({ pin, toScreen }: { pin: PinMarker; toScreen: (p: XY) => XY }) {
  const s = toScreen(pin);
  const category = pinCategoryLabel(pin.category);
  return (
    <div className="plan-tooltip plan-pin-tooltip" style={{ left: s.x + 12, top: s.y - PIN_H - 6 }}>
      {category && <strong>{category}</strong>}
      {category && <br />}
      <span>{pin.text.length > 160 ? `${pin.text.slice(0, 160)}…` : pin.text}</span>
    </div>
  );
}
