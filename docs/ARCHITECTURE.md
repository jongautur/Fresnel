# Fresnel — Architecture

Native desktop app for Linux and Windows (Tauri 2 + Rust + React/TypeScript +
SQLite). Fully offline. No web server, no Electron, no cloud. Hardware access
goes through per-OS providers: NetworkManager + nl80211 on Linux (done), the
Native Wifi API on Windows (in progress).

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
│       │   │   ├── fake.rs       # FakeProvider: scripted provider for tests
│       │   │   ├── nl80211/      # kernel dBm/bands/rates helper (generic netlink)
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
│       │   │   ├── filestore.rs  # FileStore: validated files + locked import/GC (plans, photos)
│       │   │   ├── floorplan.rs  # PlanStore: plan images (FileStore, prefix plan-)
│       │   │   ├── photos.rs     # PhotoStore: photo import, report copy + thumbnail
│       │   │   └── measure.rs    # Measure Here: fresh-scan orchestration + freshness filter
│       │   └── database/
│       │       ├── mod.rs        # Database handle (rusqlite)
│       │       ├── migrations.rs # Versioned migrations (PRAGMA user_version)
│       │       ├── projects.rs   # Project repository
│       │       ├── survey.rs     # Buildings, floors, points, samples
│       │       ├── notes.rs      # Notes, note pins, per-floor annotations for reports
│       │       └── photos.rs     # Photo rows
│       └── examples/
│           └── probe.rs          # CLI: list adapters + scan against real hardware
├── src-tauri/                    # Thin Tauri shell
│   ├── src/
│   │   ├── main.rs
│   │   ├── lib.rs                # builder, state, logging
│   │   ├── logging.rs            # stdout + daily log files, panic hook
│   │   ├── state.rs              # AppState { scanner, plans, db } + corrupt-DB recovery
│   │   └── commands/
│   │       ├── mod.rs
│   │       ├── app.rs
│   │       ├── diagnostics.rs    # "Copy diagnostics" report, frontend error log
│   │       ├── adapters.rs
│   │       ├── wifi.rs
│   │       ├── projects.rs
│   │       └── survey.rs
│   └── tauri.conf.json           # + tauri.linux/windows.conf.json: deb/AppImage, NSIS
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
                                   ├── NetworkManagerProvider   (Linux, + nl80211 helper)
                                   ├── WindowsProvider          (in progress, Native Wifi API)
                                   ├── FakeProvider             (tests only)
                                   ├── Nl80211Provider          (future)
                                   ├── PcapMonitorProvider      (future)
                                   └── UsbProbeProvider         (future, ESP32)
```

Only `adapters/networkmanager/` knows that NetworkManager exists. OS-specific
advice for the user (polkit on Linux, Location on Windows) travels as the
error's `hint` (see below), so the UI stays provider-neutral.

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
| `image` 0.25 (jpeg, png, webp only) | photo decode with memory limits, EXIF orientation, resize, JPEG re-encode; pure Rust |
| `kamadak-exif` 0.6 | EXIF capture time and GPS presence (pure Rust, no deps beyond one tiny crate) |

Deliberately **not** used: the `networkmanager` crate (dbus-rs/libdbus based,
stale), `tauri-plugin-sql` (would expose raw SQL to the webview; the DB stays
behind Rust commands), nmcli parsing.

## 3. Linux provider: NetworkManager

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
`{ kind, message, hint }` and the UI shows a helpful message instead of crashing.
`hint` is optional advice the provider attaches with `WifiError::with_hint`
(the NM provider: `systemctl start NetworkManager`, the polkit session rule,
`nmcli device set … managed yes`); `kind()` and the message are unchanged by
it. Code that matches on a variant should match on `e.base()`.

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
  The NetworkManager provider therefore asks for ≥ 5.5 s between hardware
  scans on one adapter (`WifiAdapterProvider::min_scan_interval`); `Scanner`
  enforces whatever the serving provider returns, and the UI shows it
  (`Adapter.scanSpacingMs`).

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
  the provider declined or the scan looks incomplete), then keep only BSSes whose
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
* **Requirements profiles** (migration v4: `requirement_profiles`,
  `requirement_profile_targets`, `floors.requirement_profile_id`,
  `survey_points.adapter_bands`; `survey/requirements.rs`,
  `database/requirements.rs`): project-level thresholds (presets from common
  vendor guidance, or custom), targets by SSID bytes or placed AP, one
  default per project, a whole-profile override per floor. Rust judges each
  point pass / fail (which rules) / not evaluated (why): primary = strongest
  target BSSID; secondary = another physical AP (placed APs, else the
  `apKey` heuristic, flagged) on the primary's SSID and band; co-channel =
  distinct other radios whose span (centre ± width/2, else the 20 MHz
  primary channel, flagged) overlaps the primary's, at or above the level;
  a required band is only "missing" if the card could receive it
  (`adapter_bands`, or anything heard on it); SNR and BSS-Load utilisation
  only where reported; %-only points are never failed. The UI estimates the
  share of mapped area with the coverage grid (`src/lib/requirements.ts`)
  and always labels which share is of points and which of area.
* **Notes and photos** (migration v5): free-text `notes` on floors, points
  and placed APs (≤ 4000 characters, checked in Rust); `note_pins` (a note at
  a plan position, optional category); `photos` attached to a floor or to one
  point, AP or pin (cascading deletes). A photo import (JPEG/PNG/WebP sniffed
  from content, ≤ 25 MB; HEIC/AVIF refused with "export as JPEG") decodes
  under the image crate's limits (≤ 16 384 px a side, ≤ 120 MP, ≤ 512 MiB),
  applies the EXIF orientation and writes three files under `photos/photo-*`:
  the untouched original (evidence; keeps GPS and other metadata), a ≤ 1600 px
  JPEG report copy and a ≤ 256 px thumbnail, both re-encoded from pixels so
  they carry no metadata. `had_gps` and `taken_at` (EXIF `DateTimeOriginal`,
  offset only if recorded) come from kamadak-exif. The app only ever shows
  the copies. `Database::floor_annotations` gathers a floor's notes, pins and
  in-report photos for the report.
* **Rogue / evil-twin findings** (`survey::findings`, pure functions over
  the stored project; migration v6: per-sample security detail, hidden flag
  and MLD address, `point_anomalies`, `bssid_marks`): unknown transmitters
  using a project SSID (one broadcast by a BSSID linked to a placed AP),
  security mismatches per (SSID, band), one BSSID on two channels in one
  scan, a linked BSSID heard with another SSID/security than usual, and
  probable unlinked radios (same MLD address, else the `apKey` heuristic,
  labelled). Ownership is decided project-wide; the scope (floor, building,
  project) only limits the evidence. `Database::findings` is what the
  report will use.
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
                 channel_width_mhz, signal, bitrate_kbps, tx_rate, rx_rate, security,
                 ipv4_addresses, ipv4_gateway }
LinkRate { bitrate_kbps, phy, mcs, nss, width_mhz,
           short_gi: Option<bool> }   // None: not reported (Windows, HE/EHT)
```

