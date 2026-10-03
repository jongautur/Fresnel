import { useCallback, useEffect, useState } from "react";
import { api, asApiError } from "../../api/tauri";
import type { ApiError } from "../../types/wifi";
import type { ToolKind, ToolRun } from "../../types/tools";
import { ErrorBanner } from "../ErrorBanner";
import { ConfirmButton } from "../ConfirmButton";
import { formatDateTime } from "../../lib/format";
import { RUN_STATUS_CLASS, RUN_STATUS_LABEL } from "../../lib/tools";

/**
 * Earlier runs of one tool, newest first: open one to see its results,
 * "Again" to load its settings into the form. Runs attached to survey points
 * are kept when the history is cleared.
 */
export function ToolHistory({
  kind,
  refreshKey,
  selectedId,
  onOpen,
  onAgain,
}: {
  kind: ToolKind;
  /** Changes when a new run was stored. */
  refreshKey: number;
  selectedId: number | null;
  onOpen: (run: ToolRun) => void;
  onAgain: (run: ToolRun) => void;
}) {
  const [runs, setRuns] = useState<ToolRun[] | null>(null);
  const [error, setError] = useState<ApiError | null>(null);

  const load = useCallback(() => {
    api.listToolRuns(kind, 100).then(
      (r) => {
        setRuns(r);
        setError(null);
      },
      (e) => setError(asApiError(e)),
    );
  }, [kind]);

  useEffect(load, [load, refreshKey]);

  const open = async (id: number) => {
    try {
      const run = await api.getToolRun(id);
      if (run) onOpen(run);
      else load();
    } catch (e) {
      setError(asApiError(e));
    }
  };

  const remove = async (id: number) => {
    try {
      await api.deleteToolRun(id);
      load();
    } catch (e) {
      setError(asApiError(e));
    }
  };

  const clear = async () => {
    try {
      await api.clearToolRuns(kind);
      load();
    } catch (e) {
      setError(asApiError(e));
    }
  };

  return (
    <section className="card">
      <header className="card-header">
        <h2>History</h2>
        {runs && runs.length > 0 && (
          <ConfirmButton onConfirm={() => void clear()} confirmLabel="Clear history" title="Runs attached to survey points are kept">
            Clear
          </ConfirmButton>
        )}
      </header>
      {error && (
        <div className="pad">
          <ErrorBanner error={error} compact />
        </div>
      )}
      {runs && runs.length === 0 && <p className="pad muted">No runs yet. Runs are saved here when they end.</p>}
      {runs && runs.length > 0 && (
        <div className="table-scroll tool-history">
          <table className="data-table">
            <tbody>
              {runs.map((r) => (
                <tr
                  key={r.id}
                  className={`row-clickable ${r.id === selectedId ? "row-highlighted" : ""}`}
                  onClick={() => void open(r.id)}
                >
                  <td className="muted small nowrap">{formatDateTime(r.startedAt)}</td>
                  <td className="mono">{r.target}</td>
                  <td className="mono wrap">{r.summary ?? r.error ?? "—"}</td>
                  <td className="nowrap">
                    <span className={`status ${RUN_STATUS_CLASS[r.status]}`}>{RUN_STATUS_LABEL[r.status]}</span>
                    {r.pointId != null && <span className="chip" title="Attached to a survey point">survey</span>}
                  </td>
                  <td className="nowrap" onClick={(e) => e.stopPropagation()}>
                    <button type="button" className="btn btn-small" onClick={() => onAgain(r)} title="Load these settings">
                      Again
                    </button>{" "}
                    <ConfirmButton onConfirm={() => void remove(r.id)}>Delete</ConfirmButton>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}
