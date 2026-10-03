import { useCallback, useEffect, useState, type FormEvent } from "react";
import { api, asApiError } from "../../api/tauri";
import type { ApiError } from "../../types/wifi";
import type { TestSettings } from "../../types/pointTests";
import { TEST_SETTINGS_CHANGED } from "../../lib/pointTests";
import { navigate } from "../../lib/navigation";
import { ErrorBanner } from "../ErrorBanner";
import type {
  IpFamily,
  Iperf3Direction,
  Iperf3Directions,
  Iperf3Interval,
  Iperf3Params,
  Iperf3RunResults,
  ToolRun,
  Via,
} from "../../types/tools";
import { formatBps, loadForm, saveForm } from "../../lib/tools";
import { FamilyPicker, ViaPicker } from "./ViaPicker";
import { RunInfo } from "./RunInfo";
import { SeriesChart } from "./SeriesChart";
import { ToolHistory } from "./ToolHistory";
import { useToolRun } from "./useToolRun";

/** Per-run choices; the server and test lengths come from Settings. */
interface Form {
  family: IpFamily;
  via: Via;
  /** null: the direction set up in Settings. */
  directions: Iperf3Directions | null;
}

const DEFAULTS: Form = { family: "any", via: { mode: "system" }, directions: null };

const DIRECTION_LABEL: Record<Iperf3Direction, string> = { upload: "Upload", download: "Download" };
const DIRECTION_COLOR: Record<Iperf3Direction, string> = {
  upload: "var(--viz-connected)",
  download: "var(--viz-highlight)",
};

type Live = Record<Iperf3Direction, Iperf3Interval[]>;

