import { useState, type Dispatch, type FormEvent, type SetStateAction } from "react";
import { api, asApiError } from "../../api/tauri";
import type { ApiError } from "../../types/wifi";
import type { Floor } from "../../types/survey";
import { MAX_NOTE_CHARS, PIN_CATEGORIES, pinCategoryLabel, type NotePin } from "../../types/notes";
import { ErrorBanner } from "../ErrorBanner";
import { ConfirmButton } from "../ConfirmButton";
import { NotesField } from "./NotesField";
import { PhotoStrip } from "./PhotoStrip";
import type { PlanXY } from "./PlanCanvas";
import "../../styles/notes.css";

/** Create or edit one note pin: text, category, and (once saved) photos. */
function NotePinEditor({
  floorId,
  pin,
  position,
  onSaved,
  onDelete,
  onCancel,
}: {
  floorId: number;
  /** null = new pin at `position`. */
  pin: NotePin | null;
  position: PlanXY;
  onSaved: (pin: NotePin) => void;
  onDelete: () => void;
  onCancel: () => void;
}) {
  const [text, setText] = useState(pin?.text ?? "");
  const [category, setCategory] = useState(pin?.category ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<ApiError | null>(null);
  const categories =
    pin?.category && !PIN_CATEGORIES.some((c) => c.id === pin.category)
      ? [...PIN_CATEGORIES, { id: pin.category, label: pin.category }]
      : PIN_CATEGORIES;

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!text.trim()) return;
    setBusy(true);
    try {
      const input = { x: position.x, y: position.y, text: text.trim(), category: category || null };
      const saved = pin ? await api.updateNotePin(pin.id, input) : await api.createNotePin(floorId, input);
      setError(null);
      onSaved(saved);
    } catch (err) {
      setError(asApiError(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="card">
      <header className="card-header">
        <h2>{pin ? "Note" : "New note"}</h2>
        {pin && (
          <ConfirmButton onConfirm={onDelete} title="Delete this note and its photos">
            Delete
          </ConfirmButton>
        )}
      </header>
      <form className="panel-section" onSubmit={submit}>
        {error && <ErrorBanner error={error} compact />}
        <label>
          <span className="field-label">Note</span>
          <textarea
            className="input notes-textarea"
            rows={3}
            value={text}
            maxLength={MAX_NOTE_CHARS}
            placeholder="e.g. Metal cabinet, microwave, ceiling height 4 m"
            autoFocus={!pin}
            onChange={(e) => setText(e.target.value)}
          />
        </label>
        <label>
          <span className="field-label">Category (optional)</span>
          <select className="input ap-input" value={category} onChange={(e) => setCategory(e.target.value)}>
            <option value="">None</option>
            {categories.map((c) => (
              <option key={c.id} value={c.id}>
                {c.label}
              </option>
            ))}
          </select>
        </label>
        <div className="panel-actions">
          <button className="btn btn-primary" type="submit" disabled={busy || !text.trim()}>
            {pin ? "Save" : "Pin note"}
          </button>
          <button className="btn" type="button" onClick={onCancel}>
            {pin ? "Close" : "Cancel"}
          </button>
        </div>
      </form>
      <div className="panel-section">
        {pin ? (
          <PhotoStrip floorId={floorId} target={{ kind: "pin", id: pin.id }} />
        ) : (
          <p className="muted small">Photos can be added once the note is pinned.</p>
        )}
      </div>
    </section>
  );
}

/**
 * The Notes tool: the floor's own notes and photos, its note pins, and the
 * editor for the pin being placed or the one selected.
 */
export function NotesPanel({
  floor,
  pins,
  setPins,
  selectedPinId,
  setSelectedPinId,
  pendingPin,
  setPendingPin,
}: {
  floor: Floor;
  pins: NotePin[];
  setPins: Dispatch<SetStateAction<NotePin[]>>;
  selectedPinId: number | null;
  setSelectedPinId: (id: number | null) => void;
  pendingPin: PlanXY | null;
  setPendingPin: (p: PlanXY | null) => void;
}) {
  const [error, setError] = useState<ApiError | null>(null);
  const selected = pins.find((p) => p.id === selectedPinId) ?? null;

  const close = () => {
    setPendingPin(null);
    setSelectedPinId(null);
  };

  const remove = async (id: number) => {
    try {
      await api.deleteNotePin(id);
      setPins((ps) => ps.filter((p) => p.id !== id));
      close();
    } catch (e) {
      setError(asApiError(e));
    }
  };

  if (selected || pendingPin) {
    return (
      <>
        {error && <ErrorBanner error={error} />}
        <NotePinEditor
          key={selected ? `pin-${selected.id}` : `new-${pendingPin!.x}-${pendingPin!.y}`}
          floorId={floor.id}
          pin={selected}
          position={selected ? { x: selected.x, y: selected.y } : pendingPin!}
          onSaved={(saved) => {
            setPins((ps) => (ps.some((p) => p.id === saved.id) ? ps.map((p) => (p.id === saved.id ? saved : p)) : [...ps, saved]));
            setPendingPin(null);
            setSelectedPinId(saved.id);
          }}
          onDelete={() => selected && void remove(selected.id)}
          onCancel={close}
        />
      </>
    );
  }

  return (
    <>
      {error && <ErrorBanner error={error} />}
      <section className="card">
        <header className="card-header">
          <h2>Floor notes</h2>
        </header>
        <div className="panel-section">
          <NotesField target={{ kind: "floor", id: floor.id }} label={`About ${floor.name}`} rows={4} />
          <PhotoStrip floorId={floor.id} target={{ kind: "floor", id: floor.id }} />
        </div>
      </section>
      <section className="card">
        <header className="card-header">
          <h2>
            Pinned notes <span className="count">{pins.length}</span>
          </h2>
        </header>
        <div className="panel-section">
          <p>Click the plan to pin a note to a spot: a metal cabinet, a microwave, a ceiling height.</p>
        </div>
        {pins.length > 0 && (
          <ul className="ap-list">
            {pins.map((p) => (
              <li key={p.id}>
                <button type="button" onClick={() => setSelectedPinId(p.id)}>
                  <span className="pin-swatch" />
                  <span className="grow">{p.text}</span>
                  {p.category && <span className="muted small">{pinCategoryLabel(p.category)}</span>}
                </button>
              </li>
            ))}
          </ul>
        )}
      </section>
    </>
  );
}
