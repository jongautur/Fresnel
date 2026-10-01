import type { Adapter, Capability } from "../types/wifi";
import { bandsSummary, busSummary } from "../lib/format";
import { KeyValueGrid, KV } from "./KeyValue";
import { StatusDot } from "./StatusDot";

function Cap({ label, cap }: { label: string; cap: Capability }) {
  const title = { supported: "Supported", unsupported: "Not supported", unknown: "Cannot be detected by this provider" }[cap];
  return (
    <span className={`cap cap-${cap}`} title={title}>
      {cap === "supported" ? "✓" : cap === "unsupported" ? "✕" : "?"} {label}
    </span>
  );
}

export function AdapterInfo({ adapter }: { adapter: Adapter | null }) {
  return (
    <section className="card">
      <header className="card-header">
        <h2>Adapter</h2>
        {adapter && <StatusDot status={adapter.status} />}
      </header>
      {!adapter ? (
        <p className="muted pad">No adapter selected.</p>
      ) : (
        <>
          <KeyValueGrid>
            <KV k="Name">{adapter.displayName}</KV>
            <KV k="Interface" mono>{adapter.interfaceName}</KV>
            <KV k="ID" mono>{adapter.id}</KV>
            <KV k="Driver" mono>{adapter.driver}</KV>
            <KV k="MAC" mono>
              {adapter.hwAddress}
              {adapter.permanentHwAddress && (
                <span className="muted"> (perm. {adapter.permanentHwAddress})</span>
              )}
            </KV>
            <KV k="Bus" mono>{busSummary(adapter)}</KV>
            <KV k="Bands">{bandsSummary(adapter)}</KV>
            <KV k="Provider" mono>{adapter.provider}</KV>
            {adapter.statusDetail && <KV k="Status">{adapter.statusDetail}</KV>}
          </KeyValueGrid>
          <div className="caps">
            <Cap label="2.4 GHz" cap={adapter.capabilities.band2ghz} />
            <Cap label="5 GHz" cap={adapter.capabilities.band5ghz} />
            <Cap label="6 GHz" cap={adapter.capabilities.band6ghz} />
            <Cap label="Active scan" cap={adapter.capabilities.activeScan} />
            <Cap label="Monitor" cap={adapter.capabilities.monitorMode} />
            <Cap label="Capture" cap={adapter.capabilities.packetCapture} />
            <Cap label="dBm" cap={adapter.capabilities.signalDbm} />
          </div>
        </>
      )}
    </section>
  );
}
