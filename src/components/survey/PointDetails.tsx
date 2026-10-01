import type { SurveyPoint } from "../../types/survey";
import { BAND_LABEL, formatDateTime, phyShort } from "../../lib/format";
import { KeyValueGrid, KV } from "../KeyValue";
import { SignalCell } from "../SignalCell";
import { ConfirmButton } from "../ConfirmButton";

export function PointDetails({
  point,
  number,
  pxPerMetre,
  onDelete,
  apNameByBssid,
}: {
  point: SurveyPoint;
  number: number;
  pxPerMetre: number | null;
  onDelete: () => void;
  apNameByBssid?: Map<string, string>;
}) {
  const iface = point.adapter.id.split(":").slice(1).join(":") || point.adapter.id;
  return (
    <section className="card">
      <header className="card-header">
        <h2>
          Point <span className="count">{number}</span>
        </h2>
        <ConfirmButton onConfirm={onDelete} title="Delete this point and its readings">
          Delete
        </ConfirmButton>
      </header>
      <KeyValueGrid>
        <KV k="Measured" mono>{formatDateTime(point.measuredAt)}</KV>
        <KV k="Position" mono>
          {pxPerMetre
            ? `${(point.x / pxPerMetre).toFixed(1)} m, ${(point.y / pxPerMetre).toFixed(1)} m`
            : `${Math.round(point.x)}, ${Math.round(point.y)} px`}
        </KV>
        <KV k="Adapter">
          {point.adapter.model ?? iface} <span className="muted mono">{iface}</span>
        </KV>
        <KV k="Scan" mono>{(point.scanDurationMs / 1000).toFixed(1)} s</KV>
        <KV k="Heard" mono hint="Only BSSIDs heard during this point's scan are stored">
          {point.samples.length} BSSID{point.samples.length === 1 ? "" : "s"}
        </KV>
      </KeyValueGrid>
      {point.samples.length === 0 ? (
        <p className="muted pad">No networks were heard here.</p>
      ) : (
        <div className="table-scroll">
          <table className="data-table samples-table">
            <thead>
              <tr>
                <th>Network</th>
                <th>Signal</th>
                <th className="num">Ch</th>
                <th>PHY</th>
              </tr>
            </thead>
            <tbody>
              {point.samples.map((s) => (
                <tr key={s.bssid} className={s.isConnected ? "row-connected" : undefined}>
                  <td>
                    <div className="sample-ssid" title={s.ssid ?? "hidden"}>
                      {s.ssid ?? <span className="muted italic">hidden</span>}
                    </div>
                    <div className="sample-bssid">
                      {s.bssid}
                      {apNameByBssid?.get(s.bssid) && <span className="chip sample-ap">{apNameByBssid.get(s.bssid)}</span>}
                    </div>
                  </td>
                  <td>
                    <SignalCell signal={s.signal} compact />
                  </td>
                  <td className="num mono" title={`${BAND_LABEL[s.band]} · ${s.frequencyMhz} MHz`}>
                    {s.channel ?? "?"}
                    {s.channelWidthMhz != null && <span className="muted">/{s.channelWidthMhz}</span>}
                  </td>
                  <td className="mono">{phyShort(s.phyType)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}
