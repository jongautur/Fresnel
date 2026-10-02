import { useEffect, useMemo, useState } from "react";
import { api, asApiError } from "../../api/tauri";
import type { ApiError } from "../../types/wifi";
import type { Floor, PlacedAp } from "../../types/survey";
import type { Finding, Findings, FindingScope, LinkOptions, MarkStatus } from "../../types/findings";
import { BAND_LABEL, formatDateTime } from "../../lib/format";
import { ErrorBanner, NoticeBanner } from "../ErrorBanner";
import { SignalCell } from "../SignalCell";
import type { PlanXY } from "./PlanCanvas";

const KIND_LABEL: Record<Finding["kind"], string> = {
  unknown_transmitter: "Unknown transmitter",
  security_mismatch: "Security mismatch",
  multi_frequency: "Two channels",
  bssid_inconsistent: "Possible clone",
  unlinked_radio: "Unlinked radio",
};

const MARK_LABEL: Record<MarkStatus, string> = {
  ours_unplaced: "Ours, not placed yet",
  neighbour: "Neighbour",
  ignored: "Ignored",
};

type ScopeKind = FindingScope["kind"];

/**
 * Rogue / evil-twin findings (computed in Rust from the stored survey), with
 * the actions that resolve them: mark the BSSID, or make it one of yours by
 * placing a new AP or linking it to an existing one.
 */
