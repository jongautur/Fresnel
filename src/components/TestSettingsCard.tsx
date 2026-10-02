import { useEffect, useState, type FormEvent } from "react";
import { api, asApiError } from "../api/tauri";
import type { ApiError } from "../types/wifi";
import type { Iperf3Directions, TestSettings } from "../types/pointTests";
import { ErrorBanner } from "./ErrorBanner";
import { KeyValueGrid, KV } from "./KeyValue";
import { NumberInput } from "./NumberInput";

const DIRECTIONS: [Iperf3Directions, string][] = [
  ["upload", "Upload"],
  ["download", "Download"],
  ["both", "Both"],
];

/** What "+ run tests" runs after Measure Here. Stored by the backend. */
export function TestSettingsCard() {
  const [draft, setDraft] = useState<TestSettings | null>(null);
  const [extraHost, setExtraHost] = useState("");
  const [server, setServer] = useState("");
  const [error, setError] = useState<ApiError | null>(null);
  const [saved, setSaved] = useState(false);
  const [busy, setBusy] = useState(false);

  const load = (s: TestSettings) => {
    setDraft(s);
    setExtraHost(s.extraHost ?? "");
    setServer(s.iperf3Server ?? "");
  };

  useEffect(() => {
    api.getTestSettings().then(load, (e) => setError(asApiError(e)));
  }, []);

  if (!draft) {
    return error ? (
      <section className="card">
        <header className="card-header"><h2>Active tests</h2></header>
        <div className="pad"><ErrorBanner error={error} compact /></div>
      </section>
    ) : null;
  }

  const set = (patch: Partial<TestSettings>) => {
    setDraft({ ...draft, ...patch });
    setSaved(false);
  };

  const save = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    try {
      load(await api.saveTestSettings({ ...draft, extraHost: extraHost || null, iperf3Server: server || null }));
      setError(null);
      setSaved(true);
    } catch (err) {
      setError(asApiError(err));
    } finally {
      setBusy(false);
    }
  };

  const iperf = server.trim() !== "";
  return (
    <section className="card">
      <header className="card-header"><h2>Active tests</h2></header>
      <form onSubmit={save}>
        <KeyValueGrid>
          <KV k="Gateway">
            Always pinged.
            <div className="field-hint">
              Each test runs over the Wi-Fi adapter only. If the route to a target would use Ethernet or a VPN, the
              test is refused and says so.
            </div>
          </KV>
          <KV k="Pings per target">
            <NumberInput value={draft.pingCount} min={1} max={100} onCommit={(pingCount) => set({ pingCount })} />
            <div className="field-hint">
              ICMP echo, without admin rights. Where the system doesn't allow that (Linux{" "}
              <span className="mono">net.ipv4.ping_group_range</span>), connects to TCP port{" "}
              <NumberInput value={draft.tcpPort} min={1} max={65535} onCommit={(tcpPort) => set({ tcpPort })} /> are
              timed instead and labelled “TCP connect”.
            </div>
          </KV>
          <KV k="Extra host">
            <input
              className="input"
              placeholder="optional, e.g. 192.168.1.20"
              value={extraHost}
              onChange={(e) => {
                setExtraHost(e.target.value);
                setSaved(false);
              }}
            />
            <div className="field-hint">
              An IP address (host names aren't looked up). Windows computers don't answer ping unless their firewall
              allows it.
            </div>
          </KV>
          <KV k="iperf3 server">
            <input
              className="input"
              placeholder="optional, e.g. 192.168.1.10"
              value={server}
              onChange={(e) => {
                setServer(e.target.value);
                setSaved(false);
              }}
            />{" "}
            port{" "}
            <NumberInput value={draft.iperf3Port} min={1} max={65535} onCommit={(iperf3Port) => set({ iperf3Port })} />
            <div className="field-hint">
              TCP throughput against your own <span className="mono">iperf3 -s</span> (iperf3 3.x). Fresnel never
              contacts a server you didn't enter. The server needs its port open inbound; this computer needs no
              firewall rule. A server runs one test at a time.
            </div>
          </KV>
          {iperf && (
            <>
              <KV k="Direction">
                <div className="segmented">
                  {DIRECTIONS.map(([d, label]) => (
                    <button
                      key={d}
                      type="button"
                      className={draft.iperf3Directions === d ? "active" : ""}
                      onClick={() => set({ iperf3Directions: d })}
                    >
                      {label}
                    </button>
                  ))}
                </div>
                <div className="field-hint">Upload: this computer sends. Download: the server sends (iperf3 -R).</div>
              </KV>
              <KV k="Streams">
                <NumberInput
                  value={draft.iperf3Streams}
                  min={1}
                  max={16}
                  onCommit={(iperf3Streams) => set({ iperf3Streams })}
                />
              </KV>
              <KV k="Duration">
                <NumberInput
                  value={draft.iperf3DurationS}
                  min={1}
                  max={120}
                  onCommit={(iperf3DurationS) => set({ iperf3DurationS })}
                />{" "}
                s measured, after{" "}
                <NumberInput value={draft.iperf3OmitS} min={0} max={10} onCommit={(iperf3OmitS) => set({ iperf3OmitS })} />{" "}
                s omitted
                <div className="field-hint">
                  The omitted seconds (TCP slow start) aren't counted. The average is the receiver's: the server's
                  count for upload, this computer's for download.
                </div>
              </KV>
            </>
          )}
        </KeyValueGrid>
        {error && <div className="pad"><ErrorBanner error={error} compact /></div>}
        <div className="pad panel-actions">
          <button type="submit" className="btn btn-primary" disabled={busy}>
            Save
          </button>
          <span className="muted small">
            {saved ? "Saved." : "Used by “+ run tests” under Measure here."}
          </span>
        </div>
      </form>
    </section>
  );
}
