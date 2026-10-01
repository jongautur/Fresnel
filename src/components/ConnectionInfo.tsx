import type { ApiError, ConnectionInfo as Conn } from "../types/wifi";
import { BAND_LABEL, SECURITY_LABEL, formatBitrate, formatLinkRate, formatMhz } from "../lib/format";
import { KeyValueGrid, KV } from "./KeyValue";
import { SignalCell } from "./SignalCell";
import { ErrorBanner } from "./ErrorBanner";

const LINK_AVG_HINT =
  "Link average: the driver's running average over received data frames. " +
  "The scan table shows one beacon reading per scan, so the two values can differ by several dB.";

export function ConnectionInfo({ connection, error }: { connection: Conn | null; error: ApiError | null }) {
  return (
    <section className="card">
      <header className="card-header">
        <h2>Current connection</h2>
        {connection && <SignalCell signal={connection.signal} description={LINK_AVG_HINT} />}
      </header>
      {error ? (
        <div className="pad"><ErrorBanner error={error} compact /></div>
      ) : !connection ? (
        <p className="muted pad">Not associated with any network.</p>
      ) : (
        <KeyValueGrid>
          <KV k="SSID">{connection.ssid ?? <span className="muted">hidden</span>}</KV>
          <KV k="BSSID" mono>{connection.bssid}</KV>
          <KV k="Interface" mono>{connection.interfaceName}</KV>
          <KV k="Channel" mono>
            {connection.channel}
            {connection.channelWidthMhz != null && <span className="muted"> @ {connection.channelWidthMhz} MHz</span>}
          </KV>
          <KV k="Frequency" mono>{formatMhz(connection.frequencyMhz)}</KV>
          <KV k="Band">{connection.band ? BAND_LABEL[connection.band] : null}</KV>
          <KV k="Link signal" mono hint={LINK_AVG_HINT}>
            {connection.signal.dbm != null && (
              <>
                {connection.signal.dbm.toFixed(0)} dBm <span className="muted">avg</span>
              </>
            )}
            {connection.signal.dbm != null && connection.signal.qualityPercent != null && " · "}
            {connection.signal.qualityPercent != null && (
              <span className="muted">{connection.signal.qualityPercent} % (NM)</span>
            )}
          </KV>
          {connection.txRate || connection.rxRate ? (
            <>
              <KV k="TX rate" mono>{formatLinkRate(connection.txRate)}</KV>
              <KV k="RX rate" mono>{formatLinkRate(connection.rxRate)}</KV>
            </>
          ) : (
            <KV k="Link rate" mono>{formatBitrate(connection.bitrateKbps)}</KV>
          )}
          <KV k="Security">{connection.security ? SECURITY_LABEL[connection.security.kind] : null}</KV>
          <KV k="IPv4" mono>{connection.ipv4Addresses.join(", ")}</KV>
          <KV k="Gateway" mono>{connection.ipv4Gateway}</KV>
        </KeyValueGrid>
      )}
    </section>
  );
}
