// Runs heatmap grid jobs on a Web Worker, falling back to the UI thread when
// a worker can't be started (or dies), so a heatmap is slower but never lost.

import { useEffect, useRef, useState, type DependencyList } from "react";
import { runHeatJob, type HeatJob, type HeatRequest, type HeatResponse, type HeatResult } from "./heatmapJobs";

interface Pending {
  job: HeatJob;
  resolve: (r: HeatResult) => void;
  reject: (e: Error) => void;
}

/** undefined: not started yet; null: unavailable, jobs run here. */
let worker: Worker | null | undefined;
const pending = new Map<number, Pending>();
let nextId = 1;

/** Run a job on this thread, as a separate task so callers stay async. */
function runHere(job: HeatJob): Promise<HeatResult> {
  return new Promise((resolve, reject) =>
    setTimeout(() => {
      try {
        resolve(runHeatJob(job));
      } catch (e) {
        reject(e instanceof Error ? e : new Error(String(e)));
      }
    }, 0),
  );
}

/** Stop using the worker; whatever it still owed is computed here. */
function giveUp(reason: string) {
  console.warn(`Heatmap worker unavailable (${reason}); computing on the UI thread.`);
  worker?.terminate();
  worker = null;
  const owed = [...pending.values()];
  pending.clear();
  for (const p of owed) runHere(p.job).then(p.resolve, p.reject);
}

function getWorker(): Worker | null {
  if (worker !== undefined) return worker;
  try {
    const w = new Worker(new URL("./heatmap.worker.ts", import.meta.url), { type: "module" });
    w.onmessage = (e: MessageEvent<HeatResponse>) => {
      const p = pending.get(e.data.id);
      if (!p) return;
      pending.delete(e.data.id);
      if (e.data.ok) p.resolve(e.data.result);
      else p.reject(new Error(e.data.message));
    };
    // Fires when the script can't load (e.g. blocked) or throws at top level.
    w.onerror = (e) => {
      e.preventDefault();
      giveUp(e.message || "worker error");
    };
    w.onmessageerror = () => giveUp("a result could not be received");
    worker = w;
  } catch (e) {
    worker = null;
    console.warn(`Heatmap worker could not start (${e instanceof Error ? e.message : String(e)}); computing on the UI thread.`);
  }
  return worker;
}

/** Build one grid, on the worker when there is one. */
export function computeHeat(job: HeatJob): Promise<HeatResult> {
  const w = getWorker();
  if (!w) return runHere(job);
  return new Promise((resolve, reject) => {
    const id = nextId++;
    pending.set(id, { job, resolve, reject });
    try {
      w.postMessage({ id, job } satisfies HeatRequest);
    } catch (e) {
      // Not cloneable: a bug, but the heatmap can still be drawn here.
      pending.delete(id);
      console.warn("Heatmap job could not be sent to the worker:", e);
      runHere(job).then(resolve, reject);
    }
  });
}

export interface HeatState {
  grid: HeatResult;
  /** A newer grid is being computed; `grid` is the previous one. */
  computing: boolean;
  error: string | null;
}

/**
 * The grid for the latest job. At most one job is in flight per hook: while
 * it runs, newer requests replace each other and only the last one is
 * started, so dragging a slider doesn't queue up stale work. The previous
 * grid stays until the new one arrives (no flicker). A null job clears.
 */
export function useHeatGrid(makeJob: () => HeatJob | null, deps: DependencyList): HeatState {
  const [state, setState] = useState<HeatState>({ grid: null, computing: false, error: null });
  const busy = useRef(false);
  /** undefined: nothing waiting. */
  const queued = useRef<HeatJob | null | undefined>(undefined);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const start = (job: HeatJob | null) => {
    if (!job) {
      setState({ grid: null, computing: false, error: null });
      return;
    }
    busy.current = true;
    setState((s) => ({ ...s, computing: true }));
    computeHeat(job)
      .then(
        (grid) => ({ grid, error: null }),
        (e: unknown) => ({ grid: undefined, error: e instanceof Error ? e.message : String(e) }),
      )
      .then((r) => {
        busy.current = false;
        if (!mounted.current) return;
        const next = queued.current;
        queued.current = undefined;
        if (next !== undefined) {
          start(next);
          return;
        }
        setState((s) => ({ grid: r.grid === undefined ? s.grid : r.grid, computing: false, error: r.error }));
      });
  };

  useEffect(() => {
    const job = makeJob();
    if (busy.current) queued.current = job;
    else start(job);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
  return state;
}
