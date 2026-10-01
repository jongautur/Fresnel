# Fresnel — Architecture

Linux-first native desktop app (Tauri 2 + Rust + React/TypeScript + SQLite).
Fully offline. No web server, no Electron, no cloud.

## 1. Repository layout

```text
wifi-tool/
├── Cargo.toml                    # Cargo workspace
├── crates/
│   └── fresnel-core/            # All domain logic. NO Tauri dependency.
│       ├── src/
│       │   ├── lib.rs
│       │   ├── error.rs          # WifiError: typed, serialisable error kinds
│       │   ├── adapters/
│       │   │   ├── mod.rs
│       │   │   ├── traits.rs     # WifiAdapterProvider trait
│       │   │   ├── registry.rs   # AdapterRegistry: routes adapter IDs → providers
│       │   │   ├── sysfs.rs      # PCI/USB product names from sysfs + pci.ids
│       │   │   └── networkmanager/
│       │   │       ├── mod.rs    # NetworkManagerProvider (impl of the trait)
│       │   │       ├── proxies.rs# zbus proxies for org.freedesktop.NetworkManager
│       │   │       └── convert.rs# NM flags/enums → normalised model
│       │   ├── wifi/
│       │   │   ├── mod.rs
│       │   │   ├── models.rs     # Normalised Adapter / AccessPoint / Connection models
│       │   │   ├── channel.rs    # frequency ↔ channel ↔ band
│       │   │   └── scanner.rs    # Scanner service (scan orchestration, provider-agnostic)
│       │   └── database/
│       │       ├── mod.rs        # Database handle (rusqlite)
│       │       ├── migrations.rs # Versioned migrations (PRAGMA user_version)
│       │       └── projects.rs   # Project repository
│       └── examples/
│           └── probe.rs          # CLI: list adapters + scan against real hardware
├── src-tauri/                    # Thin Tauri shell
│   ├── src/
│   │   ├── main.rs
│   │   ├── lib.rs                # builder, state, logging
│   │   ├── state.rs              # AppState { registry, scanner, db }
│   │   └── commands/
│   │       ├── mod.rs
│   │       ├── adapters.rs
│   │       ├── wifi.rs
│   │       └── projects.rs
│   └── tauri.conf.json           # bundle targets: appimage, deb
├── src/                          # React + TypeScript (Vite)
│   ├── api/tauri.ts              # the only file that calls invoke()
│   ├── types/wifi.ts             # mirrors Rust models
│   ├── components/               # AdapterSelector, WifiTable, ConnectionInfo, ...
│   ├── pages/                    # Live, Networks, Survey, Settings
│   └── lib/format.ts             # signal/band/security formatting
└── docs/ARCHITECTURE.md
```

**Why a separate `fresnel-core` crate:** hardware providers, models, scanning
and DB have no reason to depend on Tauri. Keeping them separate means they can be
built, unit-tested and exercised against real hardware (`cargo run --example
probe`) without WebKitGTK. Later it can also back a headless CLI or a probe daemon.

Dependency direction (strict):

```text
React UI ──invoke──▶ Tauri commands ──▶ Scanner / Registry / Database
                                              │
                                              ▼
                                   dyn WifiAdapterProvider
                                   ├── NetworkManagerProvider   (v0.1)
                                   ├── Nl80211Provider          (future)
                                   ├── PcapMonitorProvider      (future)
                                   └── UsbProbeProvider         (future, ESP32)
```

Only `adapters/networkmanager/` knows that NetworkManager exists.

## 2. Crates

| Crate | Purpose |
|---|---|
| `tauri` 2.x | desktop shell, IPC, bundling (AppImage/.deb) |
| `zbus` 5.x (tokio) | pure-Rust D-Bus; no libdbus C dependency; maintained by the D-Bus/GNOME folks |
| `tokio` | async runtime (shared with Tauri) |
| `async-trait` | object-safe async trait (`Arc<dyn WifiAdapterProvider>`) |
| `rusqlite` (`bundled`) | SQLite; bundled → no system libsqlite3 version skew in AppImage |
| `rusqlite_migration` | versioned migrations via `PRAGMA user_version` |
| `serde` / `serde_json` | IPC serialisation |
| `thiserror` | typed errors |
| `tracing` / `tracing-subscriber` | structured logging (`RUST_LOG=fresnel_core=debug`) |
| `chrono` | UTC timestamps |
| `futures` | concurrent property fetches |

Deliberately **not** used: the `networkmanager` crate (dbus-rs/libdbus based,
stale), `tauri-plugin-sql` (would expose raw SQL to the webview; the DB stays
behind Rust commands), nmcli parsing.

## 3. Talking to NetworkManager

System bus, `org.freedesktop.NetworkManager`, via hand-written `zbus::proxy`
definitions (only the members we use):

