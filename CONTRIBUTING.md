# Contributing to Fresnel

Thanks for helping. Reports from real hardware are as valuable as code: if you only have
15 minutes, see [Help test Fresnel](README.md#help-test-fresnel).

## Ground rules

These shape every change:

- **Never fake data.** dBm and quality % are never converted into each other. What the
  hardware or OS doesn't report stays unknown (`None` / `null`), never a default. Estimates
  are labelled as estimates.
- **Offline.** Nothing contacts the internet unless the user typed that address into a tool.
  No telemetry, no update checks, no web server.
- **Linux and Windows are equal targets.** Every change builds and passes the tests on both;
  CI checks this on every push.
- **Hardware goes through `WifiAdapterProvider`.** The UI and survey code never know which OS
  or backend they run on.
- **No admin or root.** Features work as a normal user, or are labelled as needing more.

## Getting started

### Linux (Ubuntu/Debian)

```bash
sudo apt install build-essential curl wget file pkg-config libssl-dev \
  libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libxdo-dev patchelf
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust; the version is pinned in rust-toolchain.toml
# Node.js 20 or newer, then:
npm install
npm run tauri dev
```

NetworkManager must be running. From the integrated terminal of the snap build of VS Code,
use `scripts/clean-snap-env.sh npm run tauri dev`, or GTK picks up snap libraries and crashes.

### Windows

Install Git, Node.js 20+, Rust via rustup (MSVC), and Visual Studio Build Tools with the
"Desktop development with C++" workload. Then `npm install` and `npm run tauri dev`.

On Windows 11 with **Smart App Control** on, freshly built (unsigned) build scripts are
blocked ("An Application Control policy has blocked this file"). Building needs Smart App
Control off.

### Notes for both

- Only one Fresnel runs at a time: close an installed copy before `npm run tauri dev`, or the
  dev build hands over to it and exits.
- The dev build uses the same data folder as an installed one. If your branch adds a database
  migration, an older installed Fresnel can no longer open that database (it is backed up first).
- Logs: `RUST_LOG=fresnel_core=debug npm run tauri dev`; files in the app's `logs/` folder.

### Without the GUI

The core crate has no Tauri or WebKit dependency, so it runs against real hardware directly:

```bash
cargo run -p fresnel-core --example probe            # adapters, connection, cached BSSIDs
cargo run -p fresnel-core --example probe -- --scan  # trigger a fresh scan
```

## Checks

CI runs these on Ubuntu 22.04 and Windows for every push and pull request. Run them before
you send a change:

```bash
npm run build                                          # TypeScript + Vite
npm run test:report                                    # the exported report stays inert
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Some tests use real hardware or the network and are ignored by default, e.g.
`FRESNEL_TRACE_TARGET=1.1.1.1 cargo test -p fresnel-core real_trace -- --ignored --nocapture`.
The iperf3 integration test runs when `iperf3` is installed.

## Layout

| Path | What |
|---|---|
| `crates/fresnel-core/src/adapters/` | Providers behind `WifiAdapterProvider`: NetworkManager + nl80211 (Linux), Native Wifi (Windows), a fake for tests |
| `crates/fresnel-core/src/wifi/` | Normalised models, channel maths, IE parsers, the provider-agnostic `Scanner` |
| `crates/fresnel-core/src/survey/` | Survey models, Measure here, requirements, findings, photos |
| `crates/fresnel-core/src/nettools/` | Ping, traceroute, DNS, port check, iperf3 client, interface binding |
| `crates/fresnel-core/src/tools/` | Tools page runs and their history |
| `crates/fresnel-core/src/database/` | SQLite, versioned migrations, repositories |
| `src-tauri/` | Thin Tauri shell: app state and IPC commands |
| `src/` | React UI; `api/tauri.ts` is the only file that calls the backend |

More in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Sending a change

1. For anything bigger than a small fix, open an issue or a discussion first, so we can agree
   on the approach before you spend time on it.
2. Branch from `main`. Keep commits focused; write the message as what the change does and
   why, in plain sentences (see `git log` for the style).
3. Add or update tests. Hardware-dependent code gets a pure, testable core (parsers,
   decisions) with the OS calls kept thin.
4. Database changes are new migrations in `database/migrations.rs`; never edit an old one.
5. Open a pull request describing what you tested and on which hardware and OS.

Good places to start are issues labelled
[good first issue](https://github.com/jongautur/Fresnel/labels/good%20first%20issue).

## Releases

The version lives in `Cargo.toml` (`[workspace.package]`); `package.json` must match, which
`scripts/check-version.sh` checks. Tagging `vX.Y.Z` runs `.github/workflows/release.yml`:
CI, then the `.deb`, AppImage and Windows installer with `SHA256SUMS`, in a draft release.

## License

By contributing you agree that your work is released under the [MIT license](LICENSE).
