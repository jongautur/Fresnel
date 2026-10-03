import type { ApiError } from "../../types/wifi";
import type { ToolRun } from "../../types/tools";
import { ErrorBanner } from "../ErrorBanner";
import { IconAlert } from "../Icons";
import { formatDateTime } from "../../lib/format";
import { RUN_STATUS_CLASS, RUN_STATUS_LABEL, fmtMs, shortLink } from "../../lib/tools";
import type { Started } from "./useToolRun";

/**
 * What a run talks to and over what: the address used (and the name it came
 * from), the interface, and the Wi-Fi link. Live while running, from the
 * stored run afterwards; plus its status and error.
 */
export function RunInfo({
  started,
  run,
  error,
  running,
}: {
  started: Started | null;
  run: ToolRun | null;
  error: ApiError | null;
  running: boolean;
}) {
  if (error) return <ErrorBanner error={error} />;
  const target = started?.target;
  const ip = target?.ip ?? run?.resolvedIp ?? null;
  const name = target?.input ?? run?.target ?? null;
  const iface = started?.routeIface ?? run?.routeIface ?? null;
  const bound = started ? started.adapterId != null : run?.adapterId != null;
  const link = shortLink(started?.link ?? run?.link ?? null);
  if (!running && !run && !started) return null;
  return (
    <div className="run-info">
      <div className="run-info-line">
        {running ? (
          <span className="status status-warn">
            <span className="status-dot" /> Running
          </span>
        ) : (
          run && (
            <span className={`status ${RUN_STATUS_CLASS[run.status]}`}>
              <span className="status-dot" /> {RUN_STATUS_LABEL[run.status]}
            </span>
          )
        )}
        {name && (
          <span className="mono">
            {name}
            {ip && ip !== name && <span className="muted"> → {ip}</span>}
          </span>
        )}
        {target?.resolveMs != null && <span className="muted small">resolved in {fmtMs(target.resolveMs)}</span>}
        {iface && (
          <span className="small">
            <span className="muted">{bound ? "bound to" : "via"}</span> <span className="mono">{iface}</span>
          </span>
        )}
        {link && <span className="small mono">{link}</span>}
        {run && !running && (
          <span className="muted small">
            {formatDateTime(run.startedAt)} · {(run.durationMs / 1000).toFixed(1)} s
          </span>
        )}
        {run?.roamed && <span className="status status-warn small">roamed during the run</span>}
      </div>
      {run && !running && run.error && (
        <div className="banner banner-error" role="alert">
          <IconAlert className="banner-icon" />
          <div>
            <strong>{run.status === "stopped" ? "Stopped" : "Failed"}</strong>
            <div className="banner-message">{run.error}</div>
            {run.errorHint && <div className="banner-hint">{run.errorHint}</div>}
          </div>
        </div>
      )}
    </div>
  );
}
