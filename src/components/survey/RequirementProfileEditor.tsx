import { useEffect, useMemo, useState, type FormEvent } from "react";
import { api, asApiError } from "../../api/tauri";
import type { ApiError, Band } from "../../types/wifi";
import type {
  PresetInfo,
  RequirementProfile,
  RequirementProfileInput,
  RequirementTarget,
  RequirementValues,
  TargetOptions,
} from "../../types/requirements";
import { BAND_LABEL } from "../../lib/format";
import { ConfirmButton } from "../ConfirmButton";
import { ErrorBanner } from "../ErrorBanner";
import { NumberInput } from "../NumberInput";

const REQUIRABLE: Band[] = ["2.4ghz", "5ghz", "6ghz"];
const NEW = -1;

const targetKey = (t: RequirementTarget) => (t.kind === "ssid" ? `ssid:${t.ssidRaw.join(",")}` : `ap:${t.apId}`);

function keyToTarget(k: string): RequirementTarget {
  return k.startsWith("ap:")
    ? { kind: "ap", apId: Number(k.slice(3)) }
    : { kind: "ssid", ssidRaw: k.slice(5).split(",").map(Number) };
}

const blank = (presets: PresetInfo[]): RequirementProfileInput => {
  const office = presets.find((p) => p.preset === "office_data")?.values;
  return {
    name: "",
    preset: office ? "office_data" : "custom",
    isDefault: false,
    ...(office ?? {
      primaryMinDbm: -67,
      secondaryMinDbm: null,
      cochannelMax: null,
      cochannelLevelDbm: null,
      requiredBands: [],
      minSnrDb: null,
      maxUtilPct: null,
    }),
  };
};

const fromProfile = (p: RequirementProfile): RequirementProfileInput => ({
  name: p.name,
  preset: p.preset,
  isDefault: p.isDefault,
  primaryMinDbm: p.primaryMinDbm,
  secondaryMinDbm: p.secondaryMinDbm,
  cochannelMax: p.cochannelMax,
  cochannelLevelDbm: p.cochannelLevelDbm,
  requiredBands: p.requiredBands,
  minSnrDb: p.minSnrDb,
  maxUtilPct: p.maxUtilPct,
});

/** An optional rule: a checkbox that switches it on, and its number. */
function OptionalNumber({
  label,
  unit,
  value,
  fallback,
  min,
  max,
  hint,
  onChange,
}: {
  label: string;
  unit: string;
  value: number | null;
  fallback: number;
  min: number;
  max: number;
  hint?: string;
  onChange: (v: number | null) => void;
}) {
  return (
    <div className="req-field">
      <label className="checkbox" title={hint}>
        <input type="checkbox" checked={value != null} onChange={(e) => onChange(e.target.checked ? fallback : null)} />
        {label}
      </label>
      {value != null && (
        <span className="length-input">
          <NumberInput value={value} min={min} max={max} onCommit={onChange} />
          <span className="muted">{unit}</span>
        </span>
      )}
    </div>
  );
}

/**
 * Project-level requirement profiles: values (from a preset or custom),
 * which networks they apply to, and which one is the project default.
 */
