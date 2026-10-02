import type { ApiError } from "../types/wifi";
import { IconAlert } from "./Icons";

const TITLES: Partial<Record<ApiError["kind"], string>> = {
  service_unavailable: "Wi-Fi service unavailable",
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
  backend: "Internal error",
  ipc: "Backend not reachable",
};

/** Fallback advice that holds on every OS; platform-specific advice comes from the provider as `error.hint`. */
const HINTS: Partial<Record<ApiError["kind"], string>> = {
  radio_disabled: "Enable Wi-Fi in system settings, or check the hardware switch / airplane mode.",
  adapter_not_found: "The adapter may have been unplugged. Pick another adapter or refresh the list.",
};

export function ErrorBanner({
  error,
  compact = false,
  source,
}: {
  error: ApiError;
  compact?: boolean;
  /** Where the error came from, e.g. the provider id of a provider issue. */
  source?: string;
}) {
  const title = TITLES[error.kind] ?? "Error";
  const hint = error.hint || HINTS[error.kind];
  return (
    <div className={`banner banner-error ${compact ? "banner-compact" : ""}`} role="alert">
      <IconAlert className="banner-icon" />
      <div>
        <strong>{title}</strong>
        {source && <span className="muted"> · {source}</span>}
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
