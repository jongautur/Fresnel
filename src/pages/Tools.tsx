import { useState } from "react";
import type { ToolKind } from "../types/tools";
import { TOOL_LABEL } from "../lib/tools";
import { PingTool } from "../components/tools/PingTool";
import { TraceTool } from "../components/tools/TraceTool";
import { DnsTool } from "../components/tools/DnsTool";
import { PortTool } from "../components/tools/PortTool";
import { Iperf3Tool } from "../components/tools/Iperf3Tool";

const TABS: ToolKind[] = ["ping", "traceroute", "dns", "port_check", "iperf3"];
const TAB_KEY = "fresnel.tools.tab";

function loadTab(): ToolKind {
  try {
    const t = localStorage.getItem(TAB_KEY) as ToolKind | null;
    if (t && TABS.includes(t)) return t;
  } catch {
    /* ignore */
  }
  return "ping";
}

/**
 * Network troubleshooting tools. Every tool stays mounted (hidden when not
 * shown), so a run keeps its live output while you look at another tool.
 */
export function Tools() {
  const [tab, setTabState] = useState<ToolKind>(loadTab);
  const setTab = (t: ToolKind) => {
    setTabState(t);
    try {
      localStorage.setItem(TAB_KEY, t);
    } catch {
      /* ignore */
    }
  };
  return (
    <div className="page">
      <div className="page-header">
        <h1>Tools</h1>
        <p className="muted">
          Native tests, no admin rights. Nothing is contacted unless you enter it. Runs are saved in the history below
          each tool.
        </p>
      </div>
      <div className="segmented tool-tabs">
        {TABS.map((t) => (
          <button key={t} type="button" className={tab === t ? "active" : ""} onClick={() => setTab(t)}>
            {TOOL_LABEL[t]}
          </button>
        ))}
      </div>
      <div hidden={tab !== "ping"} className="tool-panel"><PingTool /></div>
      <div hidden={tab !== "traceroute"} className="tool-panel"><TraceTool /></div>
      <div hidden={tab !== "dns"} className="tool-panel"><DnsTool /></div>
      <div hidden={tab !== "port_check"} className="tool-panel"><PortTool /></div>
      <div hidden={tab !== "iperf3"} className="tool-panel"><Iperf3Tool /></div>
    </div>
  );
}
