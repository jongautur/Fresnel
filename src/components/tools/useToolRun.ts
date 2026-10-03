import { useCallback, useRef, useState } from "react";
import { api, asApiError } from "../../api/tauri";
import type { ApiError } from "../../types/wifi";
import type { ToolEvent, ToolKind, ToolRun } from "../../types/tools";
import { newRunId } from "../../lib/tools";

export type Started = Extract<ToolEvent, { event: "started" }>;

/**
 * One tool's run lifecycle: start (with live events), stop, the stored run
 * when it ends, or the error when it couldn't start. Survives tab switches
 * because the Tools page stays mounted.
 */
export function useToolRun(kind: ToolKind, onStored?: (run: ToolRun) => void) {
  const [running, setRunning] = useState(false);
  const [stopping, setStopping] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [run, setRun] = useState<ToolRun | null>(null);
  const [started, setStarted] = useState<Started | null>(null);
  const runId = useRef<string | null>(null);

  const start = useCallback(
    async (
      exec: (runId: string, onEvent: (e: ToolEvent) => void) => Promise<ToolRun>,
      onEvent: (e: ToolEvent) => void,
    ) => {
      const id = newRunId(kind);
      runId.current = id;
      setRunning(true);
      setStopping(false);
      setError(null);
      setRun(null);
      setStarted(null);
      try {
        const stored = await exec(id, (e) => {
          if (runId.current !== id) return;
          if (e.event === "started") setStarted(e);
          onEvent(e);
        });
        if (runId.current === id) setRun(stored);
        onStored?.(stored);
      } catch (e) {
        if (runId.current === id) setError(asApiError(e));
      } finally {
        if (runId.current === id) {
          setRunning(false);
          setStopping(false);
        }
      }
    },
    [kind, onStored],
  );

  const stop = useCallback(() => {
    if (!runId.current) return;
    setStopping(true);
    api.cancelTool(runId.current).catch(() => setStopping(false));
  }, []);

  /** Show a stored run from history instead of the live one. */
  const show = useCallback((stored: ToolRun) => {
    runId.current = null;
    setRunning(false);
    setStopping(false);
    setError(null);
    setStarted(null);
    setRun(stored);
  }, []);

  return { running, stopping, error, run, started, start, stop, show };
}
