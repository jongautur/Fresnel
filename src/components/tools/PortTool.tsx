import { useCallback, useState, type FormEvent } from "react";
import { api } from "../../api/tauri";
import type { IpFamily, PortCheckParams, PortCheckResult, PortOutcome, PortState, ToolRun, Via } from "../../types/tools";
import { NumberInput } from "../NumberInput";
import { fmtMs, loadForm, saveForm } from "../../lib/tools";
import { FamilyPicker, ViaPicker } from "./ViaPicker";
import { RunInfo } from "./RunInfo";
import { ToolHistory } from "./ToolHistory";
import { useToolRun } from "./useToolRun";

/** Common services, so nobody has to remember port numbers. */
const PRESETS: { label: string; ports: string }[] = [
  { label: "Web", ports: "80, 443, 8080, 8443" },
  { label: "Remote access", ports: "22, 23, 3389, 5900" },
  { label: "Mail", ports: "25, 110, 143, 465, 587, 993, 995" },
  { label: "File sharing", ports: "21, 139, 445, 2049" },
  { label: "Printers", ports: "515, 631, 9100" },
  { label: "DNS & DHCP admin", ports: "53, 853" },
  { label: "Network gear", ports: "22, 23, 80, 443, 161, 8291, 8443" },
];

/** Well-known names for the ports above (and a few more). */
const SERVICE: Record<number, string> = {
  21: "FTP", 22: "SSH", 23: "Telnet", 25: "SMTP", 53: "DNS", 80: "HTTP", 110: "POP3", 139: "NetBIOS",
  143: "IMAP", 161: "SNMP", 443: "HTTPS", 445: "SMB", 465: "SMTPS", 515: "LPD", 587: "Submission",
  631: "IPP", 853: "DNS over TLS", 993: "IMAPS", 995: "POP3S", 1883: "MQTT", 2049: "NFS", 3306: "MySQL",
  3389: "RDP", 5432: "PostgreSQL", 5900: "VNC", 8080: "HTTP alt", 8291: "Winbox", 8443: "HTTPS alt",
  9100: "Raw printing",
};

const STATE_LABEL: Record<PortState, string> = {
  open: "Open",
  closed: "Closed",
  filtered: "Filtered",
  unreachable: "Unreachable",
  error: "Error",
};
const STATE_CLASS: Record<PortState, string> = {
  open: "status-ok",
  closed: "status-idle",
  filtered: "status-warn",
  unreachable: "status-bad",
  error: "status-bad",
};
const STATE_NOTE: Record<PortState, string> = {
  open: "Something accepted the connection.",
  closed: "The host answered that nothing listens there (TCP reset).",
  filtered: "No answer before the timeout: a firewall dropped it, or the host is off.",
  unreachable: "A router or this computer said the host can't be reached.",
  error: "The connection failed for another reason.",
};

interface Form {
  target: string;
  family: IpFamily;
  via: Via;
  ports: string;
  timeoutMs: number;
  showAll: boolean;
}

const DEFAULTS: Form = {
  target: "",
  family: "any",
  via: { mode: "system" },
  ports: "22, 80, 443",
  timeoutMs: 2000,
  showAll: false,
};