Access points are **never merged by SSID** in Rust. SSID grouping is a UI toggle.

## 5. Robustness, data safety, diagnostics

* **Timeouts.** Every provider step is bounded: NM allows 10 s per D-Bus call
  and for connecting to the bus (zbus waits forever by default; a timed-out
  connection is dropped and reopened on next use) and 15 s for `LastScan` to
  advance (then cached results with a notice); each nl80211 dump gets 5 s
  (the socket is reopened, enrichment skipped). On top, `Scanner` gives each
  provider call, provider lookup included, a 30 s deadline: the call is
  dropped and `Timeout` returned, so a wedged service can't hold the
  per-adapter lock until restart.
* **Plan and photo files.** `FileStore` (behind `PlanStore` and `PhotoStore`)
  runs an import (write the files atomically, then reference them in the DB)
  and garbage collection (read the referenced set, then sweep) under one
  lock, so a collection can't delete files mid-import. Renames and deletes
  are retried briefly on sharing violations (Windows antivirus). A damaged
  database is set aside together with both directories.
* **Database.** Opening runs `PRAGMA quick_check`. Before a schema upgrade the
  WAL is checkpointed and the DB copied with `VACUUM INTO` to
  `fresnel.db.bak-v{old}-{timestamp}` (newest 3 kept); if the backup fails,
  nothing is migrated. A DB made by a newer Fresnel is refused with a clear
  message. A damaged DB is moved aside with its WAL/SHM files and floor plans
  (`*.corrupt-{timestamp}`), a new one is started, and the UI shows a notice.
  A busy DB is retried on the next command; any other failure leaves live
  scanning usable while project commands return the error.
* **Logs and diagnostics.** `tracing` to stdout and to daily
  `fresnel.YYYY-MM-DD.log` files in the app log dir (14 kept, synchronous
  writes); a panic hook logs message and backtrace; uncaught frontend errors
  are forwarded to the log (length-capped, rate-limited). "Copy diagnostics"
  in Settings builds a plain-text report: version, OS, webview, environment
  fixes, providers and adapters, database state and the last 200 log lines.
* **FakeProvider** (`adapters/fake.rs`, `cfg(test)`): a scripted provider
  (return, fail, hang per call) with no hardware or services, used to test
  `Scanner` timeouts, error pass-through and lock release on any OS.

## 6. Implementation sequence

1. Workspace + `fresnel-core` models, channel math, error types (+ unit tests)
2. NM provider: device discovery → adapters (verify with `probe` example on real HW)
3. NM provider: scan + AP normalisation + current connection (verify on real HW)
4. Registry + Scanner service; SQLite with versioned migrations (`projects`)
5. Tauri shell: state, commands, logging
6. React UI: layout/nav, AdapterSelector, ConnectionInfo, WifiTable (sorting, SSID grouping), Live page
7. Packaging config (AppImage, .deb); README with system prerequisites
8. Clean-up pass

Done since: survey DB (buildings/floors/floor plans/points/samples), Measure
Here, channel analyser, heatmaps (IDW), placed APs, the robustness work in
§5. Next: the Windows provider, report export, active tests, per-model
calibration offsets.
