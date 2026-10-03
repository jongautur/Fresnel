import { useWifi } from "../../state/WifiContext";
import type { IpFamily, Via } from "../../types/tools";

/** "System route" (what the computer uses anyway) or bound to one Wi-Fi adapter. */
export function ViaPicker({ value, onChange, disabled }: { value: Via; onChange: (v: Via) => void; disabled?: boolean }) {
  const { listing } = useWifi();
  const adapters = listing?.adapters ?? [];
  const selected = value.mode === "wifi" ? value.adapterId : "";
  return (
    <label className="tool-field" title="System route: whichever interface the computer uses for the target (Ethernet, VPN or Wi-Fi). Wi-Fi: bound to that adapter; refused if the target would be reached another way.">
      <span className="field-label">Via</span>
      <select
        className="input"
        disabled={disabled}
        value={selected}
        onChange={(e) => onChange(e.target.value ? { mode: "wifi", adapterId: e.target.value } : { mode: "system" })}
      >
        <option value="">System route</option>
        {adapters.map((a) => (
          <option key={a.id} value={a.id}>
            Wi-Fi: {a.interfaceName ?? a.displayName}
            {a.connectedSsid ? ` (${a.connectedSsid})` : " (not connected)"}
          </option>
        ))}
        {value.mode === "wifi" && !adapters.some((a) => a.id === value.adapterId) && (
          <option value={value.adapterId}>Wi-Fi: {value.adapterId} (not found)</option>
        )}
      </select>
    </label>
  );
}

export function FamilyPicker({
  value,
  onChange,
  disabled,
}: {
  value: IpFamily;
  onChange: (v: IpFamily) => void;
  disabled?: boolean;
}) {
  return (
    <label className="tool-field" title="Which address to use when a name has both IPv4 and IPv6 addresses">
      <span className="field-label">IP</span>
      <select className="input" disabled={disabled} value={value} onChange={(e) => onChange(e.target.value as IpFamily)}>
        <option value="any">Auto</option>
        <option value="v4">IPv4</option>
        <option value="v6">IPv6</option>
      </select>
    </label>
  );
}
