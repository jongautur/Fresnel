import { useEffect, useState } from "react";
import { api, asApiError } from "../../api/tauri";
import type { ApiError } from "../../types/wifi";
import type { FloorRequirements, Outcome, PointEvaluation, RequirementProfile } from "../../types/requirements";
import { STATUS_CRITICAL, STATUS_GOOD } from "../../lib/heatmap";
import { describeValues, formatShare, ruleLabel } from "../../lib/requirements";
import { RequirementProfileEditor } from "./RequirementProfileEditor";

const OUTCOME_LABEL: Record<Outcome, string> = { pass: "Pass", fail: "Fail", not_evaluated: "Not evaluated" };
const OUTCOME_MARK: Record<Outcome, string> = { pass: "✓", fail: "✗", not_evaluated: "–" };

export function OutcomeBadge({ outcome }: { outcome: Outcome }) {
  return (
    <span className={`req-badge req-${outcome}`}>
      {OUTCOME_MARK[outcome]} {OUTCOME_LABEL[outcome]}
    </span>
  );
}

/** Per-point result with the reasons, shown in the point details. */
export function PointRequirements({ evaluation, profileName }: { evaluation: PointEvaluation; profileName: string }) {
  return (
    <div className="panel-section req-point">
      <div className="req-point-head">
        <span className="field-label">Requirements · {profileName}</span>
        <OutcomeBadge outcome={evaluation.outcome} />
      </div>
      {evaluation.reason && <p className="muted small">{evaluation.reason}</p>}
      {evaluation.rules.length > 0 && (
        <ul className="req-rules">
          {evaluation.rules.map((r, i) => (
            <li key={i} className={`req-${r.outcome}`} title={r.heuristic ? "APs told apart by BSSID similarity (heuristic)" : undefined}>
              <span className="req-mark">{OUTCOME_MARK[r.outcome]}</span>
              <span className="req-rule">{ruleLabel(r)}</span>
              <span className="muted small">
                {r.detail}
                {r.limit != null && r.outcome !== "not_evaluated" && ` (limit ${r.limit})`}
                {r.heuristic && " · heuristic"}
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** Legend for the Requirements heatmap view. */
export function RequirementsLegend({
  profile,
  areaFraction,
  pointFraction,
}: {
  profile: RequirementProfile;
  areaFraction: number | null;
  pointFraction: number | null;
}) {
  return (
    <div className="heat-legend">
      <div className="heat-stat">
        <strong>{areaFraction != null ? formatShare(areaFraction) : "—"}</strong>{" "}
        <span className="muted">of the mapped area meets “{profile.name}” (estimate)</span>
      </div>
      {pointFraction != null && (
        <div className="muted small">{formatShare(pointFraction)} of evaluated points pass (measured).</div>
      )}
      <div className="heat-swatches">
        <span>
          <i style={{ background: STATUS_GOOD }} /> ✓ meets the profile
        </span>
        <span>
          <i style={{ background: STATUS_CRITICAL }} /> ✗ fails a rule
        </span>
      </div>
      <div className="heat-legend-note muted small">
        {describeValues(profile).join(" · ")}. Signal levels are interpolated between points; co-channel counts are
        rounded; band, SNR and load rules come from the nearest point. Points not evaluated are left out.
      </div>
    </div>
  );
}

/**
 * The floor's requirement profile (project default or an override), its
 * summary over points and mapped area, and the project's profile editor.
 */
export function RequirementsPanel({
  requirements,
  areaFraction,
  onRequirements,
  onProfilesChanged,
}: {
  requirements: FloorRequirements | null;
  /** Share of mapped area meeting the profile (IDW estimate), when computed. */
  areaFraction: number | null;
  /** The floor re-evaluated after changing its override. */
  onRequirements: (r: FloorRequirements) => void;
  /** Profiles were edited: re-evaluate. */
  onProfilesChanged: () => void;
}) {
  const [profiles, setProfiles] = useState<RequirementProfile[]>([]);
  const [editing, setEditing] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const [rev, setRev] = useState(0);
  const projectId = requirements?.projectId;

  useEffect(() => {
    if (projectId == null) return;
    let cancelled = false;
    api.listRequirementProfiles(projectId).then(
      (list) => !cancelled && setProfiles(list),
      (e) => !cancelled && setError(asApiError(e)),
    );
    return () => {
      cancelled = true;
    };
  }, [projectId, rev]);

  if (!requirements) return null;

  if (editing) {
    return (
      <RequirementProfileEditor
        projectId={requirements.projectId}
        initialId={requirements.profile?.id ?? null}
        onChanged={() => {
          setRev((n) => n + 1);
          onProfilesChanged();
        }}
        onClose={() => setEditing(false)}
      />
    );
  }

  const setOverride = async (profileId: number | null) => {
    try {
      onRequirements(await api.setFloorRequirementProfile(requirements.floorId, profileId));
      setError(null);
    } catch (e) {
      setError(asApiError(e));
    }
  };

  const { profile, summary } = requirements;
  const projectDefault = profiles.find((p) => p.isDefault);
  const evaluated = summary ? summary.passed + summary.failed : 0;

  return (
    <section className="card">
      <header className="card-header">
        <h2>Requirements</h2>
        <button type="button" className="btn btn-small" onClick={() => setEditing(true)}>
          {profiles.length ? "Edit profiles" : "Set up"}
        </button>
      </header>
      {error && <div className="banner banner-error banner-compact">{error.message}</div>}
      <div className="panel-section">
        <label>
          <span className="field-label">Profile for this floor</span>
          <select
            className="input req-wide"
            value={requirements.overrideProfileId ?? ""}
            onChange={(e) => void setOverride(e.target.value === "" ? null : Number(e.target.value))}
            disabled={profiles.length === 0}
          >
            <option value="">Project default{projectDefault ? ` (${projectDefault.name})` : " (none set)"}</option>
            {profiles.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </select>
        </label>

        {!profile ? (
          <p className="muted small">
            No requirement profile applies. A profile says what “good” means here (signal, a second AP for roaming,
            co-channel, bands) and marks each point pass or fail.
          </p>
        ) : (
          <>
            <p className="muted small">
              {describeValues(profile).join(" · ")}.{" "}
              {requirements.source === "floor_override" ? "Chosen for this floor." : "Project default."} The coverage and
              AP-overlap heatmaps start at these levels.
            </p>
            {profile.targets.length === 0 ? (
              <p className="small">
                The profile has no target networks yet: choose SSIDs or access points under <em>Edit profiles</em>.
              </p>
            ) : (
              <p className="small">
                <span className="muted">Applies to </span>
                {profile.targets.map((t) => t.label || "(unnamed)").join(", ")}
              </p>
            )}
            {summary && summary.points > 0 && (
              <div className="req-summary">
                <div>
                  <strong>
                    {summary.passFractionOfPoints != null ? formatShare(summary.passFractionOfPoints) : "—"}
                  </strong>{" "}
                  <span className="muted">
                    of points pass: {summary.passed} of {evaluated} evaluated
                    {summary.notEvaluated > 0 && `, ${summary.notEvaluated} not evaluated`}
                  </span>
                </div>
                <div>
                  <strong>{areaFraction != null ? formatShare(areaFraction) : "—"}</strong>{" "}
                  <span className="muted">of the mapped area (IDW estimate between points)</span>
                </div>
                {summary.failuresByRule.length > 0 && (
                  <div className="small">
                    <span className="muted">Failing: </span>
                    {summary.failuresByRule.map((f) => `${ruleLabel(f)} ${f.count}`).join(" · ")}
                  </div>
                )}
                {summary.rulesNotEvaluated.length > 0 && (
                  <div className="muted small">
                    Not judged at some points: {summary.rulesNotEvaluated.map((f) => `${ruleLabel(f)} ${f.count}`).join(" · ")}{" "}
                    (see point details)
                  </div>
                )}
                {summary.notEvaluatedReasons.map((r) => (
                  <div key={r.reason} className="muted small">
                    {r.count} point{r.count === 1 ? "" : "s"} not evaluated: {r.reason}
                  </div>
                ))}
                {summary.adapters.length > 1 && (
                  <div className="small req-caveat">
                    Measured with {summary.adapters.length} adapters: cards read differently, so results aren't
                    directly comparable.
                  </div>
                )}
                {summary.usesHeuristic && (
                  <div className="muted small">
                    Some APs were told apart by BSSID similarity (a heuristic). Link BSSIDs to placed APs to be sure.
                  </div>
                )}
                {summary.widthUnknownPoints > 0 && (
                  <div className="muted small">
                    {summary.widthUnknownPoints} point{summary.widthUnknownPoints === 1 ? "" : "s"}: the primary's channel
                    width is unknown, so co-channel was judged on its primary channel only.
                  </div>
                )}
              </div>
            )}
          </>
        )}
      </div>
    </section>
  );
}
