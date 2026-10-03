import { useCallback, useEffect, useState, type FormEvent } from "react";
import { api } from "../../api/tauri";
import type { DnsLookup, DnsParams, DnsRunResults, DnsTransport, SystemDnsServer, ToolRun, Via } from "../../types/tools";
import { NumberInput } from "../NumberInput";
import { fmtMs, loadForm, saveForm } from "../../lib/tools";
import { ViaPicker } from "./ViaPicker";
import { RunInfo } from "./RunInfo";
import { ToolHistory } from "./ToolHistory";
import { useToolRun } from "./useToolRun";

const RECORD_TYPES = ["A", "AAAA", "CNAME", "MX", "TXT", "NS", "SOA", "PTR", "SRV", "CAA"];

interface Form {
  name: string;
  recordType: string;
  useSystem: boolean;
  /** Other servers, comma separated. */
  others: string;
  transport: DnsTransport;
  timeoutMs: number;
  via: Via;
}

const DEFAULTS: Form = {
  name: "",
  recordType: "A",
  useSystem: true,
  others: "",
  transport: "udp",
  timeoutMs: 3000,
  via: { mode: "system" },
};

const looksLikeIp = (s: string) => /^[0-9.]+$/.test(s.trim()) || s.includes(":");

export function DnsTool() {
  const [form, setFormState] = useState<Form>(() => loadForm("dns", DEFAULTS));
  const setForm = (patch: Partial<Form>) =>
    setFormState((f) => {
      const next = { ...f, ...patch };
      saveForm("dns", next);
      return next;
    });
  const [system, setSystem] = useState<SystemDnsServer[] | null>(null);
  const [refresh, setRefresh] = useState(0);
  const onStored = useCallback(() => setRefresh((n) => n + 1), []);
  const { running, stopping, error, run, started, start, stop, show } = useToolRun("dns", onStored);
  const results = run?.results as DnsRunResults | null | undefined;

  useEffect(() => {
    api.dnsSystemServers().then(setSystem, () => setSystem([]));
  }, []);

  const servers = [
    ...(form.useSystem ? ["system"] : []),
    ...form.others
      .split(/[,\s]+/)
      .map((s) => s.trim())
      .filter(Boolean),
  ];

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (running) return;
    const params: DnsParams = {
      name: form.name.trim(),
      recordType: form.recordType,
      servers,
      transport: form.transport,
      timeoutMs: form.timeoutMs,
      via: form.via,
    };
    void start((id, onEvent) => api.toolDns(id, params, onEvent), () => {});
  };

  const again = (r: ToolRun) => {
    const p = r.params as Partial<DnsParams>;
    const list = p.servers ?? [];
    setForm({
      name: p.name ?? form.name,
      recordType: p.recordType ?? form.recordType,
      useSystem: list.includes("system"),
      others: list.filter((s) => s !== "system").join(", "),
      transport: p.transport ?? form.transport,
      timeoutMs: p.timeoutMs ?? form.timeoutMs,
      via: p.via ?? form.via,
    });
  };

  return (
    <>
      <section className="card">
        <form className="tool-form" onSubmit={submit}>
          <label className="tool-field tool-target">
            <span className="field-label">Name</span>
            <input
              className="input"
              placeholder={form.recordType === "PTR" ? "an IP address, e.g. 192.168.1.10" : "e.g. example.com"}
              value={form.name}
              disabled={running}
              onChange={(e) => {
                const name = e.target.value;
                // An address asks for its name: switch to PTR once, the first time it looks like one.
                setForm(looksLikeIp(name) && !looksLikeIp(form.name) && form.recordType === "A" ? { name, recordType: "PTR" } : { name });
              }}
            />
          </label>
          <label className="tool-field">
            <span className="field-label">Type</span>
            <select className="input" value={form.recordType} disabled={running} onChange={(e) => setForm({ recordType: e.target.value })}>
              {RECORD_TYPES.map((t) => (
                <option key={t}>{t}</option>
              ))}
            </select>
          </label>
          <ViaPicker value={form.via} onChange={(via) => setForm({ via })} disabled={running} />
          {running ? (
            <button type="button" className="btn btn-danger" onClick={stop} disabled={stopping}>
              Stop
            </button>
          ) : (
            <button type="submit" className="btn btn-primary" disabled={!form.name.trim() || servers.length === 0 || servers.length > 5}>
              Look up
            </button>
          )}
          <div className="tool-options">
            <label className="checkbox" title={system?.map((s) => `${s.address} (${s.source})`).join("\n")}>
              <input type="checkbox" checked={form.useSystem} disabled={running} onChange={(e) => setForm({ useSystem: e.target.checked })} />
              System DNS
              {system && (
                <span className="muted mono small">
                  {system[0] ? ` ${system[0].address}` : " (none configured)"}
                </span>
              )}
            </label>
            <span className="muted">{form.useSystem ? "and" : "servers"}</span>
            <input
              className="input"
              placeholder="e.g. 1.1.1.1, 9.9.9.9 (up to 5 to compare)"
              value={form.others}
              disabled={running}
              onChange={(e) => setForm({ others: e.target.value })}
            />
            <div className="segmented">
              {(["udp", "tcp"] as const).map((t) => (
                <button key={t} type="button" className={form.transport === t ? "active" : ""} disabled={running} onClick={() => setForm({ transport: t })}>
                  {t.toUpperCase()}
                </button>
              ))}
            </div>
            <span className="muted">timeout</span>
            <NumberInput value={form.timeoutMs} min={200} max={30000} onCommit={(timeoutMs) => setForm({ timeoutMs })} />
            <span className="muted">ms</span>
          </div>
          {servers.length > 5 && <div className="field-hint status-bad">Compare at most 5 servers.</div>}
        </form>
        <div className="pad">
          <RunInfo started={started} run={run} error={error} running={running} />
        </div>
      </section>

      {!running && results && (
        <div className={results.lookups.length > 1 ? "dns-compare" : undefined}>
          {results.lookups.map((l) => (
            <LookupCard key={l.server} lookup={l} fastest={fastest(results.lookups) === l.server && results.lookups.length > 1} />
          ))}
        </div>
      )}

      <ToolHistory kind="dns" refreshKey={refresh} selectedId={run?.id ?? null} onOpen={show} onAgain={again} />
    </>
  );
}

