import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { api, asApiError } from "../api/tauri";
import type {
  Adapter,
  AdapterId,
  AdapterListing,
  ApiError,
  ConnectionInfo,
  ScanResult,
} from "../types/wifi";

const SELECTED_KEY = "fresnel.selectedAdapter";
const AUTO_INTERVAL_KEY = "fresnel.autoScanSeconds";

interface WifiState {
  listing: AdapterListing | null;
  adaptersLoading: boolean;
  adaptersError: ApiError | null;
  selectedId: AdapterId | null;
  selectedAdapter: Adapter | null;
  selectAdapter: (id: AdapterId) => void;
  refreshAdapters: () => Promise<void>;

  scan: ScanResult | null;
  scanning: boolean;
  scanError: ApiError | null;
  runScan: (trigger?: boolean) => Promise<void>;

  connection: ConnectionInfo | null;
  connectionError: ApiError | null;

  autoScan: boolean;
  setAutoScan: (on: boolean) => void;
  autoScanSeconds: number;
  setAutoScanSeconds: (s: number) => void;
  /** Auto-scan only runs while a page showing live results is mounted; returns the unregister function. */
  watchAutoScan: () => () => void;
  /** Pause auto-scan while the radio is needed elsewhere (Measure Here); returns the release function. */
  holdAutoScan: () => () => void;
}

export const AUTO_SCAN_MIN_S = 5;
export const AUTO_SCAN_MAX_S = 300;

export const clampAutoScanSeconds = (s: number) =>
  Math.max(AUTO_SCAN_MIN_S, Math.min(AUTO_SCAN_MAX_S, Math.round(s)));

const WifiContext = createContext<WifiState | null>(null);

function readStorage(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function writeStorage(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    /* non-essential */
  }
}

