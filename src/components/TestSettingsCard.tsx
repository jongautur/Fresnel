import { useEffect, useState, type FormEvent } from "react";
import { api, asApiError } from "../api/tauri";
import type { ApiError } from "../types/wifi";
import type { Iperf3Directions, TestSettings } from "../types/pointTests";
import { TEST_SETTINGS_CHANGED, patchTestSettings } from "../lib/pointTests";
import { ErrorBanner } from "./ErrorBanner";
import { KeyValueGrid, KV } from "./KeyValue";
import { NumberInput } from "./NumberInput";

const DIRECTIONS: [Iperf3Directions, string][] = [
  ["upload", "Upload"],
  ["download", "Download"],
  ["both", "Both"],
];

/** The stored settings, re-read when another card saves. */
function useTestSettings() {
  const [settings, setSettings] = useState<TestSettings | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  useEffect(() => {
    const load = () =>
      api.getTestSettings().then(
        (s) => {
          setSettings(s);
          setError(null);
        },
        (e) => setError(asApiError(e)),
      );
    void load();
    window.addEventListener(TEST_SETTINGS_CHANGED, load);
    return () => window.removeEventListener(TEST_SETTINGS_CHANGED, load);
  }, []);
  return { settings, error };
}

/** A card's draft of some fields, saved on their own (see `patchTestSettings`). */
function useDraft<K extends keyof TestSettings>(settings: TestSettings | null, keys: readonly K[]) {
  const [draft, setDraft] = useState<Pick<TestSettings, K> | null>(null);
  const [dirty, setDirty] = useState(false);
  const [saved, setSaved] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  useEffect(() => {
    if (settings && !dirty) {
      const picked = {} as Pick<TestSettings, K>;
      for (const k of keys) picked[k] = settings[k];
      setDraft(picked);
    }
    // keys is a constant per card.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [settings, dirty]);
  const set = (patch: Partial<Pick<TestSettings, K>>) => {
    setDraft((d) => (d ? { ...d, ...patch } : d));
    setDirty(true);
    setSaved(false);
  };
  const save = async (e: FormEvent, normalise?: (d: Pick<TestSettings, K>) => Partial<TestSettings>) => {
    e.preventDefault();
    if (!draft) return;
    setBusy(true);
    try {
      await patchTestSettings(normalise ? normalise(draft) : draft);
      setError(null);
      setDirty(false);
      setSaved(true);
    } catch (err) {
      setError(asApiError(err));
    } finally {
      setBusy(false);
    }
  };
  return { draft, set, save, saved, busy, error };
}

function LoadError({ title, error }: { title: string; error: ApiError | null }) {
  return error ? (
    <section className="card">
      <header className="card-header"><h2>{title}</h2></header>
      <div className="pad"><ErrorBanner error={error} compact /></div>
    </section>
  ) : null;
}

const IPERF3_KEYS = ["iperf3Server", "iperf3Port", "iperf3Streams", "iperf3DurationS", "iperf3OmitS", "iperf3Directions"] as const;

/** The iperf3 server, set up once for the Tools page and for "+ run tests". */
export function Iperf3SettingsCard() {
  const { settings, error: loadError } = useTestSettings();
  const { draft, set, save, saved, busy, error } = useDraft(settings, IPERF3_KEYS);
  if (!draft) return <LoadError title="iperf3 server" error={loadError} />;
  return (
    <section className="card" id="iperf3-settings">
      <header className="card-header"><h2>iperf3 server</h2></header>
      <form onSubmit={(e) => void save(e, (d) => ({ ...d, iperf3Server: d.iperf3Server?.trim() || null }))}>
        <KeyValueGrid>
          <KV k="Server">
            <input
              className="input"
              placeholder="e.g. 192.168.1.10"
              value={draft.iperf3Server ?? ""}
              onChange={(e) => set({ iperf3Server: e.target.value })}
            />{" "}
            port <NumberInput value={draft.iperf3Port} min={1} max={65535} onCommit={(iperf3Port) => set({ iperf3Port })} />
            <div className="field-hint">
              Your own <span className="mono">iperf3 -s</span> (iperf3 3.x), as an IP address. Fresnel never contacts a
              server you didn't enter. The server needs its port open inbound; this computer needs no firewall rule. A
              server runs one test at a time.
            </div>
          </KV>
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
            <div className="field-hint">
              Upload: this computer sends. Download: the server sends (iperf3 -R). Both: one after the other. The Tools
              page starts with this and lets you pick per run.
            </div>
          </KV>
          <KV k="Streams">
            <NumberInput value={draft.iperf3Streams} min={1} max={16} onCommit={(iperf3Streams) => set({ iperf3Streams })} />
          </KV>
          <KV k="Duration">
            <NumberInput
              value={draft.iperf3DurationS}
              min={1}
              max={120}
              onCommit={(iperf3DurationS) => set({ iperf3DurationS })}
            />{" "}
            s measured, after{" "}
            <NumberInput value={draft.iperf3OmitS} min={0} max={10} onCommit={(iperf3OmitS) => set({ iperf3OmitS })} /> s
            omitted
            <div className="field-hint">
              The omitted seconds (TCP slow start) aren't counted. The average is the receiver's: the server's count for
              upload, this computer's for download.
            </div>
          </KV>
        </KeyValueGrid>
        {error && <div className="pad"><ErrorBanner error={error} compact /></div>}
        <div className="pad panel-actions">
          <button type="submit" className="btn btn-primary" disabled={busy}>
            Save
          </button>
          <span className="muted small">{saved ? "Saved." : "Used by Tools › iperf3 and by “+ run tests”."}</span>
        </div>
      </form>
    </section>
  );
}

const ACTIVE_KEYS = ["pingCount", "tcpPort", "extraHost", "iperf3InPointTests"] as const;

/** What "+ run tests" runs after Measure Here. */
export function TestSettingsCard() {
  const { settings, error: loadError } = useTestSettings();
  const { draft, set, save, saved, busy, error } = useDraft(settings, ACTIVE_KEYS);
  if (!draft || !settings) return <LoadError title="Active tests" error={loadError} />;
  const server = settings.iperf3Server;
  return (
    <section className="card">
      <header className="card-header"><h2>Active tests</h2></header>
      <form onSubmit={(e) => void save(e, (d) => ({ ...d, extraHost: d.extraHost?.trim() || null }))}>
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
              value={draft.extraHost ?? ""}
              onChange={(e) => set({ extraHost: e.target.value })}
            />
            <div className="field-hint">
              An IP address (host names aren't looked up). Windows computers don't answer ping unless their firewall
              allows it.
            </div>
          </KV>
          <KV k="iperf3">
            <label className="checkbox">
              <input
                type="checkbox"
                checked={draft.iperf3InPointTests}
                disabled={!server}
                onChange={(e) => set({ iperf3InPointTests: e.target.checked })}
              />
              Also measure throughput
            </label>
            <div className="field-hint">
              {server ? (
                <>
                  Against <span className="mono">{server}:{settings.iperf3Port}</span>, as set up under “iperf3 server”.
                </>
              ) : (
                "Set up an iperf3 server above first."
              )}
            </div>
          </KV>
        </KeyValueGrid>
        {error && <div className="pad"><ErrorBanner error={error} compact /></div>}
        <div className="pad panel-actions">
          <button type="submit" className="btn btn-primary" disabled={busy}>
            Save
          </button>
          <span className="muted small">{saved ? "Saved." : "Used by “+ run tests” under Measure here."}</span>
        </div>
      </form>
    </section>
  );
}