export function FindingsPanel({
  floor,
  revision,
  floorAps,
  onHighlight,
  onPlaceAp,
  onEditAp,
}: {
  floor: Floor;
  /** Changes whenever points or APs change, to reload. */
  revision: string;
  /** APs placed on this floor (they can be edited in place). */
  floorAps: PlacedAp[];
  /** Points on this floor to highlight, with a label each; null clears. */
  onHighlight: (labels: Map<number, string> | null) => void;
  /** Open the AP editor for a new AP linking `bssids`, at `at` (or wherever the user clicks). */
  onPlaceAp: (bssids: string[], at: PlanXY | null) => void;
  /** Open the AP editor for an AP on this floor with `bssids` added. */
  onEditAp: (apId: number, bssids: string[]) => void;
}) {
  const [scopeKind, setScopeKind] = useState<ScopeKind>("floor");
  const [data, setData] = useState<Findings | null>(null);
  const [error, setError] = useState<ApiError | null>(null);
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const [reload, setReload] = useState(0);
  const projectId = data?.projectId ?? null;

  const scope = useMemo((): FindingScope | null => {
    if (scopeKind === "floor") return { kind: "floor", id: floor.id };
    if (scopeKind === "building") return { kind: "building", id: floor.buildingId };
    return projectId != null ? { kind: "project", id: projectId } : null;
  }, [scopeKind, floor.id, floor.buildingId, projectId]);
  const scopeKey = JSON.stringify(scope);

  useEffect(() => {
    if (!scope) return;
    let cancelled = false;
    api
      .surveyFindings(scope)
      .then((f) => {
        if (cancelled) return;
        setData(f);
        setError(null);
      })
      .catch((e) => !cancelled && setError(asApiError(e)));
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [scopeKey, revision, reload]);

  const selected = data?.findings.find((f) => f.key === selectedKey) ?? null;

  // Highlight where the selected finding was heard on this floor.
  useEffect(() => {
    if (!selected) {
      onHighlight(null);
      return;
    }
    const labels = new Map<number, string>();
    for (const h of selected.heard) {
      if (h.floorId !== floor.id || labels.has(h.pointId)) continue;
      labels.set(h.pointId, h.signal.dbm != null ? String(Math.round(h.signal.dbm)) : `${h.signal.qualityPercent ?? "?"}%`);
    }
    onHighlight(labels);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selected, floor.id]);
  // Clear the highlight when the panel closes.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  useEffect(() => () => onHighlight(null), []);

  const mark = async (bssid: string, status: MarkStatus, note: string | null) => {
    if (projectId == null) return;
    try {
      await api.setBssidMark(projectId, bssid, status, note);
      setSelectedKey(null);
      setReload((r) => r + 1);
    } catch (e) {
      setError(asApiError(e));
    }
  };

  const unmark = async (bssid: string) => {
    if (projectId == null) return;
    try {
      await api.clearBssidMark(projectId, bssid);
      setReload((r) => r + 1);
    } catch (e) {
      setError(asApiError(e));
    }
  };

  const warnings = data?.findings.filter((f) => f.severity === "warning").length ?? 0;

  return (
    <section className="card">
      <header className="card-header">
        <h2>
          Findings <span className="count">{data ? data.findings.length : "…"}</span>
        </h2>
        <div className="segmented" role="tablist" aria-label="Scope">
          {(["floor", "building", "project"] as const).map((k) => (
            <button
              key={k}
              type="button"
              role="tab"
              aria-selected={scopeKind === k}
              className={scopeKind === k ? "active" : ""}
              disabled={k === "project" && projectId == null}
              onClick={() => {
                setScopeKind(k);
                setSelectedKey(null);
              }}
            >
              {k === "floor" ? "Floor" : k === "building" ? "Building" : "Project"}
            </button>
          ))}
        </div>
      </header>
      {error && <ErrorBanner error={error} />}
      {data && (
        <div className="panel-section">
          <p className="muted small">
            {data.points} point{data.points === 1 ? "" : "s"} checked · {warnings} warning{warnings === 1 ? "" : "s"} ·{" "}
            {data.linkedBssids} BSSID{data.linkedBssids === 1 ? "" : "s"} linked to placed APs
            {data.projectSsids.length > 0 && <> · your SSIDs: {data.projectSsids.join(", ")}</>}
          </p>
          {data.linkedBssids === 0 && (
            <NoticeBanner>
              Nothing counts as yours yet. Place your access points and link their BSSIDs, then transmitters using
              your SSIDs that aren't linked show up here.
            </NoticeBanner>
          )}
          {data.findings.length === 0 && data.linkedBssids > 0 && (
            <p className="muted">No findings in this scope.</p>
          )}
        </div>
      )}
      {data && data.findings.length > 0 && (
        <ul className="finding-list">
          {data.findings.map((f) => (
            <li key={f.key} className={`finding ${f.severity} ${f.key === selectedKey ? "open" : ""}`}>
              <button
                type="button"
                className="finding-head"
                aria-expanded={f.key === selectedKey}
                onClick={() => setSelectedKey((k) => (k === f.key ? null : f.key))}
              >
                <span className={`severity-dot ${f.severity}`} title={f.severity === "warning" ? "Warning" : "Info"} />
                <span className="grow">
                  <span className="finding-title">{f.title}</span>
                  <span className="sample-bssid">
                    {KIND_LABEL[f.kind]} · {f.bssid}
                    {f.ap && ` · ${f.ap.apName}`}
                  </span>
                </span>
              </button>
              {f.key === selectedKey && (
                <FindingDetail
                  finding={f}
                  floor={floor}
                  projectId={data.projectId}
                  floorAps={floorAps}
                  onMark={(status, note) => void mark(f.bssid, status, note)}
                  onPlaceAp={onPlaceAp}
                  onEditAp={onEditAp}
                  onLinked={() => {
                    setSelectedKey(null);
                    setReload((r) => r + 1);
                  }}
                  onError={setError}
                />
              )}
            </li>
          ))}
        </ul>
      )}
      {data && data.marks.length > 0 && (
        <div className="panel-section">
          <span className="field-label">Marked BSSIDs (left out of the checks)</span>
          <ul className="mark-list">
            {data.marks.map((m) => (
              <li key={m.bssid}>
                <span className="grow">
                  <span className="mono">{m.bssid}</span> <span className="chip">{MARK_LABEL[m.status]}</span>
                  {m.note && <div className="muted small">{m.note}</div>}
                </span>
                <button type="button" className="btn btn-small" onClick={() => void unmark(m.bssid)}>
                  Clear
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}
      {data && (
        <div className="panel-section">
          <span className="field-label">Limits</span>
          <ul className="finding-limits muted small">
            {data.limits.map((l) => (
              <li key={l}>{l}</li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}

function FindingDetail({
  finding: f,
  floor,
  projectId,
  floorAps,
  onMark,
  onPlaceAp,
  onEditAp,
  onLinked,
  onError,
}: {
  finding: Finding;
  floor: Floor;
  projectId: number;
  floorAps: PlacedAp[];
  onMark: (status: MarkStatus, note: string | null) => void;
  onPlaceAp: (bssids: string[], at: PlanXY | null) => void;
  onEditAp: (apId: number, bssids: string[]) => void;
  onLinked: () => void;
  onError: (e: ApiError) => void;
}) {
  const [note, setNote] = useState("");
  const [ours, setOurs] = useState<LinkOptions | null>(null);
  const [chosen, setChosen] = useState<Set<string>>(new Set());
  const [apId, setApId] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  // A linked BSSID is yours by definition: marks don't apply to it.
  const markable = f.ownership !== "linked";
  const elsewhere = f.heard.filter((h) => h.floorId !== floor.id).length;
  const strongest = f.heard[0];

  const openOurs = async () => {
    try {
      const o = await api.bssidLinkOptions(projectId, f.bssid);
      setOurs(o);
      setChosen(new Set([f.bssid]));
      setApId(f.ap?.apId ?? o.probableAp?.apId ?? null);
    } catch (e) {
      onError(asApiError(e));
    }
  };

  const floorName = (id: number) => {
    const fl = ours?.floors.find((x) => x.id === id);
    return fl ? `${fl.buildingName} › ${fl.name}` : `floor ${id}`;
  };

  const link = async () => {
    if (!ours || apId == null) return;
    const ap = ours.aps.find((a) => a.id === apId);
    if (!ap) return;
    if (floorAps.some((a) => a.id === ap.id)) {
      onEditAp(ap.id, [...chosen]);
      return;
    }
    // On another floor: link directly through the same command the editor uses.
    setBusy(true);
    try {
      await api.updatePlacedAp(ap.id, {
        name: ap.name,
        x: ap.x,
        y: ap.y,
        model: ap.model,
        notes: ap.notes,
        bssids: [...new Set([...ap.bssids, ...chosen])],
      });
      onLinked();
    } catch (e) {
      onError(asApiError(e));
    } finally {
      setBusy(false);
    }
  };

  const here = ours?.strongestPerFloor.find((h) => h.floorId === floor.id) ?? null;

  return (
    <div className="finding-detail">
      <p className="small">{f.explanation}</p>
      {f.coarse && <p className="muted small">Coarse comparison: some readings predate security detail.</p>}
      <dl className="finding-facts small">
        {strongest && (
          <>
            <dt>Strongest</dt>
            <dd>
              <SignalCell signal={strongest.signal} compact /> at point {strongest.pointNumber}, {strongest.buildingName} ›{" "}
              {strongest.floorName}
            </dd>
          </>
        )}
        {f.channels.length > 0 && (
          <>
            <dt>Channels</dt>
            <dd className="mono">{f.channels.join(", ")}</dd>
          </>
        )}
        {f.firstSeen && f.lastSeen && (
          <>
            <dt>Seen</dt>
            <dd className="mono">
              {formatDateTime(f.firstSeen)}
              {f.lastSeen !== f.firstSeen && ` – ${formatDateTime(f.lastSeen)}`}
            </dd>
          </>
        )}
      </dl>
      {elsewhere > 0 && (
        <p className="muted small">
          {elsewhere} of {f.heard.length} readings are on other floors; the points on this plan are highlighted.
        </p>
      )}

      {f.comparedWith.length > 0 && (
        <div>
          <span className="field-label">Compared with yours</span>
          <ul className="finding-refs small">
            {f.comparedWith.map((r) => (
              <li key={`${r.bssid}-${r.band}`}>
                <span className="mono">{r.bssid}</span>
                {r.apName && <span className="chip">{r.apName}</span>} <span className="muted">{BAND_LABEL[r.band]}</span>
                <div className="muted">{r.security.summary}</div>
              </li>
            ))}
          </ul>
        </div>
      )}

      <div className="table-scroll">
        <table className="data-table samples-table">
          <thead>
            <tr>
              <th>Where</th>
              <th>Signal</th>
              <th className="num">Ch</th>
            </tr>
          </thead>
          <tbody>
            {f.heard.map((h, i) => (
              <tr key={`${h.pointId}-${h.frequencyMhz}-${i}`}>
                <td>
                  <div>
                    Point {h.pointNumber}
                    {h.floorId !== floor.id && <span className="muted"> · {h.floorName}</span>}
                  </div>
                  <div className="sample-bssid">
                    {h.ssid ?? "hidden"}
                    {h.security && ` · ${h.security.summary}`}
                  </div>
                  {h.note && <div className="muted small">{h.note}</div>}
                </td>
                <td>
                  <SignalCell signal={h.signal} compact />
                </td>
                <td className="num mono" title={`${BAND_LABEL[h.band]} · ${h.frequencyMhz} MHz`}>
                  {h.channel ?? "?"}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      {f.kind === "unlinked_radio" && f.ap && floorAps.some((a) => a.id === f.ap!.apId) && (
        <div className="panel-actions">
          <button type="button" className="btn btn-primary btn-small" onClick={() => onEditAp(f.ap!.apId, [f.bssid])}>
            Link to {f.ap.apName}…
          </button>
        </div>
      )}

      {markable && !ours && (
        <>
          <input
            className="input ap-input"
            placeholder="Note (optional), e.g. café next door"
            value={note}
            onChange={(e) => setNote(e.target.value)}
          />
          <div className="panel-actions">
            <button type="button" className="btn btn-small" onClick={() => onMark("neighbour", note.trim() || null)}>
              Mark as neighbour
            </button>
            <button type="button" className="btn btn-small" onClick={() => onMark("ignored", note.trim() || null)}>
              Ignore
            </button>
            <button
              type="button"
              className="btn btn-small"
              title="Yours, but not on a plan: no longer reported as unknown"
              onClick={() => onMark("ours_unplaced", note.trim() || null)}
            >
              Ours, not placed yet
            </button>
            <button type="button" className="btn btn-primary btn-small" onClick={() => void openOurs()}>
              This is ours…
            </button>
          </div>
        </>
      )}

      {ours && (
        <div className="finding-ours">
          <span className="field-label">Link as yours</span>
          {ours.relatedBssids.length > 1 && (
            <p className="muted small">
              BSSIDs that look like radios of the same AP (same MLD address, else a similar MAC: a heuristic). Tick
              the ones that are.
            </p>
          )}
          {ours.relatedBssids.map((b) => (
            <label key={b} className="checkbox">
              <input
                type="checkbox"
                checked={chosen.has(b)}
                onChange={(e) =>
                  setChosen((c) => {
                    const n = new Set(c);
                    if (e.target.checked) n.add(b);
                    else n.delete(b);
                    return n;
                  })
                }
              />
              <span className="mono">{b}</span>
            </label>
          ))}
          <div className="panel-actions">
            <button
              type="button"
              className="btn btn-small"
              disabled={chosen.size === 0}
              onClick={() => onPlaceAp([...chosen], here ? { x: here.x, y: here.y } : null)}
              title={
                here
                  ? `Starts at point ${here.pointNumber}, its strongest reading on this floor; click the plan to move it`
                  : "Not heard on this floor: click the plan where it is mounted"
              }
            >
              Place a new AP on this floor…
            </button>
          </div>
          {ours.aps.length > 0 && (
            <div className="length-input">
              <select
                className="input"
                value={apId ?? ""}
                onChange={(e) => setApId(e.target.value ? Number(e.target.value) : null)}
              >
                <option value="">Link to an existing AP…</option>
                {ours.aps.map((a) => (
                  <option key={a.id} value={a.id}>
                    {a.name} ({floorName(a.floorId)})
                    {ours.probableAp?.apId === a.id ? " · probable" : ""}
                  </option>
                ))}
              </select>
              <button
                type="button"
                className="btn btn-primary btn-small"
                disabled={apId == null || chosen.size === 0 || busy}
                onClick={() => void link()}
              >
                Link
              </button>
            </div>
          )}
          <div className="panel-actions">
            <button type="button" className="btn btn-small" onClick={() => setOurs(null)}>
              Back
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
