import { useMemo, useState } from "react";
import type { PointTest } from "../../types/pointTests";
import type { PointEvaluation } from "../../types/requirements";
import {
  formatMbps,
  linkSummary,
  pointSpeeds,
  type GatewayPing,
  type PointSpeed,
  type Throughput,
} from "../../lib/pointTests";
import { PointTestsTable } from "./PointTestsTable";

type Column = "point" | "down" | "up" | "ping" | "loss" | "signal";

const SPEED_RULES = new Set(["download", "upload", "latency", "loss"]);

const mbps = (t: Throughput) => (t && t !== "failed" ? t.mbps : null);
const avgMs = (p: GatewayPing) => (p && p !== "failed" ? p.avgMs : null);
const loss = (p: GatewayPing) => (p && p !== "failed" ? p.lossPercent : null);
const ms = (v: number) => `${v < 10 ? v.toFixed(1) : v.toFixed(0)} ms`;

interface Row {
  id: number;
  number: number;
  speed: PointSpeed;
  /** Speed rules that failed here. */
  failing: Set<string>;
  /** "pass" | "fail" on the speed targets, null when none apply. */
  outcome: "pass" | "fail" | null;
}

function median(values: number[]): number | null {
  if (!values.length) return null;
  const s = [...values].sort((a, b) => a - b);
  const mid = Math.floor(s.length / 2);
  return s.length % 2 ? s[mid]! : (s[mid - 1]! + s[mid]!) / 2;
}

/**
 * One row per point: the latest download, upload and gateway ping, with
 * the link they ran over. Summary rows on top; any column sorts; a click
 * selects the point. Every single test stays one toggle away.
 */
