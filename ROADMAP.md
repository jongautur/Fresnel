# Roadmap

Where Fresnel is going, roughly in order. Want to help with something here? Say so in
[Discussions](https://github.com/jongautur/Fresnel/discussions) or on the issue.

## Now: towards 1.0

1.0 means a dependable Linux and Windows app, tested on real hardware.

- **Windows on real hardware.** The first round (MediaTek MT7925, Windows 11) found and fixed
  several bugs. Still to check: error cases (Location off, Wi-Fi off, WLAN service stopped),
  the survey and report on Windows, display scaling, and the installer. More cards wanted.
- **Code signing for Windows**, so SmartScreen stops warning and Smart App Control allows the
  uninstaller. Then a Microsoft Store listing.
- **Linux packaging checks:** the AppImage on a clean Ubuntu 24.04 (no `libfuse2`), NVIDIA +
  Wayland and X11; whether the `.deb` should require NetworkManager or only recommend it.
- **Attach Tools runs to survey points**, so a ping or iperf3 run from the Tools page shows up
  in point details, the speed table and the report.
- **Small fixes from testing:** explain an empty network list when Windows has aged out its
  cache; 6 GHz channel numbers when Windows has dropped the connected network's entry.

## Next: more troubleshooting

- **LAN scanner:** your own subnet only. Who's there (IP, MAC, vendor from an offline list,
  hostname), and what's new since last time.
- **UDP iperf3:** jitter and loss at a target bitrate; bidirectional tests.
- **HTTP(S) check:** status, redirects, TLS version and certificate expiry, time to first
  byte. Path MTU in traceroute.
- **Internet speed tests**, only when you start them and clearly labelled as contacting an
  external server: M-Lab NDT7, Ookla through its own CLI if installed, or your own LibreSpeed.
- **Monitor one spot over time:** signal, link rate, channel load, roams and disconnects,
  logged with charts and an event list.
- **AP locator:** a large live signal readout (with optional tone) to walk towards an AP.

## After 1.0

- **Before/after comparison:** several survey sessions per floor, a difference heatmap and a
  summary ("coverage at −67 dBm went from 64 % to 91 %").
- **Channel load heatmap** from the BSS Load that access points advertise.
- **Security audit:** open networks, WEP, WPA/TKIP, WPS, transition modes, PMF, hidden SSIDs,
  as findings in the report.
- **Channel planner:** suggested channel and width per placed AP from what was heard on site,
  with the reasoning shown.
- **Project files:** export and import a whole project (data, plans, photos) to move it
  between computers or keep a backup.
- **Smaller:** undo for deletes; an Icelandic translation; a dimmed floor plan in dark mode.

## Later

- **Per-card calibration:** dBm offsets per adapter model, applied at display time only.
- **Walk surveys:** measure continuously along a path.
- **Model-assisted estimates** between points (a path-loss model per AP, labelled as a model),
  suggested AP positions, and eventually walls with materials and predictive design.
- **More providers:** a root-capable nl80211 provider with monitor mode, packet capture, and
  external probes (USB, ESP32).

## Not planned

- Accounts, cloud sync or telemetry. Fresnel stays offline and local.
- Faking data the hardware doesn't report (for example converting % to dBm).
- A Flatpak: the sandbox conflicts with the planned monitor-mode and USB-probe access.
