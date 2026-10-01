import { useState, type FormEvent } from "react";
import { ConfirmButton } from "../ConfirmButton";

export interface PickerItem {
  id: number;
  label: string;
}

interface ExtraField {
  placeholder: string;
  type: "text" | "number";
  initial: string;
  className?: string;
}

/** One level of the project › building › floor breadcrumb: select, add, delete. */
export function EntityPicker({
  label,
  items,
  selectedId,
  onSelect,
  onCreate,
  onDelete,
  deleteTitle,
  placeholder,
  extra,
  disabled,
}: {
  label: string;
  items: PickerItem[];
  selectedId: number | null;
  onSelect: (id: number) => void;
  onCreate: (name: string, extra: string) => Promise<boolean>;
  onDelete: (id: number) => void;
  /** What deleting the selected item also removes. */
  deleteTitle?: string;
  placeholder: string;
  extra?: ExtraField;
  disabled?: boolean;
}) {
  const [creating, setCreating] = useState(false);
  const [name, setName] = useState("");
  const [extraValue, setExtraValue] = useState("");
  const [busy, setBusy] = useState(false);

  const start = () => {
    setName("");
    setExtraValue(extra?.initial ?? "");
    setCreating(true);
  };

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!name.trim()) return;
    setBusy(true);
    const ok = await onCreate(name.trim(), extraValue.trim());
    setBusy(false);
    if (ok) setCreating(false);
  };

  return (
    <div className="picker">
      <label className="field-label">{label}</label>
      {creating ? (
        <form className="picker-row" onSubmit={submit} onKeyDown={(e) => e.key === "Escape" && setCreating(false)}>
          <input
            className="input"
            autoFocus
            placeholder={placeholder}
            value={name}
            onChange={(e) => setName(e.target.value)}
          />
          {extra && (
            <input
              className={`input ${extra.className ?? ""}`}
              type={extra.type}
              placeholder={extra.placeholder}
              title={extra.placeholder}
              value={extraValue}
              onChange={(e) => setExtraValue(e.target.value)}
            />
          )}
          <button className="btn" type="submit" disabled={busy || !name.trim()}>
            Add
          </button>
          <button className="btn" type="button" onClick={() => setCreating(false)}>
            Cancel
          </button>
        </form>
      ) : (
        <div className="picker-row">
          {items.length > 0 ? (
            <select
              className="input"
              value={selectedId ?? ""}
              disabled={disabled}
              onChange={(e) => onSelect(Number(e.target.value))}
            >
              {selectedId == null && <option value="">Select…</option>}
              {items.map((i) => (
                <option key={i.id} value={i.id}>
                  {i.label}
                </option>
              ))}
            </select>
          ) : (
            <span className="picker-empty muted">None yet</span>
          )}
          <button
            type="button"
            className="btn btn-square"
            title={`Add ${label.toLowerCase()}`}
            onClick={start}
            disabled={disabled}
          >
            +
          </button>
          {selectedId != null && (
            <ConfirmButton
              title={deleteTitle}
              onConfirm={() => onDelete(selectedId)}
              disabled={disabled}
              confirmLabel="Confirm"
            >
              Delete
            </ConfirmButton>
          )}
        </div>
      )}
    </div>
  );
}
