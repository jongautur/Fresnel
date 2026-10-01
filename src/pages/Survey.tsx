import { useCallback, useEffect, useState, type FormEvent } from "react";
import { api, asApiError } from "../api/tauri";
import type { Project } from "../types/project";
import type { Building, Floor } from "../types/survey";
import type { ApiError } from "../types/wifi";
import { ErrorBanner } from "../components/ErrorBanner";
import { EntityPicker } from "../components/survey/SurveyNav";
import { FloorWorkspace } from "../components/survey/FloorWorkspace";

const SELECTION_KEY = "fresnel.survey.selection";

interface Selection {
  projectId: number | null;
  buildingId: number | null;
  floorId: number | null;
}

function loadSelection(): Selection {
  try {
    const s = JSON.parse(localStorage.getItem(SELECTION_KEY) ?? "null") as Partial<Selection> | null;
    if (s) return { projectId: s.projectId ?? null, buildingId: s.buildingId ?? null, floorId: s.floorId ?? null };
  } catch {
    /* ignore */
  }
  return { projectId: null, buildingId: null, floorId: null };
}

/** Keep `wanted` if it's still in the list, else fall back to the first item. */
const pickId = (items: { id: number }[], wanted: number | null) =>
  items.some((i) => i.id === wanted) ? wanted : (items[0]?.id ?? null);

function QuickCreate({
  placeholder,
  button,
  onCreate,
  secondary,
}: {
  placeholder: string;
  button: string;
  onCreate: (name: string, secondary: string) => Promise<boolean>;
  secondary?: string;
}) {
  const [name, setName] = useState("");
  const [second, setSecond] = useState("");
  const [busy, setBusy] = useState(false);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    if (await onCreate(name.trim(), second.trim())) {
      setName("");
      setSecond("");
    }
    setBusy(false);
  };
  return (
    <form className="inline-form" onSubmit={submit}>
      <input className="input" autoFocus placeholder={placeholder} value={name} onChange={(e) => setName(e.target.value)} />
      {secondary && (
        <input className="input" placeholder={secondary} value={second} onChange={(e) => setSecond(e.target.value)} />
      )}
      <button className="btn btn-primary" type="submit" disabled={busy || !name.trim()}>
        {button}
      </button>
    </form>
  );
}

