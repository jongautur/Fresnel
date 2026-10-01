import { Fragment, useMemo, useState } from "react";
import type { AccessPointObservation, Band } from "../types/wifi";
import {
  BAND_LABEL,
  SECURITY_LABEL,
  formatAge,
  formatBitrate,
  signalSortValue,
} from "../lib/format";
import { SignalCell } from "./SignalCell";

type SortKey =
  | "ssid"
  | "bssid"
  | "signal"
  | "channel"
  | "frequency"
  | "band"
  | "width"
  | "security"
  | "age";
type Dir = "asc" | "desc";

interface Column {
  key: SortKey;
  label: string;
  align?: "right";
  /** Initial direction when first clicked */
  defaultDir: Dir;
}

const COLUMNS: Column[] = [
  { key: "ssid", label: "SSID", defaultDir: "asc" },
  { key: "bssid", label: "BSSID", defaultDir: "asc" },
  { key: "signal", label: "Signal", defaultDir: "desc" },
  { key: "channel", label: "Ch", align: "right", defaultDir: "asc" },
  { key: "frequency", label: "Freq", align: "right", defaultDir: "asc" },
  { key: "band", label: "Band", defaultDir: "asc" },
  { key: "width", label: "Width", align: "right", defaultDir: "desc" },
  { key: "security", label: "Security", defaultDir: "asc" },
  { key: "age", label: "Seen", align: "right", defaultDir: "asc" },
];

const BAND_ORDER: Record<Band, number> = { "2.4ghz": 0, "5ghz": 1, "6ghz": 2, "60ghz": 3, unknown: 4 };

function compare(a: AccessPointObservation, b: AccessPointObservation, key: SortKey): number {
  const num = (x: number | null | undefined, y: number | null | undefined) =>
    (x ?? Number.POSITIVE_INFINITY) - (y ?? Number.POSITIVE_INFINITY);
  switch (key) {
    case "ssid":
      // Hidden networks sort last.
      if (a.ssid == null || b.ssid == null) return (a.ssid == null ? 1 : 0) - (b.ssid == null ? 1 : 0);
      return a.ssid.localeCompare(b.ssid, undefined, { sensitivity: "base" });
    case "bssid":
      return a.bssid.localeCompare(b.bssid);
    case "signal":
      return signalSortValue(a.signal) - signalSortValue(b.signal);
    case "channel":
      return BAND_ORDER[a.band] - BAND_ORDER[b.band] || num(a.channel, b.channel);
    case "frequency":
      return a.frequencyMhz - b.frequencyMhz;
    case "band":
      return BAND_ORDER[a.band] - BAND_ORDER[b.band];
    case "width":
      return num(a.channelWidthMhz, b.channelWidthMhz);
    case "security":
      return SECURITY_LABEL[a.security.kind].localeCompare(SECURITY_LABEL[b.security.kind]);
    case "age":
      return num(a.lastSeenAgeMs, b.lastSeenAgeMs);
  }
}

const BAND_FILTERS: { value: Band | "all"; label: string }[] = [
  { value: "all", label: "All" },
  { value: "2.4ghz", label: "2.4" },
  { value: "5ghz", label: "5" },
  { value: "6ghz", label: "6" },
];

/** Rows older than this are probably out of range; NM keeps BSSes for minutes. */
const STALE_MS = 30_000;

interface Group {
  key: string;
  ssid: string | null;
  rows: AccessPointObservation[];
}

