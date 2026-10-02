import { useEffect, useRef, useState, type ReactNode } from "react";

/** A confirm this soon after arming is the second half of a double-click, not a decision. */
const CONFIRM_DELAY_MS = 400;

/** Two-step destructive button: first click arms it, second confirms. Disarms after 3 s. */
export function ConfirmButton({
  onConfirm,
  children = "Delete",
  confirmLabel = "Confirm delete",
  title,
  disabled,
  small = true,
}: {
  onConfirm: () => void;
  children?: ReactNode;
  confirmLabel?: ReactNode;
  title?: string;
  disabled?: boolean;
  small?: boolean;
}) {
  const [armed, setArmed] = useState(false);
  const armedAt = useRef(0);
  useEffect(() => {
    if (!armed) return;
    const t = window.setTimeout(() => setArmed(false), 3000);
    return () => window.clearTimeout(t);
  }, [armed]);
  return (
    <button
      type="button"
      title={title}
      disabled={disabled}
      className={`btn ${small ? "btn-small" : ""} ${armed ? "btn-danger" : ""}`}
      onClick={() => {
        if (armed) {
          if (performance.now() - armedAt.current < CONFIRM_DELAY_MS) return;
          setArmed(false);
          onConfirm();
        } else {
          armedAt.current = performance.now();
          setArmed(true);
        }
      }}
    >
      {armed ? confirmLabel : children}
    </button>
  );
}
