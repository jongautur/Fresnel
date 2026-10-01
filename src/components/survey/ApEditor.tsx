import { useMemo, useState, type FormEvent } from "react";
import type { PlacedAp, PlacedApInput, SurveyPoint } from "../../types/survey";
import { BAND_LABEL } from "../../lib/format";
import { apKey, suggestBssidGroup } from "../../lib/heatmap";
import { ConfirmButton } from "../ConfirmButton";

interface BssidRow {
  bssid: string;
  ssids: Set<string>;
  band: string | null;
  channel: number | null;
  bestDbm: number | null;
}

function normalizeBssid(s: string): string | null {
  const t = s.trim();
  if (!/^[0-9a-fA-F:.\-]+$/.test(t)) return null;
  const hex = t.replace(/[^0-9a-fA-F]/g, "");
  return hex.length === 12 ? hex.toUpperCase().match(/../g)!.join(":") : null;
}

/** Create or edit a placed AP: name, details and the BSSIDs it broadcasts. */
export function ApEditor({
  ap,
  position,
  points,
  otherAps,
  suggestRadiusPx,
  defaultName,
  color,
  onSave,
  onDelete,
  onCancel,
}: {
  /** null = new AP at `position`. */
  ap: PlacedAp | null;
  position: { x: number; y: number };
  points: SurveyPoint[];
  /** Every other AP in the building (their BSSIDs can't be claimed twice). */
  otherAps: PlacedAp[];
  suggestRadiusPx: number;
  defaultName: string;
  color: string;
  onSave: (input: PlacedApInput) => Promise<boolean>;
  onDelete?: () => void;
  onCancel: () => void;
}) {
  const claimedBy = useMemo(() => {
    const m = new Map<string, string>();
    for (const o of otherAps) for (const b of o.bssids) m.set(b, o.name);
    return m;
  }, [otherAps]);

  const [name, setName] = useState(ap?.name ?? defaultName);
  const [model, setModel] = useState(ap?.model ?? "");
  const [notes, setNotes] = useState(ap?.notes ?? "");
  const [manual, setManual] = useState("");
  const [extra, setExtra] = useState<string[]>([]);
  const suggested = useMemo(
    () => (ap ? [] : suggestBssidGroup(points, position.x, position.y, suggestRadiusPx, new Set(claimedBy.keys()))),
    // Only for a new AP, computed once per placement.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [],
  );
  const [selected, setSelected] = useState<Set<string>>(() => new Set(ap?.bssids ?? suggested));
  const [busy, setBusy] = useState(false);

  // Every BSSID heard on this floor, plus ones already linked or typed in.
  const groups = useMemo(() => {
    const rows = new Map<string, BssidRow>();
    const row = (bssid: string) => {
      let r = rows.get(bssid);
      if (!r) {
        r = { bssid, ssids: new Set(), band: null, channel: null, bestDbm: null };
        rows.set(bssid, r);
      }
      return r;
    };
    for (const p of points)
      for (const s of p.samples) {
        const r = row(s.bssid);
        if (s.ssid) r.ssids.add(s.ssid);
        r.band = BAND_LABEL[s.band];
        r.channel = s.channel;
        if (s.signal.dbm != null && (r.bestDbm == null || s.signal.dbm > r.bestDbm)) r.bestDbm = s.signal.dbm;
      }
    for (const b of [...(ap?.bssids ?? []), ...extra]) row(b);
    const byKey = new Map<string, BssidRow[]>();
    for (const r of rows.values()) {
      const k = apKey(r.bssid);
      byKey.set(k, [...(byKey.get(k) ?? []), r]);
    }
    const sorted = [...byKey.entries()].map(([key, rs]) => ({
      key,
      rows: rs.sort((a, b) => a.bssid.localeCompare(b.bssid)),
      best: Math.max(...rs.map((r) => r.bestDbm ?? -200)),
      suggested: rs.some((r) => suggested.includes(r.bssid)),
      linked: rs.some((r) => selected.has(r.bssid)),
    }));
    // Linked / suggested first, then strongest.
    return sorted.sort((a, b) => Number(b.linked) - Number(a.linked) || Number(b.suggested) - Number(a.suggested) || b.best - a.best);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [points, ap, extra, suggested]);

  const toggle = (bssids: string[], on: boolean) =>
    setSelected((s) => {
      const n = new Set(s);
      for (const b of bssids) if (!claimedBy.has(b)) (on ? n.add(b) : n.delete(b));
      return n;
    });

  const addManual = () => {
    const b = normalizeBssid(manual);
    if (!b) return;
    if (!claimedBy.has(b)) {
      setExtra((e) => (e.includes(b) ? e : [...e, b]));
      toggle([b], true);
    }
    setManual("");
  };

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!name.trim()) return;
    setBusy(true);
    const ok = await onSave({
      name: name.trim(),
      x: position.x,
      y: position.y,
      model: model.trim() || null,
      notes: notes.trim() || null,
      bssids: [...selected],
    });
    setBusy(false);
    return ok;
  };

  const manualValid = normalizeBssid(manual) != null;

  return (
    <section className="card">
      <header className="card-header">
        <h2>
          <span className="ap-swatch" style={{ background: color }} /> {ap ? "Access point" : "New access point"}
        </h2>
        {ap && onDelete && (
          <ConfirmButton onConfirm={onDelete} title="Remove this access point from the plan">
            Delete
          </ConfirmButton>
        )}
      </header>
      <form className="panel-section" onSubmit={submit}>
        <label>
          <span className="field-label">Name</span>
          <input className="input ap-input" value={name} onChange={(e) => setName(e.target.value)} autoFocus={!ap} />
        </label>
        <div className="ap-fields">
          <label>
            <span className="field-label">Model (optional)</span>
            <input className="input ap-input" value={model} placeholder="e.g. Ubiquiti U6 Lite" onChange={(e) => setModel(e.target.value)} />
          </label>
          <label>
            <span className="field-label">Notes (optional)</span>
            <input className="input ap-input" value={notes} placeholder="e.g. ceiling, 2.6 m" onChange={(e) => setNotes(e.target.value)} />
          </label>
        </div>

        <div>
          <span className="field-label">
            BSSIDs it broadcasts <span className="muted">({selected.size} linked)</span>
          </span>
          {!ap && suggested.length > 0 && (
            <p className="muted small">Pre-selected: the strongest group heard near this spot. Check it matches.</p>
          )}
          <div className="bssid-groups">
            {groups.length === 0 && <p className="muted small">No BSSIDs heard on this floor yet. Add one below.</p>}
            {groups.map((g) => {
              const free = g.rows.filter((r) => !claimedBy.has(r.bssid));
              const allOn = free.length > 0 && free.every((r) => selected.has(r.bssid));
              return (
                <div key={g.key} className="bssid-group">
                  {g.rows.length > 1 && free.length > 0 && (
                    <label className="checkbox bssid-group-head" title="BSSIDs with the same base MAC: usually one physical AP">
                      <input type="checkbox" checked={allOn} onChange={(e) => toggle(free.map((r) => r.bssid), e.target.checked)} />
                      Same base MAC ({g.rows.length}){g.suggested && <span className="chip">suggested</span>}
                    </label>
                  )}
                  {g.rows.map((r) => {
                    const owner = claimedBy.get(r.bssid);
                    return (
                      <label key={r.bssid} className={`checkbox bssid-row ${owner ? "disabled" : ""}`} title={owner ? `Linked to ${owner}` : undefined}>
                        <input
                          type="checkbox"
                          checked={selected.has(r.bssid)}
                          disabled={!!owner}
                          onChange={(e) => toggle([r.bssid], e.target.checked)}
                        />
                        <span className="bssid-main">
                          <span className="sample-ssid">{r.ssids.size ? [...r.ssids].join(", ") : <span className="muted italic">hidden</span>}</span>
                          <span className="sample-bssid">
                            {r.bssid}
                            {r.band && ` · ${r.band}`}
                            {r.channel != null && ` ch ${r.channel}`}
                            {owner && ` · on ${owner}`}
                          </span>
                        </span>
                        <span className="mono muted">{r.bestDbm != null ? `${Math.round(r.bestDbm)} dBm` : "—"}</span>
                      </label>
                    );
                  })}
                </div>
              );
            })}
          </div>
          <div className="length-input bssid-manual">
            <input
              className="input"
              placeholder="Add BSSID, e.g. from the label"
              value={manual}
              onChange={(e) => setManual(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  addManual();
                }
              }}
            />
            <button type="button" className="btn" onClick={addManual} disabled={!manualValid}>
              Add
            </button>
          </div>
        </div>

        <div className="panel-actions">
          <button className="btn btn-primary" type="submit" disabled={busy || !name.trim()}>
            {ap ? "Save" : "Place AP"}
          </button>
          <button className="btn" type="button" onClick={onCancel}>
            Cancel
          </button>
          {ap && <span className="muted small">Drag the marker to move it.</span>}
        </div>
      </form>
    </section>
  );
}