export function RequirementProfileEditor({
  projectId,
  initialId,
  onChanged,
  onClose,
}: {
  projectId: number;
  /** Profile to open first; null = the default, or a new one if none. */
  initialId: number | null;
  /** After any save or delete, so the floor is re-evaluated. */
  onChanged: () => void;
  onClose: () => void;
}) {
  const [profiles, setProfiles] = useState<RequirementProfile[] | null>(null);
  const [presets, setPresets] = useState<PresetInfo[]>([]);
  const [options, setOptions] = useState<TargetOptions | null>(null);
  const [editId, setEditId] = useState<number>(NEW);
  const [draft, setDraft] = useState<RequirementProfileInput | null>(null);
  const [targets, setTargets] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);

  const open = (id: number, list: RequirementProfile[], presetList: PresetInfo[]) => {
    const p = list.find((x) => x.id === id);
    setEditId(p ? p.id : NEW);
    setDraft(p ? fromProfile(p) : { ...blank(presetList), isDefault: list.length === 0 });
    setTargets(new Set(p ? p.targets.map(targetKey) : []));
  };

  useEffect(() => {
    let cancelled = false;
    Promise.all([api.listRequirementProfiles(projectId), api.requirementPresets(), api.requirementTargetOptions(projectId)])
      .then(([list, presetList, opts]) => {
        if (cancelled) return;
        setProfiles(list);
        setPresets(presetList);
        setOptions(opts);
        open(initialId ?? list.find((p) => p.isDefault)?.id ?? list[0]?.id ?? NEW, list, presetList);
      })
      .catch((e) => !cancelled && setError(asApiError(e)));
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projectId]);

  // Targets saved earlier that are no longer offered (e.g. an SSID only heard on a deleted floor).
  const extraTargets = useMemo(() => {
    const p = profiles?.find((x) => x.id === editId);
    if (!p || !options) return [];
    const offered = new Set([
      ...options.ssids.map((s) => targetKey({ kind: "ssid", ssidRaw: s.ssidRaw })),
      ...options.aps.map((a) => targetKey({ kind: "ap", apId: a.apId })),
    ]);
    return p.targets.filter((t) => !offered.has(targetKey(t)));
  }, [profiles, options, editId]);

  if (!draft || !profiles) {
    return (
      <section className="card">
        <header className="card-header">
          <h2>Requirement profiles</h2>
        </header>
        {error && <ErrorBanner error={error} />}
      </section>
    );
  }

  // Any edit to a value leaves the preset (the backend does the same).
  const setValues = (patch: Partial<RequirementValues>) => setDraft((d) => d && { ...d, ...patch, preset: "custom" });
  const applyPreset = (preset: PresetInfo) =>
    setDraft((d) => d && { ...d, preset: preset.preset, ...(preset.values ?? {}) });
  const toggleTarget = (k: string, on: boolean) =>
    setTargets((s) => {
      const n = new Set(s);
      if (on) n.add(k);
      else n.delete(k);
      return n;
    });

  const save = async (e: FormEvent) => {
    e.preventDefault();
    if (!draft.name.trim()) return;
    setBusy(true);
    try {
      const input = { ...draft, name: draft.name.trim() };
      const saved =
        editId === NEW
          ? await api.createRequirementProfile(projectId, input)
          : await api.updateRequirementProfile(editId, input);
      await api.setRequirementTargets(saved.id, [...targets].map(keyToTarget));
      const list = await api.listRequirementProfiles(projectId);
      setProfiles(list);
      open(saved.id, list, presets);
      setError(null);
      onChanged();
    } catch (err) {
      setError(asApiError(err));
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    if (editId === NEW) return;
    try {
      await api.deleteRequirementProfile(editId);
      const list = await api.listRequirementProfiles(projectId);
      setProfiles(list);
      open(list.find((p) => p.isDefault)?.id ?? list[0]?.id ?? NEW, list, presets);
      setError(null);
      onChanged();
    } catch (err) {
      setError(asApiError(err));
    }
  };

  const presetLabel = presets.find((p) => p.preset === draft.preset)?.label ?? "Custom";

  return (
    <section className="card">
      <header className="card-header">
        <h2>Requirement profiles</h2>
        <button type="button" className="btn btn-small" onClick={onClose}>
          Done
        </button>
      </header>
      {error && <ErrorBanner error={error} />}
      <form className="panel-section" onSubmit={save}>
        <label>
          <span className="field-label">Profile (project-wide)</span>
          <select className="input req-wide" value={editId} onChange={(e) => open(Number(e.target.value), profiles, presets)}>
            {profiles.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
                {p.isDefault ? " (default)" : ""}
              </option>
            ))}
            <option value={NEW}>New profile…</option>
          </select>
        </label>
        <div className="heat-fields">
          <label>
            <span className="field-label">Name</span>
            <input
              className="input req-wide"
              value={draft.name}
              placeholder="e.g. Office"
              onChange={(e) => setDraft({ ...draft, name: e.target.value })}
            />
          </label>
          <label>
            <span className="field-label">Start from</span>
            <select
              className="input req-wide"
              value={draft.preset}
              onChange={(e) => {
                const p = presets.find((x) => x.preset === e.target.value);
                if (p) applyPreset(p);
              }}
            >
              {presets.map((p) => (
                <option key={p.preset} value={p.preset}>
                  {p.label}
                </option>
              ))}
            </select>
          </label>
          <label>
            <span className="field-label">Primary signal</span>
            <span className="length-input">
              <NumberInput
                value={draft.primaryMinDbm}
                min={-100}
                max={-20}
                onCommit={(v) => setValues({ primaryMinDbm: v })}
              />
              <span className="muted">dBm</span>
            </span>
          </label>
        </div>
        <p className="muted small">
          {presetLabel === "Custom"
            ? "Custom values."
            : `${presetLabel}: common vendor design guidance, a starting point rather than a standard.`}{" "}
          Primary = the strongest target BSSID at each point.
        </p>

        <OptionalNumber
          label="Second AP at least"
          unit="dBm"
          value={draft.secondaryMinDbm}
          fallback={-75}
          min={-100}
          max={-20}
          hint="A different physical AP, same SSID and band as the primary (for roaming)"
          onChange={(v) => setValues({ secondaryMinDbm: v })}
        />
        <div className="req-field">
          <label className="checkbox" title="Other radios whose channel overlaps the primary's, heard at or above the level">
            <input
              type="checkbox"
              checked={draft.cochannelMax != null}
              onChange={(e) =>
                setValues(
                  e.target.checked ? { cochannelMax: 2, cochannelLevelDbm: -80 } : { cochannelMax: null, cochannelLevelDbm: null },
                )
              }
            />
            Co-channel at most
          </label>
          {draft.cochannelMax != null && (
            <span className="length-input">
              <NumberInput value={draft.cochannelMax} min={0} max={50} onCommit={(v) => setValues({ cochannelMax: v })} />
              <span className="muted">at</span>
              <NumberInput
                value={draft.cochannelLevelDbm ?? -80}
                min={-100}
                max={-20}
                onCommit={(v) => setValues({ cochannelLevelDbm: v })}
              />
              <span className="muted">dBm</span>
            </span>
          )}
        </div>
        <div className="req-field">
          <span className="field-label">Required bands</span>
          <span className="panel-actions">
            {REQUIRABLE.map((b) => (
              <label key={b} className="checkbox">
                <input
                  type="checkbox"
                  checked={draft.requiredBands.includes(b)}
                  onChange={(e) =>
                    setValues({
                      requiredBands: e.target.checked
                        ? [...draft.requiredBands, b]
                        : draft.requiredBands.filter((x) => x !== b),
                    })
                  }
                />
                {BAND_LABEL[b]}
              </label>
            ))}
          </span>
        </div>
        <OptionalNumber
          label="SNR at least"
          unit="dB"
          value={draft.minSnrDb}
          fallback={25}
          min={0}
          max={80}
          hint="Judged only where the driver reports a noise floor (not iwlwifi, not Windows)"
          onChange={(v) => setValues({ minSnrDb: v })}
        />
        <OptionalNumber
          label="Channel load at most"
          unit="%"
          value={draft.maxUtilPct}
          fallback={50}
          min={0}
          max={100}
          hint="As the AP advertises it (BSS Load element); judged only where the AP sends it"
          onChange={(v) => setValues({ maxUtilPct: v })}
        />

        <div>
          <span className="field-label">
            Applies to <span className="muted">({targets.size} selected)</span>
          </span>
          <div className="req-targets">
            {options && options.aps.length > 0 && (
              <>
                <div className="muted small">Placed access points (all their BSSIDs; works for hidden SSIDs)</div>
                {options.aps.map((a) => {
                  const k = targetKey({ kind: "ap", apId: a.apId });
                  return (
                    <label key={k} className="checkbox">
                      <input type="checkbox" checked={targets.has(k)} onChange={(e) => toggleTarget(k, e.target.checked)} />
                      <span className="grow">{a.name}</span>
                      <span className="muted small">
                        {a.buildingName} › {a.floorName} · {a.bssidCount} BSSID{a.bssidCount === 1 ? "" : "s"}
                      </span>
                    </label>
                  );
                })}
              </>
            )}
            {options && options.ssids.length > 0 && <div className="muted small">SSIDs heard in this project</div>}
            {options?.ssids.map((s) => {
              const k = targetKey({ kind: "ssid", ssidRaw: s.ssidRaw });
              return (
                <label key={k} className="checkbox">
                  <input type="checkbox" checked={targets.has(k)} onChange={(e) => toggleTarget(k, e.target.checked)} />
                  <span className="grow">{s.label}</span>
                  <span className="muted small">{s.points} pts</span>
                </label>
              );
            })}
            {extraTargets.map((t) => {
              const k = targetKey(t);
              return (
                <label key={k} className="checkbox">
                  <input type="checkbox" checked={targets.has(k)} onChange={(e) => toggleTarget(k, e.target.checked)} />
                  <span className="grow">{t.label}</span>
                  <span className="muted small">not heard</span>
                </label>
              );
            })}
            {options && options.ssids.length === 0 && options.aps.length === 0 && (
              <p className="muted small">Measure some points or place access points first.</p>
            )}
          </div>
        </div>

        <label className="checkbox">
          <input type="checkbox" checked={draft.isDefault} onChange={(e) => setDraft({ ...draft, isDefault: e.target.checked })} />
          Project default (used by floors without their own choice)
        </label>

        <div className="panel-actions">
          <button className="btn btn-primary" type="submit" disabled={busy || !draft.name.trim()}>
            {editId === NEW ? "Create profile" : "Save"}
          </button>
          {editId !== NEW && (
            <ConfirmButton onConfirm={() => void remove()} title="Floors using it fall back to the project default">
              Delete
            </ConfirmButton>
          )}
        </div>
      </form>
    </section>
  );
}
