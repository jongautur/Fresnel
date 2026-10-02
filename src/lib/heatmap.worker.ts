// Builds heatmap grids off the UI thread. Bundled by Vite as its own file
// (a same-origin script, so the app CSP's 'self' covers it; blob: workers
// would be blocked).

import { runHeatJob, type HeatRequest, type HeatResponse } from "./heatmapJobs";

// The DOM lib types `self` as a Window; only postMessage is needed here.
const scope = self as unknown as {
  onmessage: ((e: MessageEvent<HeatRequest>) => void) | null;
  postMessage(message: HeatResponse, transfer: Transferable[]): void;
};

scope.onmessage = (e) => {
  const { id, job } = e.data;
  try {
    const result = runHeatJob(job);
    scope.postMessage({ id, ok: true, result }, result ? [result.rgba.buffer] : []);
  } catch (err) {
    scope.postMessage({ id, ok: false, message: err instanceof Error ? err.message : String(err) }, []);
  }
};
