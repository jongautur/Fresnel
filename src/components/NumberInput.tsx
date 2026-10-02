import { useState } from "react";

/**
 * Whole-number field that keeps what is being typed: in-range values apply
 * at once, anything else is clamped on blur or Enter (so typing "30" doesn't
 * get stuck on a clamped "3").
 */
export function NumberInput({
  value,
  min,
  max,
  onCommit,
  className = "input input-narrow",
}: {
  value: number;
  min: number;
  max: number;
  onCommit: (v: number) => void;
  className?: string;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  const commit = () => {
    if (draft === null) return;
    setDraft(null);
    const v = Number(draft.trim());
    if (draft.trim() !== "" && Number.isFinite(v)) onCommit(Math.min(max, Math.max(min, Math.round(v))));
  };
  return (
    <input
      className={className}
      type="number"
      min={min}
      max={max}
      step={1}
      value={draft ?? String(value)}
      onChange={(e) => {
        const text = e.target.value;
        setDraft(text);
        const v = Number(text);
        if (text.trim() !== "" && Number.isInteger(v) && v >= min && v <= max) onCommit(v);
      }}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          commit();
        }
      }}
    />
  );
}