export function WifiTable({ accessPoints }: { accessPoints: AccessPointObservation[] }) {
  const [sortKey, setSortKey] = useState<SortKey>("signal");
  const [dir, setDir] = useState<Dir>("desc");
  const [filter, setFilter] = useState("");
  const [band, setBand] = useState<Band | "all">("all");
  const [grouped, setGrouped] = useState(false);
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());

  const onSort = (col: Column) => {
    if (col.key === sortKey) setDir((d) => (d === "asc" ? "desc" : "asc"));
    else {
      setSortKey(col.key);
      setDir(col.defaultDir);
    }
  };

  const rows = useMemo(() => {
    const q = filter.trim().toLowerCase();
    const filtered = accessPoints.filter(
      (ap) =>
        (band === "all" || ap.band === band) &&
        (!q ||
          (ap.ssid ?? "").toLowerCase().includes(q) ||
          ap.bssid.toLowerCase().includes(q) ||
          String(ap.channel ?? "") === q),
    );
    const sign = dir === "asc" ? 1 : -1;
    // Tie-break on BSSID for a stable order between scans.
    return filtered.sort((a, b) => sign * compare(a, b, sortKey) || a.bssid.localeCompare(b.bssid));
  }, [accessPoints, filter, band, sortKey, dir]);

  // SSID grouping is purely presentational: groups follow the order in which
  // their first member appears in the sorted list.
  const groups = useMemo<Group[]>(() => {
    const map = new Map<string, Group>();
    for (const ap of rows) {
      const key = ap.ssid ?? "\u0000hidden";
      let g = map.get(key);
      if (!g) {
        g = { key, ssid: ap.ssid, rows: [] };
        map.set(key, g);
      }
      g.rows.push(ap);
    }
    return [...map.values()];
  }, [rows]);

  const allQuality = accessPoints.length > 0 && accessPoints.every((a) => a.signal.dbm == null);
  const bandCounts = useMemo(() => {
    const c: Partial<Record<Band, number>> = {};
    for (const ap of accessPoints) c[ap.band] = (c[ap.band] ?? 0) + 1;
    return c;
  }, [accessPoints]);

  const toggleGroup = (key: string) =>
    setCollapsed((s) => {
      const n = new Set(s);
      if (n.has(key)) n.delete(key);
      else n.add(key);
      return n;
    });

  const renderRow = (ap: AccessPointObservation, inGroup: boolean) => {
    const stale = ap.lastSeenAgeMs != null && ap.lastSeenAgeMs > STALE_MS;
    return (
      <tr
        key={ap.bssid}
        className={[ap.isConnected ? "row-connected" : "", stale ? "row-stale" : "", inGroup ? "row-child" : ""].join(" ")}
      >
        <td className="col-ssid">
          {ap.isConnected && <span className="connected-mark" title="Currently connected BSSID">●</span>}
          {ap.ssid ?? <span className="muted italic">hidden</span>}
        </td>
        <td className="mono">{ap.bssid}</td>
        <td><SignalCell signal={ap.signal} /></td>
        <td className="mono num">{ap.channel ?? "—"}</td>
        <td className="mono num">{ap.frequencyMhz}</td>
        <td><span className={`band band-${ap.band.replace(".", "_")}`}>{BAND_LABEL[ap.band]}</span></td>
        <td className="mono num">{ap.channelWidthMhz ?? "—"}</td>
        <td title={securityTitle(ap)}>{SECURITY_LABEL[ap.security.kind]}</td>
        <td className="mono num" title={stale ? "Not heard recently — probably out of range" : undefined}>
          {formatAge(ap.lastSeenAgeMs)}
        </td>
      </tr>
    );
  };

  return (
    <section className="card card-table">
      <header className="card-header table-toolbar">
        <h2>
          Visible BSSIDs <span className="count">{rows.length}{rows.length !== accessPoints.length && ` / ${accessPoints.length}`}</span>
        </h2>
        <div className="toolbar-controls">
          <div className="segmented" role="group" aria-label="Band filter">
            {BAND_FILTERS.map((b) => (
              <button
                key={b.value}
                type="button"
                className={band === b.value ? "active" : ""}
                onClick={() => setBand(b.value)}
              >
                {b.label}
                {b.value !== "all" && <span className="seg-count">{bandCounts[b.value] ?? 0}</span>}
              </button>
            ))}
          </div>
          <label className="checkbox">
            <input type="checkbox" checked={grouped} onChange={(e) => setGrouped(e.target.checked)} />
            Group by SSID
          </label>
          <input
            className="input search"
            placeholder="Filter SSID / BSSID / channel"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          />
        </div>
      </header>
      {allQuality && (
        <div className="table-note">
          Signal is NetworkManager's 0–100 % quality value, not dBm. A dBm-capable provider (nl80211) is planned.
        </div>
      )}
      <div className="table-scroll">
        <table className="data-table">
          <thead>
            <tr>
              {COLUMNS.map((c) => (
                <th
                  key={c.key}
                  className={`${c.align === "right" ? "num" : ""} ${sortKey === c.key ? "sorted" : ""}`}
                  onClick={() => onSort(c)}
                  aria-sort={sortKey === c.key ? (dir === "asc" ? "ascending" : "descending") : "none"}
                >
                  {c.key === "signal" && allQuality ? "Signal %" : c.label}
                  <span className="sort-arrow">{sortKey === c.key ? (dir === "asc" ? "▲" : "▼") : ""}</span>
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.length === 0 && (
              <tr>
                <td colSpan={COLUMNS.length} className="empty">
                  {accessPoints.length === 0 ? "No scan results yet. Press Scan." : "No BSSIDs match the filter."}
                </td>
              </tr>
            )}
            {!grouped && rows.map((ap) => renderRow(ap, false))}
            {grouped &&
              groups.map((g) => {
                const best = g.rows.reduce((m, r) => (signalSortValue(r.signal) > signalSortValue(m.signal) ? r : m));
                const bands = [...new Set(g.rows.map((r) => BAND_LABEL[r.band]))].join(", ");
                const isCollapsed = collapsed.has(g.key);
                return (
                  <Fragment key={g.key}>
                    <tr className="row-group" onClick={() => toggleGroup(g.key)}>
                      <td className="col-ssid">
                        <span className="group-toggle">{isCollapsed ? "▸" : "▾"}</span>
                        {g.ssid ?? <span className="muted italic">hidden networks</span>}
                        {g.rows.some((r) => r.isConnected) && <span className="connected-mark">●</span>}
                      </td>
                      <td className="muted">{g.rows.length} BSSID{g.rows.length > 1 ? "s" : ""}</td>
                      <td><SignalCell signal={best.signal} /></td>
                      <td className="mono num muted">{[...new Set(g.rows.map((r) => r.channel))].join(",")}</td>
                      <td />
                      <td className="muted">{bands}</td>
                      <td />
                      <td className="muted">{[...new Set(g.rows.map((r) => SECURITY_LABEL[r.security.kind]))].join(", ")}</td>
                      <td />
                    </tr>
                    {!isCollapsed && g.rows.map((ap) => renderRow(ap, true))}
                  </Fragment>
                );
              })}
          </tbody>
        </table>
      </div>
    </section>
  );
}

function securityTitle(ap: AccessPointObservation): string {
  const s = ap.security;
  const parts = [];
  if (s.akms.length) parts.push(`AKM: ${s.akms.join(", ")}`);
  if (s.pairwiseCiphers.length) parts.push(`Pairwise: ${s.pairwiseCiphers.join(", ")}`);
  if (s.groupCiphers.length) parts.push(`Group: ${s.groupCiphers.join(", ")}`);
  if (ap.maxBitrateKbps) parts.push(`Max rate: ${formatBitrate(ap.maxBitrateKbps)}`);
  return parts.join("\n");
}
