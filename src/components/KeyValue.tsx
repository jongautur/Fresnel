import type { ReactNode } from "react";

export function KeyValueGrid({ children }: { children: ReactNode }) {
  return <dl className="kv">{children}</dl>;
}

export function KV({ k, children, mono, hint }: { k: string; children: ReactNode; mono?: boolean; hint?: string }) {
  const empty = children == null || children === "" || children === false;
  return (
    <>
      <dt title={hint}>{k}</dt>
      <dd className={mono ? "mono" : undefined}>{empty ? <span className="muted">—</span> : children}</dd>
    </>
  );
}
