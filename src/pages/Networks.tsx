import { useMemo } from "react";
import { useWifi } from "../state/WifiContext";
import type { AccessPointObservation } from "../types/wifi";
import { BAND_LABEL, SECURITY_LABEL, signalSortValue } from "../lib/format";
import { SignalCell } from "../components/SignalCell";
import { usePreferences } from "../state/Preferences";

interface SsidSummary {
  ssid: string | null;
  bssids: AccessPointObservation[];
  best: AccessPointObservation;
}

/** SSID-level view of the latest scan. Presentation only — the data stays per BSSID. */
export function Networks() {
  const { scan, selectedAdapter } = useWifi();
  const { signalUnit } = usePreferences();

  const summaries = useMemo<SsidSummary[]>(() => {
    const map = new Map<string, AccessPointObservation[]>();
    for (const ap of scan?.accessPoints ?? []) {
      const k = ap.ssid ?? "\u0000";
      map.set(k, [...(map.get(k) ?? []), ap]);
    }
    return [...map.values()]
      .map((bssids) => ({
        ssid: bssids[0]!.ssid,
        bssids,
        best: bssids.reduce((m, r) =>
          signalSortValue(r.signal, signalUnit) > signalSortValue(m.signal, signalUnit) ? r : m,
        ),
      }))
      .sort((a, b) => signalSortValue(b.best.signal, signalUnit) - signalSortValue(a.best.signal, signalUnit));
  }, [scan, signalUnit]);

  return (
    <div className="page">
      <div className="page-header">
        <h1>Networks</h1>
        <p className="muted">
          SSIDs from the latest scan on {selectedAdapter?.interfaceName ?? "—"}. Run scans from the Live page.
        </p>
      </div>
      <section className="card card-table">
        <div className="table-scroll">
          <table className="data-table">
            <thead>
              <tr>
                <th>SSID</th>
                <th className="num">BSSIDs</th>
                <th>Best signal</th>
                <th>Bands</th>
                <th>Channels</th>
                <th>Security</th>
              </tr>
            </thead>
            <tbody>
              {summaries.length === 0 && (
                <tr><td colSpan={6} className="empty">No scan data yet.</td></tr>
              )}
              {summaries.map((s) => (
                <tr key={s.ssid ?? "\u0000"} className={s.bssids.some((b) => b.isConnected) ? "row-connected" : ""}>
                  <td className="col-ssid">{s.ssid ?? <span className="muted italic">hidden</span>}</td>
                  <td className="mono num">{s.bssids.length}</td>
                  <td><SignalCell signal={s.best.signal} /></td>
                  <td>{[...new Set(s.bssids.map((b) => BAND_LABEL[b.band]))].join(", ")}</td>
                  <td className="mono">{[...new Set(s.bssids.map((b) => b.channel ?? "?"))].join(", ")}</td>
                  <td>{[...new Set(s.bssids.map((b) => SECURITY_LABEL[b.security.kind]))].join(", ")}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      </section>
    </div>
  );
}
