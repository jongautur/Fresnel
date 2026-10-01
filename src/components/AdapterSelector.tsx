import { useEffect, useRef, useState } from "react";
import { useWifi } from "../state/WifiContext";
import type { Adapter } from "../types/wifi";
import { bandsSummary } from "../lib/format";
import { StatusDot } from "./StatusDot";
import { IconChevron, IconRefresh } from "./Icons";

function AdapterLines({ adapter }: { adapter: Adapter }) {
  return (
    <span className="adapter-lines">
      <span className="adapter-name">{adapter.displayName}</span>
      <span className="adapter-meta">
        <span className="mono">{adapter.interfaceName ?? adapter.id}</span>
        <span className="sep">·</span>
        <span>{bandsSummary(adapter)}</span>
        <span className="sep">·</span>
        <StatusDot status={adapter.status} />
      </span>
    </span>
  );
}

export function AdapterSelector() {
  const { listing, selectedAdapter, selectAdapter, refreshAdapters, adaptersLoading } = useWifi();
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const adapters = listing?.adapters ?? [];

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (!root.current?.contains(e.target as Node)) setOpen(false);
    };
    const esc = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", esc);
    };
  }, [open]);

  return (
    <div className="adapter-selector" ref={root}>
      <label className="field-label">Adapter</label>
      <div className="adapter-selector-row">
        <button
          type="button"
          className="adapter-button"
          onClick={() => setOpen((o) => !o)}
          disabled={adapters.length === 0}
          aria-haspopup="listbox"
          aria-expanded={open}
        >
          {selectedAdapter ? (
            <AdapterLines adapter={selectedAdapter} />
          ) : (
            <span className="muted">{adaptersLoading ? "Detecting adapters…" : "No Wi-Fi adapter"}</span>
          )}
          <IconChevron className="chevron" />
        </button>
        <button
          type="button"
          className="btn btn-icon"
          title="Re-detect adapters"
          onClick={() => void refreshAdapters()}
          disabled={adaptersLoading}
        >
          <IconRefresh className={adaptersLoading ? "spin" : ""} />
        </button>
      </div>
      {open && (
        <ul className="adapter-menu" role="listbox">
          {adapters.map((a) => (
            <li key={a.id}>
              <button
                type="button"
                role="option"
                aria-selected={a.id === selectedAdapter?.id}
                className={a.id === selectedAdapter?.id ? "selected" : ""}
                onClick={() => {
                  selectAdapter(a.id);
                  setOpen(false);
                }}
              >
                <AdapterLines adapter={a} />
                <span className="adapter-provider">{a.provider}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
