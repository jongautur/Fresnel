import type { ApiError } from "../types/wifi";
import { IconAlert } from "./Icons";

const TITLES: Partial<Record<ApiError["kind"], string>> = {
  service_unavailable: "NetworkManager unavailable",
  no_adapters: "No Wi-Fi adapters",
  adapter_not_found: "Adapter not found",
  adapter_unavailable: "Adapter unavailable",
  radio_disabled: "Wi-Fi radio is off",
  permission_denied: "Permission denied",
  scan_rejected: "Scan rejected",
  timeout: "Timed out",
  unsupported: "Not supported",
  database: "Database error",
  invalid_input: "Not possible",
  adapter_mismatch: "Different adapter",
  ipc: "Backend not reachable",
};

const HINTS: Partial<Record<ApiError["kind"], string>> = {
  radio_disabled: "Enable Wi-Fi in system settings, or check the hardware switch / airplane mode.",
  adapter_not_found: "The adapter may have been unplugged. Pick another adapter or refresh the list.",
  permission_denied:
    "NetworkManager's polkit policy did not allow this. Make sure you are in an active local session.",
};

export function ErrorBanner({ error, compact = false }: { error: ApiError; compact?: boolean }) {
  const title = TITLES[error.kind] ?? "Error";
  const hint = HINTS[error.kind];
  return (
    <div className={`banner banner-error ${compact ? "banner-compact" : ""}`} role="alert">
      <IconAlert className="banner-icon" />
      <div>
        <strong>{title}</strong>
        <div className="banner-message">{error.message}</div>
        {hint && !compact && <div className="banner-hint">{hint}</div>}
      </div>
    </div>
  );
}

export function NoticeBanner({ children }: { children: React.ReactNode }) {
  return (
    <div className="banner banner-notice" role="status">
      <IconAlert className="banner-icon" />
      <div className="banner-message">{children}</div>
    </div>
  );
}
