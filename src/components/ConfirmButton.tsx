import { useEffect, useState, type ReactNode } from "react";

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
          setArmed(false);
          onConfirm();
        } else {
          setArmed(true);
        }
      }}
    >
      {armed ? confirmLabel : children}
    </button>
  );
}
