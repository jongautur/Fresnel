import { useEffect, useRef, useState } from "react";
import { api, asApiError } from "../../api/tauri";
import { MAX_NOTE_CHARS, type NoteTarget } from "../../types/notes";
import "../../styles/notes.css";

type Status = "loading" | "idle" | "dirty" | "saving" | "saved" | "error";

/**
 * Free-text note on a floor, point or AP. Saves when the field loses focus
 * and says whether the text on screen is stored.
 */
export function NotesField({
  target,
  initial,
  label = "Notes",
  placeholder = "What you saw here: obstacles, mounting, customer remarks…",
  rows = 3,
  onChange,
  onSaved,
}: {
  target: NoteTarget;
  /** The stored note if the caller has it; otherwise it is loaded. */
  initial?: string | null;
  label?: string;
  placeholder?: string;
  rows?: number;
  /** Every edit (for a parent form that also writes the note). */
  onChange?: (text: string) => void;
  /** After a successful save, with what the backend stored. */
  onSaved?: (text: string | null) => void;
}) {
  const key = `${target.kind}-${target.id}`;
  const [text, setText] = useState(initial ?? "");
  const [status, setStatus] = useState<Status>(initial === undefined ? "loading" : "idle");
  const [error, setError] = useState<string | null>(null);
  const stored = useRef(initial ?? "");
  // Saves in flight belong to the target they were started for.
  const current = useRef(key);
  current.current = key;

  useEffect(() => {
    setError(null);
    if (initial !== undefined) {
      stored.current = initial ?? "";
      setText(initial ?? "");
      setStatus("idle");
      return;
    }
    setStatus("loading");
    let cancelled = false;
    api
      .getNotes(target)
      .then((n) => {
        if (cancelled) return;
        stored.current = n ?? "";
        setText(n ?? "");
        setStatus("idle");
      })
      .catch((e) => {
        if (cancelled) return;
        setError(asApiError(e).message);
        setStatus("error");
      });
    return () => {
      cancelled = true;
    };
    // The note is (re)loaded per target, not on every parent render.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);

  const save = async () => {
    if (status === "loading" || text.trim() === stored.current.trim()) {
      if (status === "dirty") setStatus("idle");
      return;
    }
    const forKey = key;
    setStatus("saving");
    setError(null);
    try {
      const saved = await api.setNotes(target, text.trim() || null);
      if (current.current !== forKey) return;
      stored.current = saved ?? "";
      setText((t) => (t.trim() === (saved ?? "") ? (saved ?? "") : t));
      setStatus("saved");
      onSaved?.(saved);
    } catch (e) {
      if (current.current !== forKey) return;
      setError(asApiError(e).message);
      setStatus("error");
    }
  };

  const remaining = MAX_NOTE_CHARS - text.length;
  const statusText: Record<Status, string> = {
    loading: "Loading…",
    idle: "",
    dirty: "Not saved yet",
    saving: "Saving…",
    saved: "Saved",
    error: "Not saved",
  };

  return (
    <label className="notes-field">
      <span className="field-label notes-field-head">
        {label}
        <span className={`notes-status notes-status-${status}`} aria-live="polite">
          {statusText[status]}
        </span>
      </span>
      <textarea
        className="input notes-textarea"
        rows={rows}
        value={text}
        maxLength={MAX_NOTE_CHARS}
        placeholder={placeholder}
        disabled={status === "loading"}
        onChange={(e) => {
          setText(e.target.value);
          setStatus("dirty");
          onChange?.(e.target.value);
        }}
        onBlur={() => void save()}
      />
      {(error || remaining < 300) && (
        <span className={`small ${error ? "notes-error" : "muted"}`}>
          {error ?? `${remaining} characters left`}
        </span>
      )}
    </label>
  );
}
