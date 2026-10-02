import type { PointTest } from "../../types/pointTests";
import { formatDateTime } from "../../lib/format";
import { ROLE_LABEL, linkSummary, methodLabel, methodNote, resultSummary, roamedLabel } from "../../lib/pointTests";

const STATUS_CLASS = { ok: "status-ok", failed: "status-bad", cancelled: "status-idle" } as const;
const STATUS_LABEL = { ok: "OK", failed: "Failed", cancelled: "Cancelled" } as const;

/** Active tests: one row each, with the link at test time and any error with its likely causes. */
export function PointTestsTable({
  tests,
  pointNumbers,
}: {
  tests: PointTest[];
  /** Point id → its number on the plan; shows a Point column when given. */
  pointNumbers?: Map<number, number>;
}) {
  return (
    <div className="table-scroll">
      <table className="data-table point-tests-table">
        <thead>
          <tr>
            {pointNumbers && <th className="num">Point</th>}
            <th>Test</th>
            <th>Target</th>
            <th>Method</th>
            <th>Result</th>
            <th>Link at test time</th>
            <th>Roamed</th>
            <th>Status</th>
          </tr>
        </thead>
        <tbody>
          {tests.map((t) => (
            <tr key={t.id}>
              {pointNumbers && <td className="num mono">{pointNumbers.get(t.pointId) ?? "?"}</td>}
              <td title={formatDateTime(t.startedAt)}>{ROLE_LABEL[t.role]}</td>
              <td className="mono">{t.target}</td>
              <td title={methodNote(t)}>{methodLabel(t)}</td>
              <td className="mono">{resultSummary(t)}</td>
              <td className="mono" title={t.link.bssid ?? undefined}>
                {linkSummary(t.link)}
              </td>
              <td>{roamedLabel(t)}</td>
              <td className="wrap">
                <span className={`status ${STATUS_CLASS[t.status]}`}>{STATUS_LABEL[t.status]}</span>
                {t.error && <div className="small">{t.error}</div>}
                {t.errorHint && <div className="muted small">{t.errorHint}</div>}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
