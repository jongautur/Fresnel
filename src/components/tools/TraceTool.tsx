import { useCallback, useMemo, useState, type FormEvent } from "react";
import { api } from "../../api/tauri";
import type { HopProbe, HopStats, IpFamily, ToolRun, TraceMethod, TraceParams, TraceResult, Via } from "../../types/tools";
import { NumberInput } from "../NumberInput";
import { fmtMs, loadForm, saveForm } from "../../lib/tools";
import { FamilyPicker, ViaPicker } from "./ViaPicker";
import { RunInfo } from "./RunInfo";
import { ToolHistory } from "./ToolHistory";
import { useToolRun } from "./useToolRun";

interface Form {
  target: string;
  family: IpFamily;
  via: Via;
  mtr: boolean;
  rounds: number;
  maxHops: number;
  timeoutMs: number;
  intervalMs: number;
  method: TraceMethod;
  names: boolean;
}

const DEFAULTS: Form = {
  target: "",
  family: "any",
  via: { mode: "system" },
  mtr: false,
  rounds: 3,
  maxHops: 30,
  timeoutMs: 2000,
  intervalMs: 1000,
  method: "icmp",
  names: true,
};

/** UDP traceroute needs raw sockets (administrator) on Windows; Fresnel offers it on Linux only. */
const UDP_AVAILABLE = !/Windows/i.test(navigator.userAgent);

/** Per-hop totals while the run is live (the stored result is computed the same way). */
interface LiveHop {
  sent: number;
  received: number;
  addresses: Map<string, number>;
  last: number | null;
  best: number | null;
  worst: number | null;
  sum: number;
  sumSq: number;
  rtts: number;
  unreachable: string | null;
}

function addProbe(hops: Map<number, LiveHop>, p: HopProbe): Map<number, LiveHop> {
  const next = new Map(hops);
  const h: LiveHop = { ...(hops.get(p.ttl) ?? newHop()) };
  h.addresses = new Map(h.addresses);
  h.sent += 1;
  if (p.from) {
    h.received += 1;
    h.addresses.set(p.from, (h.addresses.get(p.from) ?? 0) + 1);
  }
  if (p.rttMs != null) {
    h.last = p.rttMs;
    h.best = h.best == null ? p.rttMs : Math.min(h.best, p.rttMs);
    h.worst = h.worst == null ? p.rttMs : Math.max(h.worst, p.rttMs);
    h.sum += p.rttMs;
    h.sumSq += p.rttMs * p.rttMs;
    h.rtts += 1;
  }
  if (p.reply?.kind === "unreachable") h.unreachable = p.reply.detail;
  next.set(p.ttl, h);
  return next;
}

function newHop(): LiveHop {
  return { sent: 0, received: 0, addresses: new Map(), last: null, best: null, worst: null, sum: 0, sumSq: 0, rtts: 0, unreachable: null };
}

function toStats(ttl: number, h: LiveHop, names: Map<string, string>): HopStats {
  const avg = h.rtts ? h.sum / h.rtts : null;
  return {
    ttl,
    addresses: [...h.addresses.entries()]
      .sort((a, b) => b[1] - a[1])
      .map(([address, replies]) => ({ address, replies, name: names.get(address) ?? null })),
    sent: h.sent,
    received: h.received,
    lossPercent: h.sent ? (100 * (h.sent - h.received)) / h.sent : 0,
    lastMs: h.last,
    bestMs: h.best,
    avgMs: avg,
    worstMs: h.worst,
    stdevMs: avg != null && h.rtts > 1 ? Math.sqrt(Math.max(0, h.sumSq / h.rtts - avg * avg)) : null,
    unreachable: h.unreachable,
  };
}

