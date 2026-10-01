import { useEffect, useState, type FormEvent } from "react";
import { api, asApiError } from "../api/tauri";
import type { Project } from "../types/project";
import type { ApiError } from "../types/wifi";
import { ErrorBanner } from "../components/ErrorBanner";
import { formatDateTime } from "../lib/format";

export function Survey() {
  const [projects, setProjects] = useState<Project[]>([]);
  const [error, setError] = useState<ApiError | null>(null);
  const [name, setName] = useState("");
  const [customer, setCustomer] = useState("");
  const [busy, setBusy] = useState(false);

  const load = async () => {
    try {
      setProjects(await api.listProjects());
      setError(null);
    } catch (e) {
      setError(asApiError(e));
    }
  };

  useEffect(() => {
    void load();
  }, []);

  const create = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    try {
      await api.createProject({ name, customerName: customer || null });
      setName("");
      setCustomer("");
      await load();
    } catch (err) {
      setError(asApiError(err));
    } finally {
      setBusy(false);
    }
  };

  const remove = async (id: number) => {
    try {
      await api.deleteProject(id);
      await load();
    } catch (err) {
      setError(asApiError(err));
    }
  };

  return (
    <div className="page">
      <div className="page-header">
        <h1>Survey projects</h1>
        <p className="muted">
          Buildings, floor plans and “Measure Here” arrive in the next phase. Projects are stored locally in SQLite.
        </p>
      </div>
      {error && <ErrorBanner error={error} />}
      <section className="card">
        <form className="inline-form" onSubmit={create}>
          <input className="input" placeholder="Project name" value={name} onChange={(e) => setName(e.target.value)} required />
          <input className="input" placeholder="Customer (optional)" value={customer} onChange={(e) => setCustomer(e.target.value)} />
          <button className="btn btn-primary" type="submit" disabled={busy || !name.trim()}>Create project</button>
        </form>
      </section>
      <section className="card card-table">
        <table className="data-table">
          <thead>
            <tr><th>Name</th><th>Customer</th><th>Created</th><th>Updated</th><th /></tr>
          </thead>
          <tbody>
            {projects.length === 0 && <tr><td colSpan={5} className="empty">No projects yet.</td></tr>}
            {projects.map((p) => (
              <tr key={p.id}>
                <td>{p.name}</td>
                <td>{p.customerName ?? <span className="muted">—</span>}</td>
                <td className="mono">{formatDateTime(p.createdAt)}</td>
                <td className="mono">{formatDateTime(p.updatedAt)}</td>
                <td className="num"><DeleteButton onConfirm={() => void remove(p.id)} /></td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>
    </div>
  );
}

function DeleteButton({ onConfirm }: { onConfirm: () => void }) {
  const [armed, setArmed] = useState(false);
  useEffect(() => {
    if (!armed) return;
    const t = window.setTimeout(() => setArmed(false), 3000);
    return () => window.clearTimeout(t);
  }, [armed]);
  return (
    <button type="button" className={`btn btn-small ${armed ? "btn-danger" : ""}`} onClick={() => (armed ? onConfirm() : setArmed(true))}>
      {armed ? "Confirm delete" : "Delete"}
    </button>
  );
}
