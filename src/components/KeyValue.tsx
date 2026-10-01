import type { ReactNode } from "react";

export function KeyValueGrid({ children }: { children: ReactNode }) {
  return <dl className="kv">{children}</dl>;
}

export function KV({ k, children, mono }: { k: string; children: ReactNode; mono?: boolean }) {
  const empty = children == null || children === "" || children === false;
  return (
    <>
      <dt>{k}</dt>
      <dd className={mono ? "mono" : undefined}>{empty ? <span className="muted">—</span> : children}</dd>
    </>
  );
}
