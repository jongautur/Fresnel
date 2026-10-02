import { useEffect, useState } from "react";
import { api, asApiError } from "../api/tauri";
import type { AppInfo } from "../types/project";
import type { Adapter, ApiError } from "../types/wifi";
import { AUTO_SCAN_MAX_S, AUTO_SCAN_MIN_S, useWifi } from "../state/WifiContext";
import { usePreferences, type SignalUnit } from "../state/Preferences";
import { ErrorBanner } from "../components/ErrorBanner";
import { KeyValueGrid, KV } from "../components/KeyValue";
import { NumberInput } from "../components/NumberInput";
import { BrandingSettings } from "../components/BrandingSettings";

export type Theme = "system" | "dark" | "light";

export function Settings({ theme, setTheme }: { theme: Theme; setTheme: (t: Theme) => void }) {
  const { autoScanSeconds, setAutoScanSeconds, selectedAdapter } = useWifi();
  const { signalUnit, setSignalUnit } = usePreferences();
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const [copied, setCopied] = useState<"idle" | "copied" | "failed">("idle");

  const copyDiagnostics = async () => {
    try {
      await navigator.clipboard.writeText(await api.diagnosticsReport());
      setCopied("copied");
    } catch (e) {
      setCopied("failed");
      setError(asApiError(e));
    }
  };

  useEffect(() => {
    api.appInfo().then(setInfo, (e) => setError(asApiError(e)));
  }, []);

  return (
    <div className="page">
      <div className="page-header"><h1>Settings</h1></div>
      <section className="card">
        <header className="card-header"><h2>Appearance & scanning</h2></header>
        <KeyValueGrid>
          <KV k="Theme">
            <div className="segmented">
              {(["system", "dark", "light"] as Theme[]).map((t) => (
                <button key={t} type="button" className={theme === t ? "active" : ""} onClick={() => setTheme(t)}>
                  {t[0]!.toUpperCase() + t.slice(1)}
                </button>
              ))}
            </div>
          </KV>
          <KV k="Signal unit">
            <div className="segmented">
              {(
                [
                  ["dbm", "dBm"],
                  ["percent", "%"],
                ] as [SignalUnit, string][]
              ).map(([u, label]) => (
                <button key={u} type="button" className={signalUnit === u ? "active" : ""} onClick={() => setSignalUnit(u)}>
                  {label}
                </button>
              ))}
            </div>
            <div className="field-hint">
              dBm is the measured signal level. % is the OS's 0–100 quality value. If the chosen value isn't
              available for a network, the other is shown with its own unit.
            </div>
          </KV>
          <KV k="Auto-scan interval">
            <NumberInput
              value={autoScanSeconds}
              min={AUTO_SCAN_MIN_S}
              max={AUTO_SCAN_MAX_S}
              onCommit={setAutoScanSeconds}
            />{" "}
            seconds
            <div className="field-hint">{scanSpacingHint(selectedAdapter)}</div>
          </KV>
        </KeyValueGrid>
      </section>
      <BrandingSettings />
      <section className="card">
        <header className="card-header"><h2>About</h2></header>
        {error && <div className="pad"><ErrorBanner error={error} compact /></div>}
        {info && (
          <KeyValueGrid>
            <KV k="Version" mono>{info.version}</KV>
            <KV k="Database" mono>{info.databasePath}</KV>
            <KV k="Schema version" mono>{info.schemaVersion}</KV>
            {info.databaseError && <KV k="Database error">{info.databaseError}</KV>}
            {info.databaseNotice && <KV k="Database notice">{info.databaseNotice}</KV>}
          </KeyValueGrid>
        )}
        <div className="pad panel-actions">
          <button type="button" className="btn" onClick={() => void copyDiagnostics()}>
            Copy diagnostics
          </button>
          <span className="muted small">
            {copied === "copied"
              ? "Copied. Paste it into your bug report."
              : copied === "failed"
                ? "Could not copy the report."
                : "Versions, adapters, database state and recent log lines, for bug reports."}
          </span>
        </div>
      </section>
    </div>
  );
}

/** The scanner's pause between scans on the selected adapter, as the backend reports it. */
function scanSpacingHint(adapter: Adapter | null): string {
  if (!adapter || adapter.scanSpacingMs === null) {
    return "Scans on one adapter run one at a time. Some adapters also need a pause between scans; select one to see it.";
  }
  const name = adapter.interfaceName ?? adapter.displayName;
  if (adapter.scanSpacingMs === 0) return `Scans on ${name} run one at a time, with no pause between them.`;
  const seconds = (adapter.scanSpacingMs / 1000).toFixed(1);
  return (
    `Scans on ${name} are always at least ${seconds} s apart: on this adapter, scans started sooner after ` +
    "the previous one can come back incomplete."
  );
}