function fastest(lookups: DnsLookup[]): string | null {
  let best: DnsLookup | null = null;
  for (const l of lookups) if (l.result && (!best?.result || l.result.queryMs < best.result.queryMs)) best = l;
  return best?.server ?? null;
}

function LookupCard({ lookup, fastest }: { lookup: DnsLookup; fastest: boolean }) {
  const r = lookup.result;
  return (
    <section className="card">
      <header className="card-header">
        <h2 className="mono dns-server">{lookup.server}</h2>
        <span className="muted small">{lookup.source}</span>
      </header>
      {!r ? (
        <div className="pad">
          <div className="status status-bad">{lookup.error ?? "stopped"}</div>
          {lookup.errorHint && <div className="muted small">{lookup.errorHint}</div>}
        </div>
      ) : (
        <>
          <div className="pad dns-meta">
            <span className={`status ${r.responseCode === "NoError" ? "status-ok" : "status-bad"}`}>{r.responseCode}</span>
            <span className="mono">{fmtMs(r.queryMs)}</span>
            {fastest && <span className="chip">fastest</span>}
            <span className="muted small">
              {r.transport.toUpperCase()}
              {r.retriedOverTcp ? " (UDP answer was truncated)" : ""} · {r.responseBytes} bytes
            </span>
            <span className="muted small" title="AA: the server is authoritative. RA: it resolves recursively. AD: it says it validated the answer with DNSSEC (only as trustworthy as the path to it).">
              {[r.authoritative && "AA", r.recursionAvailable && "RA", r.authenticData && "AD", r.truncated && "TC"].filter(Boolean).join(" ") || "no flags"}
            </span>
          </div>
          {r.records.length === 0 ? (
            <p className="pad muted">
              {r.responseCode === "NXDomain" ? "The name doesn't exist." : `No ${r.recordType} records.`}
            </p>
          ) : (
            <div className="table-scroll">
              <table className="data-table">
                <thead>
                  <tr>
                    <th>Section</th>
                    <th>Name</th>
                    <th>Type</th>
                    <th className="num">TTL</th>
                    <th>Data</th>
                  </tr>
                </thead>
                <tbody>
                  {r.records.map((rec, i) => (
                    <tr key={i} className={rec.section === "answer" ? undefined : "row-stale"}>
                      <td className="muted small">{rec.section}</td>
                      <td className="mono">{rec.name}</td>
                      <td className="mono">{rec.recordType}</td>
                      <td className="num mono">{rec.ttl}</td>
                      <td className="mono wrap">{rec.data}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </>
      )}
    </section>
  );
}
