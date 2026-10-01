import { useEffect, useState } from "react";
import { api, asApiError } from "../api/tauri";
import type { AppInfo } from "../types/project";
import type { ApiError } from "../types/wifi";
import { useWifi } from "../state/WifiContext";
import { ErrorBanner } from "../components/ErrorBanner";
import { KeyValueGrid, KV } from "../components/KeyValue";

export type Theme = "system" | "dark" | "light";

export function Settings({ theme, setTheme }: { theme: Theme; setTheme: (t: Theme) => void }) {
  const { autoScanSeconds, setAutoScanSeconds } = useWifi();
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
          <KV k="Auto-scan interval">
            <input
              className="input input-narrow"
              type="number"
              min={5}
              max={300}
              value={autoScanSeconds}
              onChange={(e) => setAutoScanSeconds(Number(e.target.value))}
            />{" "}
            seconds
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