export function WifiProvider({ children }: { children: ReactNode }) {
  const [listing, setListing] = useState<AdapterListing | null>(null);
  const [adaptersLoading, setAdaptersLoading] = useState(false);
  const [adaptersError, setAdaptersError] = useState<ApiError | null>(null);
  const [selectedId, setSelectedId] = useState<AdapterId | null>(() => readStorage(SELECTED_KEY));

  const [scan, setScan] = useState<ScanResult | null>(null);
  const [scanning, setScanning] = useState(false);
  const [scanError, setScanError] = useState<ApiError | null>(null);

  const [connection, setConnection] = useState<ConnectionInfo | null>(null);
  const [connectionError, setConnectionError] = useState<ApiError | null>(null);

  const [autoScan, setAutoScan] = useState(false);
  const [autoScanSeconds, setAutoScanSecondsState] = useState(() =>
    clampAutoScanSeconds(Number(readStorage(AUTO_INTERVAL_KEY)) || 15),
  );
  const [autoScanViewers, setAutoScanViewers] = useState(0);
  const [autoScanHolds, setAutoScanHolds] = useState(0);

  // Guards against applying results for an adapter that is no longer selected.
  const selectedRef = useRef(selectedId);
  selectedRef.current = selectedId;
  const scanningRef = useRef<AdapterId | null>(null);

  const refreshAdapters = useCallback(async () => {
    setAdaptersLoading(true);
    try {
      const l = await api.listAdapters();
      setListing(l);
      setAdaptersError(null);
      setSelectedId((current) => {
        if (current && l.adapters.some((a) => a.id === current)) return current;
        const preferred = l.adapters.find((a) => a.status === "connected") ?? l.adapters[0];
        return preferred?.id ?? current;
      });
    } catch (e) {
      setAdaptersError(asApiError(e));
    } finally {
      setAdaptersLoading(false);
    }
  }, []);

  const refreshConnection = useCallback(async (id: AdapterId) => {
    try {
      const c = await api.currentConnection(id);
      if (selectedRef.current !== id) return;
      setConnection(c);
      setConnectionError(null);
    } catch (e) {
      if (selectedRef.current !== id) return;
      setConnection(null);
      setConnectionError(asApiError(e));
    }
  }, []);

  const runScan = useCallback(
    async (trigger = true) => {
      const id = selectedRef.current;
      if (!id || scanningRef.current === id) return;
      scanningRef.current = id;
      setScanning(trigger);
      try {
        const r = await api.scan(id, { trigger });
        if (selectedRef.current !== id) return;
        setScan(r);
        setScanError(null);
      } catch (e) {
        if (selectedRef.current !== id) return;
        setScanError(asApiError(e));
      } finally {
        if (scanningRef.current === id) scanningRef.current = null;
        if (selectedRef.current === id) setScanning(false);
      }
      // Adapter status/connection may have changed (or adapter vanished).
      void refreshConnection(id);
      void refreshAdapters();
    },
    [refreshAdapters, refreshConnection],
  );

  const selectAdapter = useCallback((id: AdapterId) => {
    setSelectedId(id);
  }, []);

  const setAutoScanSeconds = useCallback((s: number) => {
    const clamped = clampAutoScanSeconds(s);
    setAutoScanSecondsState(clamped);
    writeStorage(AUTO_INTERVAL_KEY, String(clamped));
  }, []);

  const watchAutoScan = useCallback(() => {
    setAutoScanViewers((n) => n + 1);
    return () => setAutoScanViewers((n) => n - 1);
  }, []);

  const holdAutoScan = useCallback(() => {
    let released = false;
    setAutoScanHolds((n) => n + 1);
    return () => {
      if (released) return;
      released = true;
      setAutoScanHolds((n) => n - 1);
    };
  }, []);

  useEffect(() => {
    void refreshAdapters();
  }, [refreshAdapters]);

  // On selection change: show the provider's cached results immediately,
  // without touching the radio, plus connection details.
  useEffect(() => {
    if (!selectedId) return;
    writeStorage(SELECTED_KEY, selectedId);
    setScan(null);
    setScanning(false);
    setScanError(null);
    setConnection(null);
    setConnectionError(null);
    void runScan(false);
  }, [selectedId, runScan]);

  // Only while someone looks at live results, and never queued ahead of a measurement
  // (the backend may space scans on one adapter apart, see Adapter.scanSpacingMs).
  const autoScanRunning = autoScan && autoScanViewers > 0 && autoScanHolds === 0;
  useEffect(() => {
    if (!autoScanRunning || !selectedId) return;
    const t = window.setInterval(() => void runScan(true), autoScanSeconds * 1000);
    return () => window.clearInterval(t);
  }, [autoScanRunning, autoScanSeconds, selectedId, runScan]);

  // Pick up hot-plugged adapters / status changes when the window regains focus.
  useEffect(() => {
    const onFocus = () => void refreshAdapters();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [refreshAdapters]);

  const selectedAdapter = useMemo(
    () => listing?.adapters.find((a) => a.id === selectedId) ?? null,
    [listing, selectedId],
  );

  const value: WifiState = {
    listing,
    adaptersLoading,
    adaptersError,
    selectedId,
    selectedAdapter,
    selectAdapter,
    refreshAdapters,
    scan,
    scanning,
    scanError,
    runScan,
    connection,
    connectionError,
    autoScan,
    setAutoScan,
    autoScanSeconds,
    setAutoScanSeconds,
    watchAutoScan,
    holdAutoScan,
  };

  return <WifiContext.Provider value={value}>{children}</WifiContext.Provider>;
}

export function useWifi(): WifiState {
  const ctx = useContext(WifiContext);
  if (!ctx) throw new Error("useWifi must be used inside <WifiProvider>");
  return ctx;
}

/** Lets auto-scan run while the calling component is mounted. */
export function useAutoScanViewer() {
  const { watchAutoScan } = useWifi();
  useEffect(() => watchAutoScan(), [watchAutoScan]);
}
