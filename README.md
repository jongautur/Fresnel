# Fresnel

Native desktop tool for Wi-Fi analysis and site surveys, for Linux and Windows.
Tauri 2 · Rust · React/TypeScript · SQLite. Fully offline.
Linux reads Wi-Fi through NetworkManager (D-Bus) and nl80211; Windows uses the
Native Wifi API. Both operate entirely offline.

**Status: v0.3**: adapter discovery, BSSID scanning with real dBm (nl80211),
current connection with TX/RX link rates, channel overlap map, dBm / % display setting.
Survey: projects → buildings → floors, floor plan import (PNG/JPEG/SVG) with a two-point
scale, **Measure Here** points stored in SQLite, access points marked on the plan with their
BSSIDs, and heatmaps (signal per network or AP, coverage vs. a target, AP overlap, serving AP).
Active tests come later.
See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Prerequisites (Ubuntu/Debian)

```bash
sudo apt install build-essential curl wget file pkg-config libssl-dev \
  libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev patchelf
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust toolchain
# Node.js ≥ 20 + npm
npm install
```

On Linux, NetworkManager must be running. Scanning uses the polkit action
`org.freedesktop.NetworkManager.wifi.scan`, which is allowed by default for
users in an active local session. No root is needed.

On Windows: Rust (MSVC toolchain) and Node.js ≥ 20; WebView2 comes with Windows 10/11.

## Run

```bash
npm run tauri dev
```

From the integrated terminal of the **snap** build of VS Code, use
`scripts/clean-snap-env.sh npm run tauri dev`. Otherwise GTK picks up snap
libraries and crashes with `symbol lookup error … /snap/core20/…`.

Logging: `RUST_LOG=fresnel_core=debug npm run tauri dev`. Logs also go to daily
`fresnel.YYYY-MM-DD.log` files in the app log folder (Linux: `~/.local/share/io.fresnel.app/logs/`,
Windows: `%LOCALAPPDATA%\io.fresnel.app\logs\`), panics included. **Settings → Copy diagnostics**
puts versions, OS, adapters, database state and recent log lines on the clipboard for bug reports.

## Without the GUI

The core crate has no Tauri/WebKit dependency, so it can be exercised directly against real hardware:

```bash
cargo run -p fresnel-core --example probe            # adapters, connection, cached BSSIDs
cargo run -p fresnel-core --example probe -- --scan  # trigger a fresh scan
cargo test -p fresnel-core
```

## Package

```bash
npm run tauri build                     # AppImage + .deb  → target/release/bundle/
npm run tauri build -- --bundles deb    # just the .deb
```

Bundle settings per OS live in `src-tauri/tauri.linux.conf.json` and `tauri.windows.conf.json`
(NSIS, per-user install, offline WebView2).
Flatpak isn't targeted on purpose: future monitor-mode and USB-probe access conflicts with its sandbox.

## CI and releases

Every push and PR runs `.github/workflows/ci.yml` on Ubuntu 22.04 and Windows:
`npm run build`, `npm run test:report` (the exported report stays inert and escapes SSIDs),
`cargo fmt --check`, `cargo clippy -D warnings` and `cargo test`.
`npm run bench:heatmap` times the heatmap grids (not in CI).
The Windows leg is allowed to fail until the Linux-only adapters are gated.
Toolchain: `rust-toolchain.toml`; the minimum Rust is `rust-version` in `Cargo.toml`.

The version lives in `Cargo.toml` (`[workspace.package]`); Tauri takes it from there.
`package.json` must match, which `scripts/check-version.sh [vX.Y.Z]` checks. To release:

```bash
# bump Cargo.toml, then:
npm version --no-git-tag-version 0.4.0 && cargo update --workspace
scripts/check-version.sh v0.4.0
git tag v0.4.0 && git push origin v0.4.0
```

The tag runs `.github/workflows/release.yml`: CI, then `.deb`, AppImage and NSIS installer
with `SHA256SUMS` in a **draft** release. Publish it after checking the builds on a clean machine.
v0.3.0 predates the workflows; v0.4.0 is the first CI-built release.

## Layout

| Path | What |
|---|---|
| `crates/fresnel-core/src/adapters/` | Hardware providers behind the `WifiAdapterProvider` trait (NetworkManager on Linux; Windows planned) |
| `crates/fresnel-core/src/wifi/` | Normalised models, channel math, provider-agnostic `Scanner` |
| `crates/fresnel-core/src/database/` | SQLite + versioned migrations, project/survey repositories |
| `crates/fresnel-core/src/survey/` | Survey models, floor plan file store, Measure Here |
| `src-tauri/` | Thin Tauri shell: app state and IPC commands |
| `src/` | React UI (`api/tauri.ts` is the only file that calls the backend) |

## Where the data comes from

On Linux, NetworkManager triggers scans and supplies the BSS list, connection state and
security. The kernel's nl80211 interface (read-only, no root) adds real **dBm**,
supported bands (incl. 6 GHz), monitor-mode support, PHY generation, BSS Load and
TX/RX link rates. Noise is shown only on drivers that report it (not iwlwifi).

- If the driver can't report dBm, signal falls back to NM's 0–100 % quality, always
  labelled as %.
- NM remembers BSSes for minutes; the kernel only ~30 s. Entries the kernel no longer
  has show % instead of dBm, and the **Seen** column dims rows older than 30 s.

On Windows, Native Wifi (`WlanScan`, `WlanGetNetworkBssList`) reports dBm and % side
by side. Fresnel rejects drivers that synthesize dBm from %, and leaves fields Windows
doesn't report empty rather than estimating them. Windows may require both Location
services and desktop-app Location access for BSSIDs; see [Windows hardware testing](docs/windows-testing.md).

## Survey measurements

**Measure Here** triggers a fresh scan and stores only the BSSIDs heard *during* that
scan (raw dBm, %, channel, width, centre, SSID, security, PHY), plus which adapter and
model took them. The backend's cache is never saved as a measurement:

- If the Wi-Fi service declines the scan, Fresnel retries; after a few attempts it saves nothing
  and says so.
- Scans that heard only the connected network (while others were in range moments ago), or
  that lost dBm for some BSSIDs, count as incomplete and are retried.
- Scans on one adapter are spaced as the provider requires: at least 5.5 s with
  NetworkManager, because on NetworkManager + iwlwifi scans started sooner often heard only
  the associated AP. Settings shows the spacing for the selected adapter.

**Heatmaps** (toolbar → Heatmap) interpolate the points' dBm with inverse-distance
weighting. A network not heard at a point counts as "not heard" there (−100 dBm), so a
strong neighbour can't paint signal where it was measurably absent. Shading stops 3 m from
the nearest point. Walls aren't modelled, so measure on both sides of walls that matter.

**Access points** (toolbar → Access points): click where an AP is mounted and link the BSSIDs it
broadcasts. Fresnel pre-selects the strongest group of BSSIDs sharing a base MAC near that spot.
Names then appear in point readings and the heatmap. A BSSID belongs to one AP per building.

Floor plans are copied into the app data folder (`~/.local/share/io.fresnel.app/floorplans/`;
Windows: `%LOCALAPPDATA%\io.fresnel.app\floorplans\`), next to `fresnel.db`. Before a schema
upgrade the database is backed up next to itself (`fresnel.db.bak-v…`, last 3 kept); a damaged
database is set aside as `fresnel.db.corrupt-…` and a new one started, with a notice in the app. Point coordinates are plan pixels; metres come from the floor's scale line.

## License

MIT. See [LICENSE](LICENSE).
