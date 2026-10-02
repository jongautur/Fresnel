import { useCallback, useEffect, useState, type FormEvent } from "react";
import { api, asApiError } from "../api/tauri";
import type { Project } from "../types/project";
import type { Building, Floor } from "../types/survey";
import type { ApiError } from "../types/wifi";
import { ErrorBanner } from "../components/ErrorBanner";
import { ErrorBoundary } from "../components/ErrorBoundary";
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

/**
 * A child list remembers the parent it was loaded for. A list for another
 * parent counts as not loaded, so nothing is shown or measured against it.
 */
interface ChildList<T> {
  parentId: number;
  list: T[];
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
  const [buildings, setBuildings] = useState<ChildList<Building> | null>(null);
  const [floors, setFloors] = useState<ChildList<Floor> | null>(null);
  const [sel, setSelState] = useState<Selection>(loadSelection);
  const [error, setError] = useState<ApiError | null>(null);
  // Bumped to reload a level after a mutation.
  const [projectsRev, setProjectsRev] = useState(0);
  const [buildingsRev, setBuildingsRev] = useState(0);
  const [floorsRev, setFloorsRev] = useState(0);

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
  // Responses for a parent that is no longer selected are dropped: a late list
  // for project A must never pick a building or floor while B is selected.

  useEffect(() => {
    let cancelled = false;
    api.listProjects().then(
      (list) => {
        if (cancelled) return;
        setProjects(list);
        setSel((s) => ({ ...s, projectId: pickId(list, s.projectId) }));
      },
      (e) => {
        if (cancelled) return;
        setError(asApiError(e));
        setProjects((p) => p ?? []);
      },
    );
    return () => {
      cancelled = true;
    };
  }, [projectsRev, setSel]);

  const projectId = sel.projectId;
  useEffect(() => {
    setBuildings((b) => (b?.parentId === projectId ? b : null));
    if (projectId == null) {
      setSel((s) => ({ ...s, buildingId: null }));
      return;
    }
    let cancelled = false;
    api.listBuildings(projectId).then(
      (list) => {
        if (cancelled) return;
        setBuildings({ parentId: projectId, list });
        setSel((s) => (s.projectId === projectId ? { ...s, buildingId: pickId(list, s.buildingId) } : s));
      },
      (e) => !cancelled && setError(asApiError(e)),
    );
    return () => {
      cancelled = true;
    };
  }, [projectId, buildingsRev, setSel]);

  const buildingId = sel.buildingId;
  useEffect(() => {
    setFloors((f) => (f?.parentId === buildingId ? f : null));
    if (buildingId == null) {
      setSel((s) => ({ ...s, floorId: null }));
      return;
    }
    let cancelled = false;
    api.listFloors(buildingId).then(
      (list) => {
        if (cancelled) return;
        setFloors({ parentId: buildingId, list });
        setSel((s) => (s.buildingId === buildingId ? { ...s, floorId: pickId(list, s.floorId) } : s));
      },
      (e) => !cancelled && setError(asApiError(e)),
    );
    return () => {
      cancelled = true;
    };
  }, [buildingId, floorsRev, setSel]);

  // --- Mutations ------------------------------------------------------------
  // Select the result only if its parent is still selected, then reload.

  const createProject = async (name: string, customer: string) => {
    try {
      const p = await api.createProject({ name, customerName: customer || null });
      setError(null);
      setSel((s) => ({ ...s, projectId: p.id }));
      setProjectsRev((n) => n + 1);
      return true;
    } catch (e) {
      return fail(e);
    }
  };

  const createBuilding = async (name: string) => {
    if (projectId == null) return false;
    try {
      const b = await api.createBuilding({ projectId, name });
      setError(null);
      setSel((s) => (s.projectId === projectId ? { ...s, buildingId: b.id } : s));
      setBuildingsRev((n) => n + 1);
      return true;
    } catch (e) {
      return fail(e);
    }
  };

  const createFloor = async (name: string, levelText: string) => {
    if (buildingId == null) return false;
    const level = levelText === "" ? 0 : Number(levelText);
    if (!Number.isInteger(level)) return fail({ kind: "invalid_input", message: "Level must be a whole number" });
    try {
      const f = await api.createFloor({ buildingId, name, level });
      setError(null);
      setSel((s) => (s.buildingId === buildingId ? { ...s, floorId: f.id } : s));
      setFloorsRev((n) => n + 1);
      return true;
    } catch (e) {
      return fail(e);
    }
  };

  const remove = async (what: "project" | "building" | "floor", id: number) => {
    try {
      if (what === "project") {
        await api.deleteProject(id);
        setSel((s) => (s.projectId === id ? { ...s, projectId: null } : s));
        setProjectsRev((n) => n + 1);
      } else if (what === "building") {
        await api.deleteBuilding(id);
        setSel((s) => (s.buildingId === id ? { ...s, buildingId: null } : s));
        setBuildingsRev((n) => n + 1);
      } else {
        await api.deleteFloor(id);
        setSel((s) => (s.floorId === id ? { ...s, floorId: null } : s));
        setFloorsRev((n) => n + 1);
      }
      setError(null);
    } catch (e) {
      fail(e);
    }
  };

  const updateFloor = useCallback((id: number, update: (f: Floor) => Floor) => {
    setFloors((fl) => fl && { ...fl, list: fl.list.map((x) => (x.id === id ? update(x) : x)) });
  }, []);

  // --- Render ---------------------------------------------------------------

  const buildingList = buildings?.parentId === sel.projectId ? buildings.list : null;
  const building = buildingList?.find((b) => b.id === sel.buildingId) ?? null;
  const floorList = building && floors?.parentId === building.id ? floors.list : null;
  const floor = floorList?.find((f) => f.id === sel.floorId) ?? null;
  const nextLevel = floorList?.length ? Math.max(...floorList.map((f) => f.level)) + 1 : 0;

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
          items={(buildingList ?? []).map((b) => ({ id: b.id, label: b.name }))}
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
          items={(floorList ?? []).map((f) => ({
            id: f.id,
            label: `${f.name}${f.pointCount ? ` · ${f.pointCount} pts` : ""}`,
          }))}
          selectedId={sel.floorId}
          onSelect={(id) => setSel((s) => ({ ...s, floorId: id }))}
          onCreate={createFloor}
          onDelete={(id) => void remove("floor", id)}
          deleteTitle={`Deletes the floor, its plan and ${floor?.pointCount ?? 0} measured point(s)`}
          placeholder="Floor name"
          extra={{ placeholder: "Level", type: "number", initial: String(nextLevel), className: "input-level" }}
          disabled={building == null}
        />
      </nav>

      {error && <ErrorBanner error={error} />}

      {sel.projectId != null && buildingList?.length === 0 ? (
        <section className="card survey-empty">
          <h2>Add a building</h2>
          <p className="muted">Name the building you are surveying. You can add more later.</p>
          <QuickCreate placeholder="Building name" button="Add building" onCreate={createBuilding} />
        </section>
      ) : building && floorList?.length === 0 ? (
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
        <ErrorBoundary key={floor.id} title="The floor view stopped working">
          <FloorWorkspace floor={floor} onFloorChange={(update) => updateFloor(floor.id, update)} />
        </ErrorBoundary>
      ) : null}
    </div>
  );
}
