# Fresnel

Native Linux desktop tool for Wi-Fi analysis and site surveys.
Tauri 2 · Rust · React/TypeScript · SQLite · NetworkManager (D-Bus). Fully offline.

**Status: v0.1**: adapter discovery, BSSID scanning, current connection, live view,
SQLite project storage. Heatmaps, floor plans and active tests come later.
See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Prerequisites (Ubuntu/Debian)

```bash
sudo apt install build-essential curl wget file pkg-config libssl-dev \
  libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev patchelf
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust toolchain
# Node.js ≥ 20 + npm
npm install
```

NetworkManager must be running. Scanning uses the polkit action
`org.freedesktop.NetworkManager.wifi.scan`, which is allowed by default for
users in an active local session. No root is needed.

## Run

```bash
npm run tauri dev
```

From the integrated terminal of the **snap** build of VS Code, use
`scripts/clean-snap-env.sh npm run tauri dev`. Otherwise GTK picks up snap
libraries and crashes with `symbol lookup error … /snap/core20/…`.

Logging: `RUST_LOG=fresnel_core=debug npm run tauri dev`.

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

Flatpak isn't targeted on purpose: future monitor-mode and USB-probe access conflicts with its sandbox.

## Layout

| Path | What |
|---|---|
| `crates/fresnel-core/src/adapters/` | Hardware providers behind the `WifiAdapterProvider` trait (NetworkManager now) |
| `crates/fresnel-core/src/wifi/` | Normalised models, channel math, provider-agnostic `Scanner` |
| `crates/fresnel-core/src/database/` | SQLite + versioned migrations |
| `src-tauri/` | Thin Tauri shell: app state and IPC commands |
| `src/` | React UI (`api/tauri.ts` is the only file that calls the backend) |

## Known limitations of the NetworkManager provider

- Signal is NM's **0–100 % quality**, not dBm. The model keeps `dbm` and `qualityPercent`
  as separate fields, and the UI always labels the unit.
- 6 GHz support shows as *unknown* because NM has no flag for it.
- NM keeps BSSes in its list for a while after they are last heard. The **Seen** column
  shows the age, and rows older than 30 s are dimmed.

All three are planned to be fixed by a future nl80211 provider (dBm, wiphy bands, noise).

## License

MIT. See [LICENSE](LICENSE).
