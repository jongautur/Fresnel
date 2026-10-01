import { useWifi } from "../state/WifiContext";
import { AdapterSelector } from "../components/AdapterSelector";
import { AdapterInfo } from "../components/AdapterInfo";
import { ConnectionInfo } from "../components/ConnectionInfo";
import { WifiTable } from "../components/WifiTable";
import { ErrorBanner, NoticeBanner } from "../components/ErrorBanner";
import { IconRadar } from "../components/Icons";
import { formatTime } from "../lib/format";

export function Live() {
  const {
    listing,
    adaptersError,
    selectedAdapter,
    scan,
    scanning,
    scanError,
    runScan,
    connection,
    connectionError,
    autoScan,
    setAutoScan,
    autoScanSeconds,
  } = useWifi();

  const noAdapters = listing && listing.adapters.length === 0 && listing.issues.length === 0;

  return (
    <div className="page">
      <div className="toolbar">
        <AdapterSelector />
        <div className="toolbar-actions">
          <button
            type="button"
            className="btn btn-primary"
            onClick={() => void runScan(true)}
            disabled={!selectedAdapter || scanning}
          >
            <IconRadar className={scanning ? "spin" : ""} />
            {scanning ? "Scanning…" : "Scan"}
          </button>
          <label className="checkbox" title="Repeat scans automatically (interval in Settings)">
            <input type="checkbox" checked={autoScan} onChange={(e) => setAutoScan(e.target.checked)} />
            Auto every {autoScanSeconds}s
          </label>
        </div>
        <div className="scan-meta">
          {scan && (
            <>
              <span>
                {scan.scanTriggered ? "Scanned" : "Cached"} {formatTime(scan.completedAt)}
              </span>
              <span className="sep">·</span>
              <span>{scan.accessPoints.length} BSSIDs</span>
              {scan.scanTriggered && (
                <>
                  <span className="sep">·</span>
                  <span>{((Date.parse(scan.completedAt) - Date.parse(scan.startedAt)) / 1000).toFixed(1)} s</span>
                </>
              )}
            </>
          )}
        </div>
      </div>

      {adaptersError && <ErrorBanner error={adaptersError} />}
      {listing?.issues.map((i) => <ErrorBanner key={i.provider} error={i.error} />)}
      {noAdapters && <ErrorBanner error={{ kind: "no_adapters", message: "No Wi-Fi interfaces were detected on this system." }} />}
      {scanError && <ErrorBanner error={scanError} />}
      {scan?.notice && <NoticeBanner>{scan.notice}</NoticeBanner>}

      <div className="grid-2">
        <AdapterInfo adapter={selectedAdapter} />
        <ConnectionInfo connection={connection} error={connectionError} />
      </div>

      <WifiTable accessPoints={scan?.accessPoints ?? []} />
    </div>
  );
}
