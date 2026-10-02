import type { ErrorInfo } from "react";
import { api } from "../api/tauri";

/** Forwards render errors caught by an error boundary to the backend log. Never throws. */
export function reportError(error: unknown, info?: ErrorInfo): void {
  try {
    const message = error instanceof Error ? `${error.name}: ${error.message}` : String(error);
    const stack = [error instanceof Error ? error.stack : undefined, info?.componentStack]
      .filter(Boolean)
      .join("\n--- component stack ---\n");
    api.logFrontendError({ message, stack: stack || undefined, source: "error boundary" }).catch(() => {});
  } catch {
    // Reporting must not raise errors of its own.
  }
}