export function TraceTool() {
  const [form, setFormState] = useState<Form>(() => {
    const f = loadForm("traceroute", DEFAULTS);
    return UDP_AVAILABLE ? f : { ...f, method: "icmp" };
  });
  const setForm = (patch: Partial<Form>) =>
    setFormState((f) => {
      const next = { ...f, ...patch };
      saveForm("traceroute", next);
      return next;
    });
  const [hops, setHops] = useState<Map<number, LiveHop>>(new Map());
  const [names, setNames] = useState<Map<string, string>>(new Map());
  const [rounds, setRounds] = useState(0);
  const [refresh, setRefresh] = useState(0);
  const onStored = useCallback(() => setRefresh((n) => n + 1), []);
  const { running, stopping, error, run, started, start, stop, show } = useToolRun("traceroute", onStored);
  const stored = run?.results as TraceResult | null | undefined;

  const liveDest = useMemo(() => {
    let d: number | null = null;
    // The destination answers every probe that reaches it; the nearest one is its distance.
    for (const [ttl, h] of hops) {
      const target = started?.target?.ip;
      if (target && h.addresses.has(target)) d = d == null ? ttl : Math.min(d, ttl);
    }
    return d;
  }, [hops, started]);

  const view: { hops: HopStats[]; dest: number | null; rounds: number } = useMemo(() => {
    if (!running && stored) return { hops: stored.hops, dest: stored.destinationTtl, rounds: stored.rounds };
    const last = liveDest ?? Math.max(0, ...hops.keys());
    const list: HopStats[] = [];
    for (let ttl = 1; ttl <= last; ttl++) list.push(toStats(ttl, hops.get(ttl) ?? newHop(), names));
    return { hops: list, dest: liveDest, rounds };
  }, [running, stored, hops, names, liveDest, rounds]);

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (running) return;
    const params: TraceParams = {
      target: form.target.trim(),
      family: form.family,
      via: form.via,
      maxHops: form.maxHops,
      timeoutMs: form.timeoutMs,
      rounds: form.mtr ? null : form.rounds,
      intervalMs: form.mtr ? form.intervalMs : 0,
      method: UDP_AVAILABLE ? form.method : "icmp",
      names: form.names,
    };
    setHops(new Map());
    setNames(new Map());
    setRounds(0);
    void start(
      (id, onEvent) => api.toolTraceroute(id, params, onEvent),
      (ev) => {
        if (ev.event !== "trace") return;
        if (ev.type === "probe") {
          setHops((h) => addProbe(h, ev));
          setRounds((r) => Math.max(r, ev.round + 1));
        } else {
          setNames((n) => new Map(n).set(ev.address, ev.name));
        }
      },
    );
  };

  const again = (r: ToolRun) => {
    const p = r.params as Partial<TraceParams>;
    setForm({
      target: p.target ?? form.target,
      family: p.family ?? form.family,
      via: p.via ?? form.via,
      mtr: p.rounds === null,
      rounds: p.rounds ?? form.rounds,
      maxHops: p.maxHops ?? form.maxHops,
      timeoutMs: p.timeoutMs ?? form.timeoutMs,
      intervalMs: p.intervalMs || form.intervalMs,
      method: p.method ?? form.method,
      names: p.names ?? form.names,
    });
  };

  // Hops after the last one that answered, up to the limit: one row.
  const lastAnswering = view.hops.reduce((m, h) => (h.received > 0 ? h.ttl : m), 0);
  const rows = view.dest != null ? view.hops : view.hops.filter((h) => h.ttl <= lastAnswering);
  const silentTail = view.dest == null && view.hops.length > lastAnswering;

  return (
    <>
      <section className="card">
        <form className="tool-form" onSubmit={submit}>
          <label className="tool-field tool-target">
            <span className="field-label">Host</span>
            <input
              className="input"
              placeholder="e.g. 8.8.8.8 or example.com"
              value={form.target}
              disabled={running}
              onChange={(e) => setForm({ target: e.target.value })}
            />
          </label>
          <ViaPicker value={form.via} onChange={(via) => setForm({ via })} disabled={running} />
          <FamilyPicker value={form.family} onChange={(family) => setForm({ family })} disabled={running} />
          {running ? (
            <button type="button" className="btn btn-danger" onClick={stop} disabled={stopping}>
              {stopping ? "Stopping…" : "Stop"}
            </button>
          ) : (
            <button type="submit" className="btn btn-primary" disabled={!form.target.trim()}>
              Trace
            </button>
          )}
          <div className="tool-options">
            <div className="segmented">
              <button type="button" className={!form.mtr ? "active" : ""} disabled={running} onClick={() => setForm({ mtr: false })}>
                Rounds
              </button>
              <button type="button" className={form.mtr ? "active" : ""} disabled={running} onClick={() => setForm({ mtr: true })} title="Keep probing every hop until stopped, like mtr">
                Continuous (MTR)
              </button>
            </div>
            {form.mtr ? (
              <>
                <span className="muted">every</span>
                <NumberInput value={form.intervalMs} min={200} max={60000} onCommit={(intervalMs) => setForm({ intervalMs })} />
                <span className="muted">ms</span>
              </>
            ) : (
              <NumberInput value={form.rounds} min={1} max={100} onCommit={(rounds) => setForm({ rounds })} />
            )}
            <span className="sep">·</span>
            <span className="muted">max hops</span>
            <NumberInput value={form.maxHops} min={1} max={64} onCommit={(maxHops) => setForm({ maxHops })} />
            <span className="muted">wait</span>
            <NumberInput value={form.timeoutMs} min={100} max={10000} onCommit={(timeoutMs) => setForm({ timeoutMs })} />
            <span className="muted">ms</span>
            <span className="sep">·</span>
            <div className="segmented" title="UDP probes go to ports 33434+; some networks pass one and drop the other (UDP needs Linux)">
              <button type="button" className={form.method === "icmp" ? "active" : ""} disabled={running} onClick={() => setForm({ method: "icmp" })}>
                ICMP
              </button>
              <button
                type="button"
                className={form.method === "udp" ? "active" : ""}
                disabled={running || !UDP_AVAILABLE}
                title={UDP_AVAILABLE ? undefined : "UDP traceroute needs administrator rights on Windows; ICMP works without"}
                onClick={() => setForm({ method: "udp" })}
              >
                UDP
              </button>
            </div>
            <label className="checkbox" title="Reverse DNS (PTR) for each hop, from this computer's DNS server">
              <input type="checkbox" checked={form.names} disabled={running} onChange={(e) => setForm({ names: e.target.checked })} />
              Names
            </label>
          </div>
        </form>
        <div className="pad">
          <RunInfo started={started} run={run} error={error} running={running} />
        </div>
      </section>

      {(running || stored) && (
        <section className="card">
          <header className="card-header">
            <h2>Route</h2>
            <span className="muted small">
              {view.dest != null ? `destination at hop ${view.dest}` : running ? "searching…" : "destination didn't answer"} ·{" "}
              {view.rounds} round{view.rounds === 1 ? "" : "s"}
              {stored?.method === "udp" && !running ? " · UDP" : ""}
              {stored?.resolutionMs ? ` · ${stored.resolutionMs} ms clock` : ""}
            </span>
          </header>
          {!running && stored?.fallbackReason && (
            <p className="pad small">
              <span className="status status-warn">UDP</span> ICMP isn't allowed here, so UDP probes were used:{" "}
              <span className="muted">{stored.fallbackReason}</span>
            </p>
          )}
          <div className="table-scroll">
            <table className="data-table trace-table">
              <thead>
                <tr>
                  <th className="num">Hop</th>
                  <th>Host</th>
                  <th className="num">Loss</th>
                  <th className="num">Sent</th>
                  <th className="num">Last</th>
                  <th className="num">Avg</th>
                  <th className="num">Best</th>
                  <th className="num">Worst</th>
                  <th className="num">StDev</th>
                </tr>
              </thead>
              <tbody>
                {rows.map((h) => (
                  <HopRow key={h.ttl} hop={h} />
                ))}
                {silentTail && (
                  <tr>
                    <td className="num mono">
                      {lastAnswering + 1}–{view.hops[view.hops.length - 1]?.ttl}
                    </td>
                    <td colSpan={8} className="muted">
                      no answer
                    </td>
                  </tr>
                )}
                {rows.length === 0 && !silentTail && (
                  <tr>
                    <td colSpan={9} className="empty">
                      {running ? "Waiting for the first answers…" : "No hop answered."}
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
          <p className="pad muted small">
            Routers often answer traceroute slowly or not at all while forwarding traffic normally: loss at one hop that
            doesn't continue to later hops isn't loss on the path.
          </p>
        </section>
      )}

      <ToolHistory kind="traceroute" refreshKey={refresh} selectedId={run?.id ?? null} onOpen={show} onAgain={again} />
    </>
  );
}

function HopRow({ hop }: { hop: HopStats }) {
  const main = hop.addresses[0];
  const others = hop.addresses.length - 1;
  return (
    <tr>
      <td className="num mono">{hop.ttl}</td>
      <td className="wrap">
        {main ? (
          <>
            <span className="mono">{main.name ?? main.address}</span>
            {main.name && <span className="muted mono small"> {main.address}</span>}
            {others > 0 && (
              <span
                className="chip"
                title={hop.addresses
                  .slice(1)
                  .map((a) => `${a.name ?? a.address} (${a.replies})`)
                  .join("\n")}
              >
                +{others}
              </span>
            )}
          </>
        ) : (
          <span className="muted">no answer</span>
        )}
        {hop.unreachable && <div className="small status-bad">{hop.unreachable}</div>}
      </td>
      <td className={`num mono ${hop.lossPercent > 0 ? "status-bad" : ""}`}>{hop.lossPercent.toFixed(0)} %</td>
      <td className="num mono">{hop.sent}</td>
      <td className="num mono">{fmtMs(hop.lastMs)}</td>
      <td className="num mono">{fmtMs(hop.avgMs)}</td>
      <td className="num mono">{fmtMs(hop.bestMs)}</td>
      <td className="num mono">{fmtMs(hop.worstMs)}</td>
      <td className="num mono">{fmtMs(hop.stdevMs)}</td>
    </tr>
  );
}
