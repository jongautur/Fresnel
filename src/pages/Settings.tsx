import { useEffect, useState } from "react";
import { api, asApiError } from "../api/tauri";
import type { AppInfo } from "../types/project";
import type { ApiError } from "../types/wifi";
import { AUTO_SCAN_MAX_S, AUTO_SCAN_MIN_S, useWifi } from "../state/WifiContext";
import { usePreferences, type SignalUnit } from "../state/Preferences";
import { ErrorBanner } from "../components/ErrorBanner";
import { KeyValueGrid, KV } from "../components/KeyValue";
import { NumberInput } from "../components/NumberInput";

export type Theme = "system" | "dark" | "light";

export function Settings({ theme, setTheme }: { theme: Theme; setTheme: (t: Theme) => void }) {
  const { autoScanSeconds, setAutoScanSeconds } = useWifi();
  const { signalUnit, setSignalUnit } = usePreferences();
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<ApiError | null>(null);

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
              dBm is the measured signal level. % is NetworkManager's 0–100 quality value. If the chosen value isn't
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
            <div className="field-hint">
              Scans on the same adapter are always at least 5.5 s apart: scans started sooner after the previous one
              often hear only the connected network.
            </div>
          </KV>
        </KeyValueGrid>
      </section>
      <section className="card">
        <header className="card-header"><h2>About</h2></header>
        {error && <div className="pad"><ErrorBanner error={error} compact /></div>}
        {info && (
          <KeyValueGrid>
            <KV k="Version" mono>{info.version}</KV>
            <KV k="Database" mono>{info.databasePath}</KV>
            <KV k="Schema version" mono>{info.schemaVersion}</KV>
            {info.databaseError && <KV k="Database error">{info.databaseError}</KV>}
          </KeyValueGrid>
        )}
      </section>
    </div>
  );
}
