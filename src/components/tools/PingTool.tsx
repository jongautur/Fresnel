import { useCallback, useMemo, useState, type FormEvent } from "react";
import { api } from "../../api/tauri";
import type { ProbeOutcome } from "../../types/pointTests";
import type { IpFamily, PingParams, PingRunResult, ToolRun, Via } from "../../types/tools";
import { NumberInput } from "../NumberInput";
import { fmtMs, loadForm, saveForm } from "../../lib/tools";
import { FamilyPicker, ViaPicker } from "./ViaPicker";
import { RunInfo } from "./RunInfo";
import { SeriesChart } from "./SeriesChart";
import { ToolHistory } from "./ToolHistory";
import { useToolRun } from "./useToolRun";

interface Form {
  target: string;
  family: IpFamily;
  via: Via;
  continuous: boolean;
  count: number;
  intervalMs: number;
  timeoutMs: number;
  payloadLen: number;
  method: "icmp" | "tcp_connect";
  tcpPort: number;
}

const DEFAULTS: Form = {
  target: "",
  family: "any",
  via: { mode: "system" },
  continuous: false,
  count: 20,
  intervalMs: 1000,
  timeoutMs: 1000,
  payloadLen: 32,
  method: "icmp",
  tcpPort: 443,
};

const rttOf = (p: ProbeOutcome): number | null =>
  p.outcome === "reply" ? p.rttMs : p.outcome === "refused" ? p.rttMs : null;
const answered = (p: ProbeOutcome) => p.outcome === "reply" || p.outcome === "refused";

/** Running totals over every probe seen live (the stored result computes the same). */
interface Totals {
  sent: number;
  received: number;
  rtts: number;
  sum: number;
  min: number | null;
  max: number | null;
  last: number | null;
  jitterSum: number;
}

const NO_TOTALS: Totals = { sent: 0, received: 0, rtts: 0, sum: 0, min: null, max: null, last: null, jitterSum: 0 };

function add(t: Totals, p: ProbeOutcome): Totals {
  const rtt = rttOf(p);
  const next = { ...t, sent: t.sent + 1, received: t.received + (answered(p) ? 1 : 0) };
  if (rtt != null) {
    next.rtts += 1;
    next.sum += rtt;
    next.min = t.min == null ? rtt : Math.min(t.min, rtt);
    next.max = t.max == null ? rtt : Math.max(t.max, rtt);
    if (t.last != null) next.jitterSum += Math.abs(rtt - t.last);
    next.last = rtt;
  }
  return next;
}

function stats(t: Totals) {
  return {
    sent: t.sent,
    received: t.received,
    lossPercent: t.sent ? (100 * (t.sent - t.received)) / t.sent : 0,
    minMs: t.min,
    avgMs: t.rtts ? t.sum / t.rtts : null,
    maxMs: t.max,
    jitterMs: t.rtts > 1 ? t.jitterSum / (t.rtts - 1) : null,
  };
}

/** Live charts keep this many probes; the stored run keeps up to 10,000. */
const LIVE_KEEP = 3000;

