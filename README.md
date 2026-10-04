<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/branding/logo-dark.svg">
    <img src="docs/branding/logo-light.svg" alt="Fresnel" width="320">
  </picture>
</p>

<p align="center">
  <b>Wi-Fi analysis, site surveys and network tools for Linux and Windows.</b><br>
  Free and open source. No account, no cloud, no internet needed.
</p>

<p align="center">
  <a href="https://github.com/jongautur/Fresnel/releases">Download</a> ·
  <a href="https://fresnel.gitstuff.dev/">Website</a> ·
  <a href="ROADMAP.md">Roadmap</a> ·
  <a href="CONTRIBUTING.md">Contributing</a>
</p>

<!-- Screenshots: add docs/screenshots/live.png, survey.png and tools.png and link them here. -->

> **Testers wanted.** Fresnel is young (0.5) and needs reports from real hardware: different
> Wi-Fi cards, drivers, Windows and Linux versions. A 15-minute test helps a lot; see
> [Help test Fresnel](#help-test-fresnel).

## What it does

**See the air**
- Every access point in range: SSID, BSSID, signal in dBm, channel, width, band, PHY
  (Wi-Fi 4–7), security and BSS load, plus your own connection and its link rates.
- A channel overlap map for 2.4, 5 and 6 GHz.

**Survey a site**
- Projects, buildings and floors. Import a floor plan (PNG, JPEG, SVG), set its scale, walk,
  and press **Measure here** at each spot.
- Heatmaps: signal per network or access point, coverage against a target, AP overlap and
  serving AP, interpolated between your points.
- Mark your access points on the plan and link the BSSIDs they broadcast.
- Requirement profiles ("Office data", "Voice / video", or your own) turn readings into pass or
  fail per point, with the reason, including speed targets.
- Speed where you stand: ping and iperf3 after each measurement, shown under every point on
  the plan and in one table per floor.
- Rogue and evil-twin detection: transmitters using your SSIDs that aren't your access points,
  and copies of your network with weaker security.
- Notes and photos on points, APs and pins. A report that prints to A4 PDF; CSV and JSON exports.

**Troubleshoot the network** (the Tools page)
- Ping, traceroute / MTR, DNS lookup (compare up to five servers), port check and iperf3,
  all with live results and a history. No admin rights needed.

## Principles

- **Real numbers.** dBm comes from the driver. A quality percentage is never passed off as dBm,
  and what the hardware doesn't report is left empty, not estimated.
- **Fresh measurements only.** A survey point stores what a new scan heard at that spot,
  never the cache from a minute ago.
- **Estimates are labelled.** Heatmaps interpolate between measured points and say so; speeds are
  shown only where they were measured.
- **Offline and local.** Everything is stored on your computer. Fresnel contacts nothing
  unless you type its address into a tool.

## Download

From the [latest release](https://github.com/jongautur/Fresnel/releases):

| | File | Notes |
|---|---|---|
| Windows 10/11 | `Fresnel_<version>_x64-setup.exe` | Per-user install, no admin. Not code-signed yet: SmartScreen may warn ("More info" → "Run anyway"). Windows needs Location turned on (including for desktop apps) to list access points. |
| Ubuntu / Debian | `Fresnel_<version>_amd64.deb` | `sudo apt install ./Fresnel_*_amd64.deb`. Needs NetworkManager. |
| Other Linux | `Fresnel_<version>_amd64.AppImage` | `chmod +x` and run. Ubuntu 24.04 without `libfuse2`: add `--appimage-extract-and-run`. Needs NetworkManager. |

Checksums are in `SHA256SUMS`. Releases are marked pre-release until the Windows build has
passed testing on real hardware.

**Known issue:** on Windows 11 with Smart App Control on, the unsigned uninstaller is blocked.
Code signing will fix it.

## Help test Fresnel

The most useful thing right now is a report from your hardware. It takes about 15 minutes:

1. Install the latest release and open **Live**: does it list your adapter and the networks
   around you? Compare a few with your phone or `netsh wlan show networks mode=bssid` /
   `nmcli dev wifi`.
2. Open **Survey**, create a project, import any floor plan image and set the scale.
   Take five **Measure here** points around a room and look at the heatmaps.
3. Try the **Tools** page: ping your router, trace a route to `1.1.1.1`, look up a name.
4. Export the floor report (**Export…** in the survey toolbar).
5. In **Settings**, press **Copy diagnostics** and paste it into a
   [hardware report](https://github.com/jongautur/Fresnel/issues/new?template=hardware_report.yml),
   with what worked and what didn't.

We especially need: Intel, MediaTek, Realtek and Qualcomm cards, USB Wi-Fi adapters,
Windows 10, Fedora and Arch, and 6 GHz (Wi-Fi 6E/7) networks.

Found a bug? Open a [bug report](https://github.com/jongautur/Fresnel/issues/new?template=bug_report.yml).
Questions and ideas go to [Discussions](https://github.com/jongautur/Fresnel/discussions).

## How it gets the data

**Linux:** NetworkManager (D-Bus) triggers scans and supplies the BSS list and connection
state. The kernel's nl80211 interface (read-only, no root) adds real dBm, supported bands
(including 6 GHz), PHY generation, BSS load and TX/RX link rates. If a driver can't report dBm,
signal falls back to NetworkManager's 0–100 % quality, always labelled as %. Scanning uses the
polkit action `org.freedesktop.NetworkManager.wifi.scan`, allowed by default for users in a
local session.

**Windows:** the Native Wifi API reports dBm and % side by side. Fresnel rejects drivers that
synthesise dBm from %, and leaves fields Windows doesn't report (noise, MCS, spatial streams)
empty.

**Measure here** triggers a fresh scan and stores only the BSSIDs heard during it, with the
adapter and model that took them. Scans that heard only the connected network, or lost dBm for
some BSSIDs, count as incomplete and are retried; if the Wi-Fi service keeps declining, nothing is
saved and the app says so.

**Heatmaps** interpolate dBm between points (inverse-distance weighting). A network not heard at
a point counts as not heard there, so a strong neighbour can't paint signal where it was
measurably absent. Shading stops 3 m from the nearest point. Walls aren't modelled, so measure
on both sides of walls that matter.

**Your data** lives in the app data folder (`~/.local/share/io.fresnel.app/`;
Windows: `%LOCALAPPDATA%\io.fresnel.app\`): the SQLite database, floor plans and photos. The
database is backed up before every schema upgrade (`fresnel.db.bak-v…`, last three kept). A
newer Fresnel's database can't be opened by an older one.

## Building and contributing

Fresnel is Rust (core and Tauri 2 shell) and React/TypeScript (UI). Build instructions,
tests and how to send changes are in [CONTRIBUTING.md](CONTRIBUTING.md); the design is in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md); what's next is in [ROADMAP.md](ROADMAP.md).

## License

MIT. See [LICENSE](LICENSE).
