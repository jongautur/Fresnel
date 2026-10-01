import type { Signal } from "../types/wifi";
import { signalView } from "../lib/format";
import { usePreferences } from "../state/Preferences";

export function SignalCell({
  signal,
  compact = false,
  description,
}: {
  signal: Signal;
  compact?: boolean;
  /** Replaces the default "Received signal strength" tooltip for dBm values. */
  description?: string;
}) {
  const { signalUnit } = usePreferences();
  const v = signalView(signal, signalUnit);
  const title =
    v.unit === "%"
      ? "Signal quality reported by NetworkManager (0–100 %). This is not a dBm value."
      : v.unit === "dBm"
        ? (description ?? "Received signal strength")
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