export function PingTool() {
  const [form, setFormState] = useState<Form>(() => loadForm("ping", DEFAULTS));
  const setForm = (patch: Partial<Form>) =>
    setFormState((f) => {
      const next = { ...f, ...patch };
      saveForm("ping", next);
      return next;
    });
  const [probes, setProbes] = useState<ProbeOutcome[]>([]);
  const [totals, setTotals] = useState<Totals>(NO_TOTALS);
  const [refresh, setRefresh] = useState(0);
  const onStored = useCallback(() => setRefresh((n) => n + 1), []);
  const { running, stopping, error, run, started, start, stop, show } = useToolRun("ping", onStored);

  const stored = run?.results as PingRunResult | null | undefined;
  const shown = running || !stored ? probes : stored.probes;
  const offset = running || !stored ? Math.max(0, totals.sent - probes.length) : stored.probesDropped;
  const summary = useMemo(() => (!running && stored ? stored : stats(totals)), [running, stored, totals]);

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (running) return;
    const params: PingParams = {
      target: form.target.trim(),
      family: form.family,
      via: form.via,
      count: form.continuous ? null : form.count,
      intervalMs: form.intervalMs,
      timeoutMs: form.timeoutMs,
      payloadLen: form.payloadLen,
      method: form.method,
      tcpPort: form.tcpPort,
    };
    setProbes([]);
    setTotals(NO_TOTALS);
    void start(
      (id, onEvent) => api.toolPing(id, params, onEvent),
      (ev) => {
        if (ev.event === "probe") {
          setTotals((t) => add(t, ev.outcome));
          setProbes((p) => {
            const next = p.length >= LIVE_KEEP ? p.slice(p.length - LIVE_KEEP + 1) : p.slice();
            next.push(ev.outcome);
            return next;
          });
        }
      },
    );
  };

  const again = (r: ToolRun) => {
    const p = r.params as Partial<PingParams>;
    setForm({
      target: p.target ?? form.target,
      family: p.family ?? form.family,
      via: p.via ?? form.via,
      continuous: p.count === null,
      count: p.count ?? form.count,
      intervalMs: p.intervalMs ?? form.intervalMs,
      timeoutMs: p.timeoutMs ?? form.timeoutMs,
      payloadLen: p.payloadLen ?? form.payloadLen,
      method: p.method ?? form.method,
      tcpPort: p.tcpPort ?? form.tcpPort,
    });
  };

  const method = !running && stored ? stored.method : form.method;
  const fallback = !running && stored?.fallbackReason;
  const resolution = !running && stored?.resolutionMs;

  return (
    <>
      <section className="card">
        <form className="tool-form" onSubmit={submit}>
          <label className="tool-field tool-target">
            <span className="field-label">Host</span>
            <input
              className="input"
              placeholder="e.g. 192.168.1.1, router.lan or example.com"
              value={form.target}
              disabled={running}
              onChange={(e) => setForm({ target: e.target.value })}
              autoFocus
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
              Ping
            </button>
          )}
          <div className="tool-options">
            <div className="segmented">
              <button type="button" className={!form.continuous ? "active" : ""} disabled={running} onClick={() => setForm({ continuous: false })}>
                Count
              </button>
              <button type="button" className={form.continuous ? "active" : ""} disabled={running} onClick={() => setForm({ continuous: true })}>
                Until stopped
              </button>
            </div>
            {!form.continuous && (
              <NumberInput value={form.count} min={1} max={100000} onCommit={(count) => setForm({ count })} />
            )}
            <span className="muted">every</span>
            <NumberInput value={form.intervalMs} min={200} max={10000} onCommit={(intervalMs) => setForm({ intervalMs })} />
            <span className="muted">ms, timeout</span>
            <NumberInput value={form.timeoutMs} min={100} max={10000} onCommit={(timeoutMs) => setForm({ timeoutMs })} />
            <span className="muted">ms</span>
            <span className="sep">·</span>
            <div className="segmented">
              <button type="button" className={form.method === "icmp" ? "active" : ""} disabled={running} onClick={() => setForm({ method: "icmp" })}>
                ICMP
              </button>
              <button type="button" className={form.method === "tcp_connect" ? "active" : ""} disabled={running} onClick={() => setForm({ method: "tcp_connect" })}>
                TCP connect
              </button>
            </div>
            {form.method === "icmp" ? (
              <>
                <span className="muted">payload</span>
                <NumberInput value={form.payloadLen} min={0} max={8192} onCommit={(payloadLen) => setForm({ payloadLen })} />
                <span className="muted">bytes</span>
              </>
            ) : (
              <>
                <span className="muted">port</span>
                <NumberInput value={form.tcpPort} min={1} max={65535} onCommit={(tcpPort) => setForm({ tcpPort })} />
              </>
            )}
          </div>
        </form>
        <div className="pad">
          <RunInfo started={started} run={run} error={error} running={running} />
        </div>
      </section>

      {(running || stored) && (
        <section className="card">
          <header className="card-header">
            <h2>Round-trip time</h2>
            <span className="muted small">
              {method === "icmp" ? "ICMP echo" : `TCP connect to port ${stored?.port ?? form.tcpPort}`}
              {resolution ? ` · ${resolution} ms clock` : ""}
            </span>
          </header>
          {fallback && (
            <p className="pad small">
              <span className="status status-warn">TCP connect</span> ICMP isn't allowed here, so TCP connects were timed:{" "}
              <span className="muted">{fallback}</span>
            </p>
          )}
          <div className="stat-row">
            <Stat label="Sent" value={String(summary.sent)} />
            <Stat label="Received" value={String(summary.received)} />
            <Stat label="Loss" value={`${summary.lossPercent.toFixed(summary.lossPercent > 0 && summary.lossPercent < 1 ? 1 : 0)} %`} bad={summary.lossPercent > 0} />
            <Stat label="Min" value={fmtMs(summary.minMs)} />
            <Stat label="Avg" value={fmtMs(summary.avgMs)} />
            <Stat label="Max" value={fmtMs(summary.maxMs)} />
            <Stat label="Jitter" value={fmtMs(summary.jitterMs)} />
          </div>
          {shown.length > 0 && (
            <div className="pad">
              <SeriesChart
                series={[{ label: "RTT", color: "var(--viz-connected)", values: shown.map(rttOf) }]}
                formatY={(v) => fmtMs(v)}
                formatX={(i) => String(i + offset + 1)}
                xLabel="Probe"
                missMarks
                window={600}
              />
              {offset > 0 && (
                <p className="muted small">The first {offset} probes aren't kept; the numbers above include them.</p>
              )}
            </div>
          )}
          <ProbeList probes={shown} offset={offset} />
        </section>
      )}

      <ToolHistory kind="ping" refreshKey={refresh} selectedId={run?.id ?? null} onOpen={show} onAgain={again} />
    </>
  );
}

function Stat({ label, value, bad }: { label: string; value: string; bad?: boolean }) {
  return (
    <div className="stat">
      <div className="stat-label">{label}</div>
      <div className={`stat-value mono ${bad ? "stat-bad" : ""}`}>{value}</div>
    </div>
  );
}

/** The last probes, newest first, with what went wrong for the ones without a reply. */
function ProbeList({ probes, offset }: { probes: ProbeOutcome[]; offset: number }) {
  const [open, setOpen] = useState(false);
  if (probes.length === 0) return null;
  const last = probes.slice(-200).map((p, k) => ({ p, n: offset + probes.length - Math.min(200, probes.length) + k + 1 }));
  return (
    <div className="pad">
      <button type="button" className="btn btn-small" onClick={() => setOpen(!open)}>
        {open ? "Hide probes" : "Show probes"}
      </button>
      {open && (
        <div className="table-scroll probe-list">
          <table className="data-table">
            <tbody>
              {last.reverse().map(({ p, n }) => (
                <tr key={n}>
                  <td className="num mono">{n}</td>
                  <td className="mono">
                    {p.outcome === "reply" && fmtMs(p.rttMs)}
                    {p.outcome === "refused" && (p.rttMs != null ? `${fmtMs(p.rttMs)} (reset)` : "reset")}
                    {p.outcome === "timeout" && <span className="muted">no reply</span>}
                    {(p.outcome === "unreachable" || p.outcome === "error") && <span className="status-bad">{p.detail}</span>}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
