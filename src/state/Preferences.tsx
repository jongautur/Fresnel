import { createContext, useCallback, useContext, useState, type ReactNode } from "react";

/** Which signal value to display when both are available. */
export type SignalUnit = "dbm" | "percent";

const UNIT_KEY = "fresnel.signalUnit";

interface Preferences {
  signalUnit: SignalUnit;
  setSignalUnit: (u: SignalUnit) => void;
}

const PreferencesContext = createContext<Preferences | null>(null);

function loadUnit(): SignalUnit {
  try {
    return localStorage.getItem(UNIT_KEY) === "percent" ? "percent" : "dbm";
  } catch {
    return "dbm";
  }
}

export function PreferencesProvider({ children }: { children: ReactNode }) {
  const [signalUnit, setUnit] = useState<SignalUnit>(loadUnit);
  const setSignalUnit = useCallback((u: SignalUnit) => {
    setUnit(u);
    try {
      localStorage.setItem(UNIT_KEY, u);
    } catch {
      /* non-essential */
    }
  }, []);
  return (
    <PreferencesContext.Provider value={{ signalUnit, setSignalUnit }}>{children}</PreferencesContext.Provider>
  );
}

export function usePreferences(): Preferences {
  const ctx = useContext(PreferencesContext);
  if (!ctx) throw new Error("usePreferences must be used inside <PreferencesProvider>");
  return ctx;
}