export function PortTool() {
  const [form, setFormState] = useState<Form>(() => loadForm("ports", DEFAULTS));
  const setForm = (patch: Partial<Form>) =>
    setFormState((f) => {
      const next = { ...f, ...patch };
      saveForm("ports", next);
      return next;
    });
  const [live, setLive] = useState<PortOutcome[]>([]);
  const [refresh, setRefresh] = useState(0);
  const onStored = useCallback(() => setRefresh((n) => n + 1), []);
  const { running, stopping, error, run, started, start, stop, show } = useToolRun("port_check", onStored);
  const stored = run?.results as PortCheckResult | null | undefined;
  const ports = (!running && stored ? stored.ports : [...live].sort((a, b) => a.port - b.port)) ?? [];
  const counts = (s: PortState) => ports.filter((p) => p.state === s).length;
  // With many ports, the closed and filtered ones are noise: show open first.
  const many = ports.length > 20;
  const visible = many && !form.showAll ? ports.filter((p) => p.state === "open" || p.state === "unreachable" || p.state === "error") : ports;

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (running) return;
    const params: PortCheckParams = {
      target: form.target.trim(),
      family: form.family,
      via: form.via,
      ports: form.ports,
      timeoutMs: form.timeoutMs,
    };
    setLive([]);
    void start(
      (id, onEvent) => api.toolPortCheck(id, params, onEvent),
      (ev) => {
        if (ev.event === "port") {
          const { event: _event, ...outcome } = ev;
          setLive((l) => [...l, outcome]);
        }
      },
    );
  };

  const again = (r: ToolRun) => {
    const p = r.params as Partial<PortCheckParams>;
    setForm({
      target: p.target ?? form.target,
      family: p.family ?? form.family,
      via: p.via ?? form.via,
      ports: p.ports ?? form.ports,
      timeoutMs: p.timeoutMs ?? form.timeoutMs,
    });
  };

  return (
    <>
      <section className="card">
        <form className="tool-form" onSubmit={submit}>
          <label className="tool-field tool-target">
            <span className="field-label">Host</span>
            <input
              className="input"
              placeholder="e.g. 192.168.1.20 or nas.lan"
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
            <button type="submit" className="btn btn-primary" disabled={!form.target.trim() || !form.ports.trim()}>
              Check
            </button>
          )}
          <div className="tool-options">
            <label className="tool-field tool-grow">
              <span className="field-label">Ports</span>
              <input
                className="input mono"
                placeholder="22, 80, 443, 8000-8100"
                value={form.ports}
                disabled={running}
                onChange={(e) => setForm({ ports: e.target.value })}
              />
            </label>
            <span className="muted">timeout</span>
            <NumberInput value={form.timeoutMs} min={100} max={10000} onCommit={(timeoutMs) => setForm({ timeoutMs })} />
            <span className="muted">ms</span>
          </div>
          <div className="tool-options">
            {PRESETS.map((p) => (
              <button key={p.label} type="button" className="btn btn-small" disabled={running} onClick={() => setForm({ ports: p.ports })} title={p.ports}>
                {p.label}
              </button>
            ))}
          </div>
          <div className="field-hint">
            TCP only, up to 1,024 ports, 64 at a time. Only check hosts you are responsible for: to the host's owner,
            a port check looks like a scan.
          </div>
        </form>
        <div className="pad">
          <RunInfo started={started} run={run} error={error} running={running} />
        </div>
      </section>

      {(running || stored) && (
        <section className="card">
          <header className="card-header">
            <h2>Ports</h2>
            <span className="small">
              <span className="status status-ok">{counts("open")} open</span>{" "}
              <span className="muted">
                · {counts("closed")} closed · {counts("filtered")} filtered
                {counts("unreachable") + counts("error") > 0 && ` · ${counts("unreachable") + counts("error")} unreachable`}
                {running && stored == null && ` · ${ports.length} checked`}
              </span>
            </span>
          </header>
          {many && (
            <div className="pad">
              <label className="checkbox">
                <input type="checkbox" checked={form.showAll} onChange={(e) => setForm({ showAll: e.target.checked })} />
                Show closed and filtered ports too
              </label>
            </div>
          )}
          <div className="table-scroll">
            <table className="data-table">
              <thead>
                <tr>
                  <th className="num">Port</th>
                  <th>Service</th>
                  <th>State</th>
                  <th className="num">Connect time</th>
                  <th>Detail</th>
                </tr>
              </thead>
              <tbody>
                {visible.map((p) => (
                  <tr key={p.port}>
                    <td className="num mono">{p.port}</td>
                    <td className="muted">{SERVICE[p.port] ?? ""}</td>
                    <td title={STATE_NOTE[p.state]}>
                      <span className={`status ${STATE_CLASS[p.state]}`}>{STATE_LABEL[p.state]}</span>
                    </td>
                    <td className="num mono">{p.connectMs != null ? fmtMs(p.connectMs) : "—"}</td>
                    <td className="muted small wrap">{p.detail ?? ""}</td>
                  </tr>
                ))}
                {visible.length === 0 && (
                  <tr>
                    <td colSpan={5} className="empty">
                      {running ? "Checking…" : "No open ports."}
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
        </section>
      )}

      <ToolHistory kind="port_check" refreshKey={refresh} selectedId={run?.id ?? null} onOpen={show} onAgain={again} />
    </>
  );
}
