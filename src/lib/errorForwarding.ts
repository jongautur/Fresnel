// Forwards uncaught errors and unhandled promise rejections to the backend
// log, so they end up in the log file (release builds have no devtools).
// Rate-limited, and never throws: a failure here must not cause more errors.
import { api, type FrontendErrorReport } from "../api/tauri";

/** Burst size and steady rate: at most 10 at once, then one every 6 s. */
const BURST = 10;
const REFILL_MS = 6_000;
/** The same message within this window is sent only once. */
const DEDUPE_MS = 5_000;

let installed = false;
let tokens = BURST;
let lastRefill = 0;
let suppressed = 0;
const recent = new Map<string, number>();

function take(key: string, now: number): boolean {
  const refills = Math.floor((now - lastRefill) / REFILL_MS);
  if (refills > 0) {
    tokens = Math.min(BURST, tokens + refills);
    lastRefill += refills * REFILL_MS;
  }
  for (const [k, at] of recent) {
    if (now - at >= DEDUPE_MS) recent.delete(k);
  }
  if (recent.has(key) || tokens <= 0) {
    suppressed += 1;
    return false;
  }
  recent.set(key, now);
  tokens -= 1;
  return true;
}

function describe(value: unknown): { message: string; stack?: string } {
  if (value instanceof Error) {
    return { message: `${value.name}: ${value.message}`, stack: value.stack };
  }
  if (typeof value === "string") return { message: value };
  try {
    return { message: JSON.stringify(value) ?? String(value) };
  } catch {
    return { message: String(value) };
  }
}

function forward(report: FrontendErrorReport): void {
  try {
    const now = Date.now();
    if (!take(report.message, now)) return;
    if (suppressed > 0) {
      const note = `${suppressed} repeated or rate-limited error(s) before this were not logged`;
      report = { ...report, message: `${report.message} (${note})` };
      suppressed = 0;
    }
    api.logFrontendError(report).catch(() => {
      // Not in the desktop app, or the backend is gone: nothing more to do.
    });
  } catch {
    // Never let error reporting raise errors of its own.
  }
}

function onError(event: ErrorEvent): void {
  try {
    const { message, stack } =
      event.error != null
        ? describe(event.error)
        : { message: event.message || "unknown error", stack: undefined };
    const where = event.filename ? ` ${event.filename}:${event.lineno}:${event.colno}` : "";
    forward({ message, stack, source: `window.error${where}` });
  } catch {
    // ignore
  }
}

function onRejection(event: PromiseRejectionEvent): void {
  try {
    const { message, stack } = describe(event.reason);
    forward({ message, stack, source: "unhandledrejection" });
  } catch {
    // ignore
  }
}

/** Install the window handlers once; later calls do nothing. */
export function installErrorForwarding(): void {
  if (installed || typeof window === "undefined") return;
  installed = true;
  lastRefill = Date.now();
  window.addEventListener("error", onError);
  window.addEventListener("unhandledrejection", onRejection);
}
