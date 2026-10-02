import { Component, type ErrorInfo, type ReactNode } from "react";
import { reportError } from "../lib/errorReport";
import { IconAlert } from "./Icons";

interface Props {
  children: ReactNode;
  /** Headline of the fallback, e.g. what stopped working. */
  title?: string;
  /** Pad the fallback like a page (for the top-level boundary). */
  page?: boolean;
}

/** Shows the error and a Reload button instead of a blank window when rendering fails. */
export class ErrorBoundary extends Component<Props, { error: Error | null }> {
  state: { error: Error | null } = { error: null };

  static getDerivedStateFromError(error: unknown) {
    return { error: error instanceof Error ? error : new Error(String(error)) };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    reportError(error, info);
  }

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;
    const banner = (
      <div className="banner banner-error" role="alert">
        <IconAlert className="banner-icon" />
        <div>
          <strong>{this.props.title ?? "Something went wrong"}</strong>
          <div className="banner-message">{error.message || error.name}</div>
          <div className="panel-actions">
            <button type="button" className="btn" onClick={() => window.location.reload()}>
              Reload
            </button>
          </div>
        </div>
      </div>
    );
    return this.props.page ? <div className="page">{banner}</div> : banner;
  }
}
