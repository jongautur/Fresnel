// Heatmap grid jobs: plain, structured-clone-safe descriptions of one grid,
// run by the worker (heatmap.worker.ts) or, as a fallback, right here.
// No DOM: the worker, the report and the node scripts all import this.

import type { FloorPlan } from "../types/survey";
import { buildGrid, buildServingGrid, type HeatGrid, type HeatPoint, type HeatmapConfig, type ServingAp, type ServingGrid } from "./heatmap";
import { buildRequirementsGrid, type AreaInputs } from "./requirements";

export type HeatJob =
  | { kind: "grid"; plan: FloorPlan; ppm: number | null; pts: HeatPoint[]; cfg: HeatmapConfig }
  | { kind: "serving"; plan: FloorPlan; ppm: number | null; inputs: ServingAp[]; threshold: number }
  | { kind: "requirements"; plan: FloorPlan; ppm: number | null; inputs: AreaInputs };

export type HeatResult = HeatGrid | ServingGrid | null;

export function runHeatJob(job: HeatJob): HeatResult {
  switch (job.kind) {
    case "grid":
      return buildGrid(job.plan, job.ppm, job.pts, job.cfg);
    case "serving":
      return buildServingGrid(job.plan, job.ppm, job.inputs, job.threshold);
    case "requirements":
      return buildRequirementsGrid(job.plan, job.ppm, job.inputs);
  }
}

/** Worker protocol. */
export interface HeatRequest {
  id: number;
  job: HeatJob;
}

export type HeatResponse = { id: number; ok: true; result: HeatResult } | { id: number; ok: false; message: string };