* `NetworkManager`: `GetDevices`, `WirelessEnabled`, `WirelessHardwareEnabled`, `Version`
* `Device`: `Interface`, `DeviceType` (2 = Wi-Fi), `State`, `Driver`, `Udi`, `HwAddress`, `Ip4Config`, `Managed`
* `Device.Wireless`: `RequestScan(a{sv})`, `GetAllAccessPoints`, `LastScan`, `ActiveAccessPoint`, `Bitrate`, `WirelessCapabilities`, `PermHwAddress`
* `AccessPoint`: `Ssid`, `HwAddress`, `Frequency`, `Strength`, `Flags`, `WpaFlags`, `RsnFlags`, `Mode`, `MaxBitrate`, `Bandwidth`, `LastSeen` (one `Properties.GetAll` per AP, fetched concurrently)
* `IP4Config`: `AddressData`, `Gateway`

Scan sequence:

1. Resolve adapter ID → NM device path (re-resolved on every call so we can tell
   when an adapter has disappeared).
2. Pre-flight: NM reachable? device present? `WirelessEnabled`/hardware rfkill?
   device state not `unavailable`/`unmanaged`?
3. Read `LastScan`, call `RequestScan({})`.
   * Polkit denial → `PermissionDenied`.
   * NM "scan already running / rate limited" → not fatal: we report
     `scan_triggered = false` and return the cached list.
4. Wait (≤ 15 s) for `LastScan` to change via the `PropertiesChanged` stream.
5. `GetAllAccessPoints` → normalise → `ScanResult`.

Errors map to `WifiError` kinds (`ServiceUnavailable`, `NoAdapters`,
`AdapterNotFound`, `AdapterUnavailable`, `RadioDisabled`, `PermissionDenied`,
`ScanBusy`, `Timeout`, `Unsupported`, `Backend`). They are serialised as
`{ kind, message }` and the UI shows a helpful message instead of crashing.

### Signal strength honesty

NetworkManager's D-Bus API exposes **only a 0–100 % quality value**, not dBm.
The model therefore carries both, independently optional:

```rust
pub struct Signal { pub dbm: Option<f32>, pub quality_percent: Option<u8> }
```

The NM provider fills `quality_percent` only. A future nl80211 provider (scan
dumps are unprivileged) fills `dbm`. The UI labels the unit explicitly and never
converts % → dBm.

### 6 GHz detection

NM has flags for 2.4 and 5 GHz but **no 6 GHz flag**. Capabilities are therefore
tri-state (`supported` / `unsupported` / `unknown`); 6 GHz is `unknown` until
the nl80211 provider (wiphy band dump) is added.

## 4. Normalised models (summary — see `wifi/models.rs`)

```rust
AdapterId(String)                    // "linux:wlp0s20f3"
Adapter {
  id, provider, interface_name, display_name, driver, hw_address,
  bus: Option<BusInfo { kind, vendor_id, product_id, vendor_name, product_name }>,
  capabilities: AdapterCapabilities,
  status: AdapterStatus,             // Connected | Disconnected | Connecting | Unavailable | Unmanaged | RadioOff | Unknown
}
AdapterCapabilities {
  band_2ghz, band_5ghz, band_6ghz,   // Capability = Supported | Unsupported | Unknown
  active_scan, passive_scan, monitor_mode, packet_capture, ap_mode,
  reports_dbm, reports_quality,
}
AccessPointObservation {             // one BSSID seen in one scan
  timestamp, adapter_id,
  bssid,                             // IDENTITY of the AP
  ssid: Option<String>, ssid_raw: Vec<u8>, hidden,
  frequency_mhz, channel, band, channel_width_mhz,
  signal: Signal,
  security: Security { kind, akms, wpa, rsn, privacy },
  mode, max_bitrate_kbps, last_seen_age_ms, is_connected,
  // future, all Option: noise_dbm, snr_db, channel_utilization, beacon_interval_tu,
  //                     phy_type, wifi_generation, retry_rate, frame_stats
}
ScanResult { adapter_id, started_at, completed_at, scan_triggered, notice, access_points }
ConnectionInfo { adapter_id, interface_name, ssid, bssid, frequency_mhz, channel, band,
                 channel_width_mhz, signal, bitrate_kbps, security, ipv4, gateway_ipv4 }
```

Access points are **never merged by SSID** in Rust. SSID grouping is a UI toggle.

## 5. Implementation sequence

1. Workspace + `fresnel-core` models, channel math, error types (+ unit tests)
2. NM provider: device discovery → adapters (verify with `probe` example on real HW)
3. NM provider: scan + AP normalisation + current connection (verify on real HW)
4. Registry + Scanner service; SQLite with versioned migrations (`projects`)
5. Tauri shell: state, commands, logging
6. React UI: layout/nav, AdapterSelector, ConnectionInfo, WifiTable (sorting, SSID grouping), Live page
7. Packaging config (AppImage, .deb); README with system prerequisites
8. Clean-up pass

Next phases: nl80211 provider (dBm, 6 GHz, noise), survey DB (buildings/floors/
floor plans/points/samples), Measure Here, channel analyser, active tests,
heatmaps (IDW).