export function Iperf3Tool() {
  const [form, setFormState] = useState<Form>(() => loadForm("iperf3", DEFAULTS));
  const setForm = (patch: Partial<Form>) =>
    setFormState((f) => {
      const next = { ...f, ...patch };
      saveForm("iperf3", next);
      return next;
    });
  const [settings, setSettings] = useState<TestSettings | null>(null);
  const [settingsError, setSettingsError] = useState<ApiError | null>(null);
  useEffect(() => {
    const load = () =>
      api.getTestSettings().then(
        (s) => {
          setSettings(s);
          setSettingsError(null);
        },
        (e) => setSettingsError(asApiError(e)),
      );
    void load();
    window.addEventListener(TEST_SETTINGS_CHANGED, load);
    return () => window.removeEventListener(TEST_SETTINGS_CHANGED, load);
  }, []);
  const directionsNow = form.directions ?? settings?.iperf3Directions ?? "both";
  const [live, setLive] = useState<Live>({ upload: [], download: [] });
  const [current, setCurrent] = useState<Iperf3Direction | null>(null);
  const [refresh, setRefresh] = useState(0);
  const onStored = useCallback(() => setRefresh((n) => n + 1), []);
  const { running, stopping, error, run, started, start, stop, show } = useToolRun("iperf3", onStored);
  const stored = run?.results as Iperf3RunResults | null | undefined;

  const intervals: Live =
    !running && stored
      ? {
          upload: stored.tests.find((t) => t.direction === "upload")?.result?.intervals ??
            stored.tests.find((t) => t.direction === "upload")?.intervals ?? [],
          download: stored.tests.find((t) => t.direction === "download")?.result?.intervals ??
            stored.tests.find((t) => t.direction === "download")?.intervals ?? [],
        }
      : live;
  const directions = (["upload", "download"] as const).filter((d) => intervals[d].length > 0);
  const length = Math.max(0, ...directions.map((d) => intervals[d].length));

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (running || !settings?.iperf3Server) return;
    const params: Iperf3Params = {
      server: settings.iperf3Server,
      port: settings.iperf3Port,
      family: form.family,
      via: form.via,
      streams: settings.iperf3Streams,
      durationS: settings.iperf3DurationS,
      omitS: settings.iperf3OmitS,
      directions: directionsNow,
    };
    setLive({ upload: [], download: [] });
    setCurrent(null);
    void start(
      (id, onEvent) => api.toolIperf3(id, params, onEvent),
      (ev) => {
        if (ev.event === "iperf3_test") setCurrent(ev.direction);
        if (ev.event === "interval") {
          const { event: _event, direction, ...interval } = ev;
          setLive((l) => ({ ...l, [direction]: [...l[direction], interval] }));
        }
      },
    );
  };

  // The server and lengths come from Settings; a past run sets the per-run choices.
  const again = (r: ToolRun) => {
    const p = r.params as Partial<Iperf3Params>;
    setForm({ family: p.family ?? form.family, via: p.via ?? form.via, directions: p.directions ?? form.directions });
  };
  const server = settings?.iperf3Server ?? null;
  const setUp = () => navigate("settings", "iperf3-settings");

  return (
    <>
      <section className="card">
        <form className="tool-form" onSubmit={submit}>
          <div className="tool-field tool-target">
            <span className="field-label">Server</span>
            {server && settings ? (
              <div className="iperf3-setup">
                <span className="mono">
                  {server}:{settings.iperf3Port}
                </span>
                <span className="muted small">
                  {settings.iperf3Streams} stream{settings.iperf3Streams === 1 ? "" : "s"} · {settings.iperf3DurationS} s after{" "}
                  {settings.iperf3OmitS} s omitted
                </span>
                <button type="button" className="btn btn-small" onClick={setUp}>
                  Change in Settings
                </button>
              </div>
            ) : (
              <div className="iperf3-setup">
                <span className="muted">{settings ? "No iperf3 server set up yet." : "Loading…"}</span>
                <button type="button" className="btn btn-small btn-primary" onClick={setUp}>
                  Set up in Settings
                </button>
              </div>
            )}
          </div>
          <ViaPicker value={form.via} onChange={(via) => setForm({ via })} disabled={running} />
          <FamilyPicker value={form.family} onChange={(family) => setForm({ family })} disabled={running} />
          {running ? (
            <button type="button" className="btn btn-danger" onClick={stop} disabled={stopping}>
              {stopping ? "Stopping…" : "Stop"}
            </button>
          ) : (
            <button type="submit" className="btn btn-primary" disabled={!server}>
              Start
            </button>
          )}
          <div className="tool-options">
            <div className="segmented">
              {(["upload", "download", "both"] as const).map((d) => (
                <button key={d} type="button" className={directionsNow === d ? "active" : ""} disabled={running} onClick={() => setForm({ directions: d })}>
                  {d === "both" ? "Both" : DIRECTION_LABEL[d]}
                </button>
              ))}
            </div>
            <span className="muted small">
              Upload: this computer sends. Download: the server sends (<span className="mono">-R</span>). Both: one after
              the other.
            </span>
          </div>
          {settingsError && <ErrorBanner error={settingsError} compact />}
        </form>
        <div className="pad">
          <RunInfo started={started} run={run} error={error} running={running} />
        </div>
      </section>

      {(running || stored) && (
        <section className="card">
          <header className="card-header">
            <h2>Throughput</h2>
            {running && current && <span className="muted small">{DIRECTION_LABEL[current]} running…</span>}
          </header>
          {!running && stored && (
            <div className="stat-row">
              {stored.tests.map((t) => (
                <div key={t.direction} className="stat">
                  <div className="stat-label">
                    <span className="series-swatch" style={{ background: DIRECTION_COLOR[t.direction] }} /> {DIRECTION_LABEL[t.direction]}
                  </div>
                  <div className={`stat-value mono ${t.result ? "" : "stat-bad"}`}>
                    {t.result ? formatBps(t.result.bitsPerSecond) : t.stopped ? "stopped" : "failed"}
                  </div>
                  {t.result && (
                    <div className="muted small">
                      {t.result.durationS} s × {t.result.streams} streams · counted by{" "}
                      {t.result.measuredBy === "server" ? "the server" : "this computer"}
                      {t.result.retransmits != null && ` · ${t.result.retransmits} retransmits`}
                    </div>
                  )}
                  {t.error && <div className="small status-bad">{t.error}</div>}
                  {t.errorHint && <div className="muted small">{t.errorHint}</div>}
                </div>
              ))}
            </div>
          )}
          {length > 0 && (
            <div className="pad">
              <SeriesChart
                series={directions.map((d) => ({
                  label: DIRECTION_LABEL[d],
                  color: DIRECTION_COLOR[d],
                  values: intervals[d].map((i) => i.bitsPerSecond),
                }))}
                formatY={formatBps}
                formatX={(i) => `${i + 1} s`}
                xLabel="Second"
              />
              <p className="muted small">
                Per second as this computer counted it (upload: what TCP accepted from us). The averages above are the
                receiver's count after the omitted seconds.
              </p>
            </div>
          )}
        </section>
      )}

      <ToolHistory kind="iperf3" refreshKey={refresh} selectedId={run?.id ?? null} onOpen={show} onAgain={again} />
    </>
  );
}
