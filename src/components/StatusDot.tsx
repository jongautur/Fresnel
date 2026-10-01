import type { AdapterStatus } from "../types/wifi";
import { STATUS_LABEL } from "../lib/format";

const TONE: Record<AdapterStatus, string> = {
  connected: "ok",
  connecting: "warn",
  disconnecting: "warn",
  disconnected: "idle",
  unavailable: "bad",
  unmanaged: "idle",
  radio_off: "bad",
  failed: "bad",
  unknown: "idle",
};

export function StatusDot({ status, label = true }: { status: AdapterStatus; label?: boolean }) {
  return (
    <span className={`status status-${TONE[status]}`}>
      <span className="status-dot" />
      {label && STATUS_LABEL[status]}
    </span>
  );
}
