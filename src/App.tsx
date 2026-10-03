import { useEffect, useState, type ComponentType, type SVGProps } from "react";
import { WifiProvider, useWifi } from "./state/WifiContext";
import { PreferencesProvider } from "./state/Preferences";
import { Live } from "./pages/Live";
import { Networks } from "./pages/Networks";
import { Survey } from "./pages/Survey";
import { Settings, type Theme } from "./pages/Settings";
import { Tools } from "./pages/Tools";
import { IconLive, IconNetworks, IconSettings, IconSurvey, IconTools } from "./components/Icons";
import { StatusDot } from "./components/StatusDot";
import { ErrorBanner } from "./components/ErrorBanner";
import { api } from "./api/tauri";
import { NAVIGATE_EVENT, type NavigateDetail, type PageId } from "./lib/navigation";

type Page = PageId;

const NAV: { id: Page; label: string; icon: ComponentType<SVGProps<SVGSVGElement>> }[] = [
  { id: "live", label: "Live", icon: IconLive },
  { id: "networks", label: "Networks", icon: IconNetworks },
  { id: "survey", label: "Survey", icon: IconSurvey },
  { id: "tools", label: "Tools", icon: IconTools },
  { id: "settings", label: "Settings", icon: IconSettings },
];

const THEME_KEY = "fresnel.theme";


function loadTheme(): Theme {
  try {
    const t = localStorage.getItem(THEME_KEY);
    if (t === "dark" || t === "light" || t === "system") return t;
  } catch {
    /* ignore */
  }
  return "system";
}

function SidebarFooter() {
  const { selectedAdapter } = useWifi();
  if (!selectedAdapter) return null;
  return (
    <div className="sidebar-footer" title={selectedAdapter.displayName}>
      <span className="mono">{selectedAdapter.interfaceName}</span>
      <StatusDot status={selectedAdapter.status} label={false} />
    </div>
  );
}

export default function App() {
  const [page, setPage] = useState<Page>("live");
  const [theme, setThemeState] = useState<Theme>(loadTheme);
  // E.g. a damaged database was set aside at startup: say so before the user
  // wonders where their projects went.
  const [dbNotice, setDbNotice] = useState<string | null>(null);

  // Other views can ask to show a page (e.g. Tools › iperf3 → Settings).
  useEffect(() => {
    const go = (e: Event) => {
      const target = (e as CustomEvent<NavigateDetail>).detail;
      if (!target || !NAV.some((n) => n.id === target.page)) return;
      setPage(target.page);
      if (target.anchor) {
        requestAnimationFrame(() => document.getElementById(target.anchor!)?.scrollIntoView({ block: "start" }));
      }
    };
    window.addEventListener(NAVIGATE_EVENT, go);
    return () => window.removeEventListener(NAVIGATE_EVENT, go);
  }, []);

  useEffect(() => {
    api.appInfo().then(
      (info) => setDbNotice(info.databaseNotice),
      () => {},
    );
  }, []);

  useEffect(() => {
    if (theme === "system") delete document.documentElement.dataset.theme;
    else document.documentElement.dataset.theme = theme;
  }, [theme]);

  const setTheme = (t: Theme) => {
    setThemeState(t);
    try {
      localStorage.setItem(THEME_KEY, t);
    } catch {
      /* ignore */
    }
  };

  return (
    <PreferencesProvider>
    <WifiProvider>
      <div className="app">
        <nav className="sidebar">
          <div className="brand">
            <span className="brand-mark">◉</span>
            <span>Fresnel</span>
          </div>
          {NAV.map(({ id, label, icon: Icon }) => (
            <button key={id} type="button" className={`nav-item ${page === id ? "active" : ""}`} onClick={() => setPage(id)}>
              <Icon />
              <span>{label}</span>
            </button>
          ))}
          <div className="spacer" />
          <SidebarFooter />
        </nav>
        <main className="content">
          {dbNotice && (
            <div className="pad">
              <ErrorBanner error={{ kind: "database", message: dbNotice }} />
            </div>
          )}
          {page === "live" && <Live />}
          {page === "networks" && <Networks />}
          {page === "survey" && <Survey />}
          {/* Kept mounted: a running tool keeps its live output across pages. */}
          <div hidden={page !== "tools"} className="page-host">
            <Tools />
          </div>
          {page === "settings" && <Settings theme={theme} setTheme={setTheme} />}
        </main>
      </div>
    </WifiProvider>
    </PreferencesProvider>
  );
}
