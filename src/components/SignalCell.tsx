import type { Signal } from "../types/wifi";
import { signalView } from "../lib/format";

export function SignalCell({ signal, compact = false }: { signal: Signal; compact?: boolean }) {
  const v = signalView(signal);
  const title =
    v.unit === "%"
      ? "Signal quality reported by NetworkManager (0–100 %). This is not a dBm value."
      : v.unit === "dBm"
        ? "Received signal strength"
        : "No signal reading";
  return (
    <span className={`signal signal-${v.level}`} title={title}>
      {!compact && (
        <span className="signal-bar">
          <span style={{ width: `${v.fill}%` }} />
        </span>
      )}
      <span className="signal-value">
        {v.value}
        {v.unit && <span className="unit">{v.unit === "%" ? "%" : " dBm"}</span>}
      </span>
    </span>
  );
}
