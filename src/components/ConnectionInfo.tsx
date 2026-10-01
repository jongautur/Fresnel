import type { ApiError, ConnectionInfo as Conn } from "../types/wifi";
import { BAND_LABEL, SECURITY_LABEL, formatBitrate, formatMhz } from "../lib/format";
import { KeyValueGrid, KV } from "./KeyValue";
import { SignalCell } from "./SignalCell";
import { ErrorBanner } from "./ErrorBanner";

export function ConnectionInfo({ connection, error }: { connection: Conn | null; error: ApiError | null }) {
  return (
    <section className="card">
      <header className="card-header">
        <h2>Current connection</h2>
        {connection && <SignalCell signal={connection.signal} />}
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
          <KV k="Link rate" mono>{formatBitrate(connection.bitrateKbps)}</KV>
          <KV k="Security">{connection.security ? SECURITY_LABEL[connection.security.kind] : null}</KV>
          <KV k="IPv4" mono>{connection.ipv4Addresses.join(", ")}</KV>
          <KV k="Gateway" mono>{connection.ipv4Gateway}</KV>
        </KeyValueGrid>
      )}
    </section>
  );
}