export function PointSpeedTable({
  tests,
  pointIds,
  evaluations,
  selectedId,
  onSelect,
}: {
  tests: PointTest[];
  /** The floor's points in measurement order (their numbers on the plan). */
  pointIds: number[];
  evaluations: PointEvaluation[];
  selectedId: number | null;
  onSelect: (id: number) => void;
}) {
  const [sort, setSort] = useState<{ col: Column; desc: boolean }>({ col: "point", desc: false });
  const [showAll, setShowAll] = useState(false);
  const numbers = useMemo(() => new Map(pointIds.map((id, i) => [id, i + 1])), [pointIds]);

  const rows: Row[] = useMemo(() => {
    const speeds = pointSpeeds(tests);
    return [...speeds.entries()].map(([id, speed]) => {
      const rules = (evaluations.find((e) => e.pointId === id)?.rules ?? []).filter((r) => SPEED_RULES.has(r.rule));
      const failing = new Set(rules.filter((r) => r.outcome === "fail").map((r) => r.rule as string));
      return {
        id,
        number: numbers.get(id) ?? 0,
        speed,
        failing,
        outcome: failing.size ? "fail" : rules.some((r) => r.outcome === "pass") ? "pass" : null,
      };
    });
  }, [tests, evaluations, numbers]);

  const value = (r: Row, c: Column): number | null => {
    switch (c) {
      case "point":
        return r.number;
      case "down":
        return mbps(r.speed.down);
      case "up":
        return mbps(r.speed.up);
      case "ping":
        return avgMs(r.speed.ping);
      case "loss":
        return loss(r.speed.ping);
      case "signal":
        return r.speed.link?.signalDbm ?? null;
    }
  };

  const sorted = [...rows].sort((a, b) => {
    const va = value(a, sort.col);
    const vb = value(b, sort.col);
    // Missing values last either way.
    if (va == null && vb == null) return a.number - b.number;
    if (va == null) return 1;
    if (vb == null) return -1;
    return (sort.desc ? vb - va : va - vb) || a.number - b.number;
  });

  const stats = (c: Column) => {
    const vs = rows.map((r) => value(r, c)).filter((v): v is number => v != null);
    // "Worst" is the low end for throughput and signal, the high end for ping and loss.
    const higherIsBetter = c === "down" || c === "up" || c === "signal";
    const lo = vs.length ? Math.min(...vs) : null;
    const hi = vs.length ? Math.max(...vs) : null;
    return { median: median(vs), worst: higherIsBetter ? lo : hi, best: higherIsBetter ? hi : lo };
  };
  const fmt = (c: Column, v: number | null) =>
    v == null
      ? "—"
      : c === "down" || c === "up"
        ? formatMbps(v)
        : c === "ping"
          ? ms(v)
          : c === "loss"
            ? `${v.toFixed(0)} %`
            : `${v.toFixed(0)} dBm`;

  const header = (c: Column, label: string, title?: string) => (
    <th
      className={`num ${sort.col === c ? "sorted" : ""}`}
      title={title}
      onClick={() => setSort((s) => ({ col: c, desc: s.col === c ? !s.desc : c === "down" || c === "up" }))}
    >
      {label}
      {sort.col === c && <span className="sort-arrow">{sort.desc ? "▼" : "▲"}</span>}
    </th>
  );

  const tp = (t: Throughput, failed: boolean) =>
    t == null ? (
      <span className="muted">—</span>
    ) : t === "failed" ? (
      <span className="status-bad">failed</span>
    ) : (
      <span
        className={failed ? "status-bad" : undefined}
        title={t.retransmits != null ? `${t.retransmits} TCP retransmits (sender)` : undefined}
      >
        {formatMbps(t.mbps)}
        {failed && " ✗"}
      </span>
    );

  const summary = (["median", "worst", "best"] as const).map((k) => (
    <tr key={k} className="summary-row">
      <td className="muted small">{k === "median" ? "Median" : k === "worst" ? "Worst" : "Best"}</td>
      {(["down", "up", "ping", "loss", "signal"] as Column[]).map((c) => (
        <td key={c} className="num mono">
          {fmt(c, stats(c)[k])}
        </td>
      ))}
      <td colSpan={2} />
    </tr>
  ));

  return (
    <>
      <div className="table-scroll">
        <table className="data-table point-speed-table">
          <thead>
            <tr>
              {header("point", "Point")}
              {header("down", "↓ Mbit/s", "iperf3 download (server → this computer), receiver's average")}
              {header("up", "↑ Mbit/s", "iperf3 upload (this computer → server), receiver's average")}
              {header("ping", "Ping", "Average round trip to the gateway")}
              {header("loss", "Loss", "Gateway ping loss")}
              {header("signal", "Signal", "Signal of the connection when the tests started")}
              <th>Link</th>
              <th>Targets</th>
            </tr>
          </thead>
          <tbody>
            {rows.length > 1 && summary}
            {sorted.map((r) => {
              const p = r.speed.ping;
              return (
                <tr
                  key={r.id}
                  className={`row-clickable ${r.id === selectedId ? "row-highlighted" : ""}`}
                  onClick={() => onSelect(r.id)}
                >
                  <td className="num mono">{r.number}</td>
                  <td className="num mono">{tp(r.speed.down, r.failing.has("download"))}</td>
                  <td className="num mono">{tp(r.speed.up, r.failing.has("upload"))}</td>
                  <td className="num mono" title={p && p !== "failed" && p.jitterMs != null ? `jitter ${ms(p.jitterMs)}` : undefined}>
                    {p == null ? (
                      <span className="muted">—</span>
                    ) : p === "failed" || p.avgMs == null ? (
                      <span className="status-bad">no reply</span>
                    ) : (
                      <span className={r.failing.has("latency") ? "status-bad" : undefined}>
                        {ms(p.avgMs)}
                        {r.failing.has("latency") && " ✗"}
                      </span>
                    )}
                  </td>
                  <td className="num mono">
                    {p && p !== "failed" ? (
                      <span className={r.failing.has("loss") ? "status-bad" : undefined}>
                        {p.lossPercent.toFixed(0)} %{r.failing.has("loss") && " ✗"}
                      </span>
                    ) : (
                      <span className="muted">—</span>
                    )}
                  </td>
                  <td className="num mono">
                    {r.speed.link?.signalDbm != null ? `${r.speed.link.signalDbm.toFixed(0)} dBm` : "—"}
                  </td>
                  <td className="mono small" title={r.speed.link?.bssid ?? undefined}>
                    {r.speed.link ? linkSummary({ ...r.speed.link, signalDbm: null }) : "—"}
                  </td>
                  <td>
                    {r.outcome === "pass" && <span className="status status-ok">✓ meets</span>}
                    {r.outcome === "fail" && <span className="status status-bad">✗ misses</span>}
                    {r.outcome == null && <span className="muted small">—</span>}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
      <div className="pad">
        <button type="button" className="btn btn-small" onClick={() => setShowAll(!showAll)}>
          {showAll ? "Hide the individual tests" : `Show all ${tests.length} tests`}
        </button>
      </div>
      {showAll && <PointTestsTable tests={tests} pointNumbers={numbers} />}
    </>
  );
}
