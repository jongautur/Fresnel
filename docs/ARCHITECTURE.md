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
│       │   ├── survey/
│       │   │   ├── models.rs     # Building / Floor / FloorPlan / FloorScale / SurveyPoint / Sample
│       │   │   ├── floorplan.rs  # PlanStore: validated plan image files in the app data dir
│       │   │   └── measure.rs    # Measure Here: fresh-scan orchestration + freshness filter
│       │   └── database/
│       │       ├── mod.rs        # Database handle (rusqlite)
│       │       ├── migrations.rs # Versioned migrations (PRAGMA user_version)
│       │       ├── projects.rs   # Project repository
│       │       └── survey.rs     # Buildings, floors, points, samples
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
│   │       ├── projects.rs
│   │       └── survey.rs
│   └── tauri.conf.json           # bundle targets: appimage, deb
├── src/                          # React + TypeScript (Vite)
│   ├── api/tauri.ts              # the only file that calls invoke()
│   ├── types/wifi.ts             # mirrors Rust models
│   ├── components/               # AdapterSelector, WifiTable, ChannelMap, ...
│   │   └── survey/               # SurveyNav, FloorWorkspace, PlanCanvas, PointDetails
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
                                   ├── NetworkManagerProvider   (+ nl80211 helper)
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
`ScanRejected`, `Timeout`, `Unsupported`, `Database`, `InvalidInput`,
`AdapterMismatch`, `Backend`). They are serialised as
`{ kind, message }` and the UI shows a helpful message instead of crashing.

### nl80211 enrichment (dBm, bands, freshness)

NetworkManager's D-Bus API exposes **only a 0–100 % quality value**, has no 6 GHz
flag, and no PHY/noise/load data. `adapters/nl80211/` reads the kernel's cfg80211
state over generic netlink (`wl-nl80211` crate). Every request used is an
unprivileged *dump*:

| nl80211 request | Gives |
|---|---|
| `GET_WIPHY` (split dump) | supported bands (incl. 6 GHz), monitor/AP mode support |
| `GET_SCAN` | per-BSS signal in mBm, ms since last heard, beacon interval, raw IEs |
| `GET_STATION` | link signal / averaged signal, TX/RX rate (PHY, MCS, NSS, width, GI) |
| `GET_SURVEY` | noise floor per channel (driver-dependent; iwlwifi has none) |

It's a helper like `sysfs.rs`, not a provider. The NM provider keeps triggering
scans (polkit-authorised) and owns the BSS list, and adds kernel measurements
matched by BSSID + frequency. Without nl80211 it degrades to NM-only data. A
future root-capable nl80211 provider (own scan trigger, monitor mode) can reuse
the module.

The model keeps both signal values, independently optional, and never converts
% → dBm:

```rust
pub struct Signal { pub dbm: Option<f32>, pub quality_percent: Option<u8> }
```

cfg80211 expires a BSS ~30 s after it was last heard, while NM remembers it
longer. A BSS that NM lists but the kernel no longer has gets no `dbm`. The UI
shows % for it and explains why.

### Scan reliability

Two NetworkManager + iwlwifi behaviours make "the scan completed" an
unreliable signal, both measured on real hardware:

* A `LastScan` change < 1.2 s after `RequestScan` is a scan that was already
  running (e.g. a supplicant background scan) ending, with partial results
  (185 ms "scans" were seen). The provider requests again once.
* Scans started < 5 s after the previous one often came back having heard
  only the associated AP (the kernel BSS table is flushed at scan start).
  `Scanner` spaces hardware scans on one adapter ≥ 5.5 s apart.

### Per-card calibration (not yet implemented)

dBm readings differ between cards by several dB (antennas, chain combining,
drivers). Samples store raw dBm plus the adapter and model that took them
(`survey_points.adapter_model`, `adapter_hw_id`); an optional per-model
offset will be applied at display/heatmap time, never to the stored data.

## 3a. Site survey

```text
projects ─< buildings ─< floors ─< survey_points ─< survey_samples
                         (plan file, scale line)   (x, y, adapter)   (one row per BSSID heard)
```

* **Floor plans**: PNG/JPEG/SVG, format sniffed from content. The file is copied
  to `<app data>/floorplans/plan-*.{png,jpg,svg}`; the DB stores only the file
  name. Unreferenced plan files are garbage-collected at startup and after
  deletes. The image travels over IPC as a raw body (headers carry floor id and
  size) and back as a raw response, with no base64.
* **Coordinates** are plan pixels in the image's natural size *as the webview
  renders it* (accounts for EXIF rotation; small SVGs are scaled up so they stay
  sharp). Metres = pixels / (scale line length px / length m). Replacing a plan is
  refused while the floor has points.
* **Measure Here** (`survey::measure`): pre-flight (floor, plan, position,
  adapter consistency), then a scan the hardware actually performed (retries if
  NM declined or the scan looks incomplete), then keep only BSSes whose
  `last_seen_age_ms` ≤ scan duration + 250 ms. A point with zero samples is a
  valid dead zone. If a provider reports no ages at all, measuring is refused
  rather than guessing.
* **Heatmaps** (`src/lib/heatmap.ts`, computed in the webview from the stored
  points): per point, the strongest dBm of the chosen SSID/BSSID/band (or
  −100 dBm "not heard"), or for overlap the number of distinct APs ≥ a level
  (BSSIDs differing only in the first/last octet count as one AP). IDW (power 2,
  neighbours within 6 m) on a ~10 cm grid, masked beyond 3 m from the nearest
  point. Only dBm is interpolated; never %. Colours: one-hue blue ramp for
  signal, fixed good/critical status colours for coverage pass/fail.
* **Placed APs** (migration v3: `placed_aps`, `placed_ap_bssids`): a position on
  one floor plus the BSSIDs it broadcasts. Names resolve building-wide, and a
  BSSID belongs to at most one AP per building (checked in Rust). Replacing a
  plan is refused while APs are placed on it. Heatmap views: "per AP" (all its
  BSSIDs as one network) and "serving AP" (IDW per AP, strongest wins; below
  the level counts as unserved). An AP's colour is its categorical slot by
  building order, never its rank.
* **One adapter per floor**: measuring with a different adapter (ID or hardware
  ID) than earlier points returns `adapter_mismatch`; the UI asks and retries
  with `allowAdapterChange`.

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
  signal_dbm, signal_quality,
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

Done since: survey DB (buildings/floors/floor plans/points/samples), Measure
Here, channel analyser, heatmaps (IDW). Next: active tests, per-model
calibration offsets, report export.
