import type { ErrorInfo } from "react";

/** Will forward render errors caught by an error boundary to the backend log. */
export function reportError(_error: unknown, _info?: ErrorInfo): void {}