export function Survey() {
  const [projects, setProjects] = useState<Project[] | null>(null);
  const [buildings, setBuildings] = useState<Building[]>([]);
  const [floors, setFloors] = useState<Floor[]>([]);
  const [sel, setSelState] = useState<Selection>(loadSelection);
  const [error, setError] = useState<ApiError | null>(null);

  const setSel = useCallback((update: (s: Selection) => Selection) => {
    setSelState((prev) => {
      const next = update(prev);
      try {
        localStorage.setItem(SELECTION_KEY, JSON.stringify(next));
      } catch {
        /* non-essential */
      }
      return next;
    });
  }, []);

  const fail = (e: unknown) => {
    setError(asApiError(e));
    return false;
  };

  // --- Loading (each level reloads when its parent selection changes) ------

  const loadProjects = useCallback(async (select?: number | null) => {
    try {
      const list = await api.listProjects();
      setProjects(list);
      setSel((s) => ({ ...s, projectId: pickId(list, select !== undefined ? select : s.projectId) }));
    } catch (e) {
      setError(asApiError(e));
      setProjects([]);
    }
  }, [setSel]);

  const loadBuildings = useCallback(
    async (projectId: number | null, select?: number | null) => {
      if (projectId == null) {
        setBuildings([]);
        setSel((s) => ({ ...s, buildingId: null }));
        return;
      }
      try {
        const list = await api.listBuildings(projectId);
        setBuildings(list);
        setSel((s) => ({ ...s, buildingId: pickId(list, select !== undefined ? select : s.buildingId) }));
      } catch (e) {
        setError(asApiError(e));
      }
    },
    [setSel],
  );

  const loadFloors = useCallback(
    async (buildingId: number | null, select?: number | null) => {
      if (buildingId == null) {
        setFloors([]);
        setSel((s) => ({ ...s, floorId: null }));
        return;
      }
      try {
        const list = await api.listFloors(buildingId);
        setFloors(list);
        setSel((s) => ({ ...s, floorId: pickId(list, select !== undefined ? select : s.floorId) }));
      } catch (e) {
        setError(asApiError(e));
      }
    },
    [setSel],
  );

  useEffect(() => {
    void loadProjects();
  }, [loadProjects]);
  useEffect(() => {
    void loadBuildings(sel.projectId);
  }, [sel.projectId, loadBuildings]);
  useEffect(() => {
    void loadFloors(sel.buildingId);
  }, [sel.buildingId, loadFloors]);

  // --- Mutations ------------------------------------------------------------

  const createProject = async (name: string, customer: string) => {
    try {
      const p = await api.createProject({ name, customerName: customer || null });
      setError(null);
      await loadProjects(p.id);
      return true;
    } catch (e) {
      return fail(e);
    }
  };

  const createBuilding = async (name: string) => {
    if (sel.projectId == null) return false;
    try {
      const b = await api.createBuilding({ projectId: sel.projectId, name });
      setError(null);
      await loadBuildings(sel.projectId, b.id);
      return true;
    } catch (e) {
      return fail(e);
    }
  };

  const createFloor = async (name: string, levelText: string) => {
    if (sel.buildingId == null) return false;
    const level = levelText === "" ? 0 : Number(levelText);
    if (!Number.isInteger(level)) return fail({ kind: "invalid_input", message: "Level must be a whole number" });
    try {
      const f = await api.createFloor({ buildingId: sel.buildingId, name, level });
      setError(null);
      await loadFloors(sel.buildingId, f.id);
      return true;
    } catch (e) {
      return fail(e);
    }
  };

  const remove = async (what: "project" | "building" | "floor", id: number) => {
    try {
      if (what === "project") {
        await api.deleteProject(id);
        await loadProjects(null);
      } else if (what === "building") {
        await api.deleteBuilding(id);
        await loadBuildings(sel.projectId, null);
      } else {
        await api.deleteFloor(id);
        await loadFloors(sel.buildingId, null);
      }
      setError(null);
    } catch (e) {
      fail(e);
    }
  };

  const onFloorChange = (f: Floor) => setFloors((fs) => fs.map((x) => (x.id === f.id ? f : x)));

  // --- Render ---------------------------------------------------------------

  const floor = floors.find((f) => f.id === sel.floorId) ?? null;
  const nextLevel = floors.length ? Math.max(...floors.map((f) => f.level)) + 1 : 0;

  if (projects === null) return <div className="page" />;

  if (projects.length === 0) {
    return (
      <div className="page">
        {error && <ErrorBanner error={error} />}
        <section className="card survey-empty">
          <h2>Start a survey</h2>
          <p className="muted">
            A project holds buildings, each building holds floors, and each floor has a plan you measure on. Everything
            is stored locally.
          </p>
          <QuickCreate placeholder="Project name" secondary="Customer (optional)" button="Create project" onCreate={createProject} />
        </section>
      </div>
    );
  }

  return (
    <div className="page page-fill">
      <nav className="survey-nav" aria-label="Survey location">
        <EntityPicker
          label="Project"
          items={projects.map((p) => ({ id: p.id, label: p.customerName ? `${p.name} — ${p.customerName}` : p.name }))}
          selectedId={sel.projectId}
          onSelect={(id) => setSel((s) => ({ ...s, projectId: id }))}
          onCreate={createProject}
          onDelete={(id) => void remove("project", id)}
          deleteTitle="Deletes the project with all its buildings, floors, plans and measurements"
          placeholder="Project name"
          extra={{ placeholder: "Customer (optional)", type: "text", initial: "" }}
        />
        <span className="crumb-sep">›</span>
        <EntityPicker
          label="Building"
          items={buildings.map((b) => ({ id: b.id, label: b.name }))}
          selectedId={sel.buildingId}
          onSelect={(id) => setSel((s) => ({ ...s, buildingId: id }))}
          onCreate={createBuilding}
          onDelete={(id) => void remove("building", id)}
          deleteTitle="Deletes the building with all its floors, plans and measurements"
          placeholder="Building name"
          disabled={sel.projectId == null}
        />
        <span className="crumb-sep">›</span>
        <EntityPicker
          label="Floor"
          items={floors.map((f) => ({ id: f.id, label: `${f.name}${f.pointCount ? ` · ${f.pointCount} pts` : ""}` }))}
          selectedId={sel.floorId}
          onSelect={(id) => setSel((s) => ({ ...s, floorId: id }))}
          onCreate={createFloor}
          onDelete={(id) => void remove("floor", id)}
          deleteTitle={`Deletes the floor, its plan and ${floor?.pointCount ?? 0} measured point(s)`}
          placeholder="Floor name"
          extra={{ placeholder: "Level", type: "number", initial: String(nextLevel), className: "input-level" }}
          disabled={sel.buildingId == null}
        />
      </nav>

      {error && <ErrorBanner error={error} />}

      {sel.projectId != null && buildings.length === 0 ? (
        <section className="card survey-empty">
          <h2>Add a building</h2>
          <p className="muted">Name the building you are surveying. You can add more later.</p>
          <QuickCreate placeholder="Building name" button="Add building" onCreate={createBuilding} />
        </section>
      ) : sel.buildingId != null && floors.length === 0 ? (
        <section className="card survey-empty">
          <h2>Add a floor</h2>
          <p className="muted">Each floor gets its own plan and measurements.</p>
          <QuickCreate
            placeholder="Floor name, e.g. Ground floor"
            button="Add floor"
            onCreate={(name) => createFloor(name, String(nextLevel))}
          />
        </section>
      ) : floor ? (
        <FloorWorkspace key={floor.id} floor={floor} onFloorChange={onFloorChange} />
      ) : null}
    </div>
  );
}
