# Windows hardware test results — 2026-10-04 (in progress)

First run of the Windows code on real hardware, following `docs/windows-testing.md`.

**Summary so far**
- `npm ci`, `npm run build` and `cargo test` pass (218 passed, 1 ignored), after Smart App Control was turned off.
- Open questions: `lRssi` is real dBm, not derived from quality; `ullHostTimestamp` is FILETIME and ages are plausible.
- Scan spacing: this driver needs none. Every scan takes about 3.7 s, and back-to-back scans are fine.
- Four bugs in the Windows provider's connection details were found and fixed on `windows-testing`: link rate 1000× too low, IPv4 always empty, connected BSS/SSID never set, 5 GHz connection band lost when the cache ages out.
- Traceroute to 1.1.1.1 and 8.8.8.8 matches `tracert` hop for hop.
- Smart App Control: it blocks local builds, and it **blocked the 0.5.0 uninstaller**.
- iperf3: the firewall error handling is right, and the core client works both ways at about 1.6 Gbit/s; the app's intermittent "server closed the connection during the start of the test" is unresolved.
- Still to do: the other GUI-driven checks in C, the physical error cases in B, and most of D (see the table for each).

## Machine

| | |
|---|---|
| Windows | Windows 11 Pro 10.0.26200 (build 26200) |
| Wi-Fi adapter | MediaTek Wi-Fi 7 MT7925 Wireless LAN Card (PCI `VEN_14C3&DEV_7925&SUBSYS_60001A3B`) |
| Driver | MediaTek 5.4.0.1920, dated 2024-06-14, Native Wi-Fi driver; radios 802.11a/b/g/n/ac/ax/be |
| Wi-Fi state | Disconnected at first; later connected to `Netið` (WPA2-Personal, 5 GHz ch 64, 160 MHz, 802.11be, 2882.4 Mbps) |
| Other links | Ethernet 2 (Realtek PCIe 2.5GbE) up at 1 Gbps on the same LAN (gateway 192.168.1.254); Intel I219-V disconnected |
| Location services | On. No policy under `HKLM\SOFTWARE\Policies\Microsoft\Windows\LocationAndSensors` or `...\AppPrivacy`; machine, user and NonPackaged consent all `Allow`. `lfsvc` running. |
| Smart App Control | Was **On (enforcing)** (`HKLM\SYSTEM\CurrentControlSet\Control\CI\Policy\VerifiedAndReputablePolicyState = 1`); turned off by the machine owner during testing (now 0) |
| Toolchain | Git 2.55.0, Node 24.19.0 / npm 11.17.0, rustup 1.29.1, Rust 1.99.0 (from `rust-toolchain.toml`), VS Build Tools 2022 17.14.41 with the VCTools workload |
| Fresnel already installed | 0.5.0 (NSIS, current user) in `%LOCALAPPDATA%\Fresnel`, installed 2026-10-03 |

## Setup

| Step | Result |
|---|---|
| `npm ci` | PASS. npm 11 skipped `esbuild@0.28.2`'s postinstall ("install scripts not yet covered by allowScripts"); the build still worked. |
| `npm run build` | PASS (`tsc -b && vite build`, 94 modules, 1.6 s) |
| `cargo test --workspace --locked` (SAC on) | **FAIL**: blocked by Smart App Control (below) |
| `cargo test --workspace --locked` (SAC off) | **PASS**: 218 passed, 0 failed, 1 ignored (`nettools::traceroute::tests::real_trace`, run separately below); 119 s cold |
| `cargo clippy --workspace --all-targets -- -D warnings` | PASS (main and `windows-testing`) |

With Smart App Control on:

```
error: failed to run custom build command for `indexmap v1.9.3`
  Caused by:
    could not execute process `...\target\debug\build\indexmap-eac5e77be1c67439\build-script-build` (never executed)
  An Application Control policy has blocked this file. (os error 4551)
error: failed to run custom build command for `selectors v0.38.0`
  ... An Application Control policy has blocked this file. (os error 4551)
```

CodeIntegrity/Operational event 3077 ("did not meet the Enterprise signing level requirements …
Policy ID {0283ac0f-fff1-49ae-ada1-8a933130cad6}") and 3118 (Smart App Control block details).
Any unsigned, freshly built executable is blocked. This is not a Fresnel bug, but contributors on
Windows 11 machines with SAC on cannot build. Worth a line in the contributor docs.

## A. Raw Wi-Fi data

### Probe vs netsh

The probe (`--scan`, `--scan --record windows-scan.json`, and without `--scan`) works. Compared
with `netsh wlan show networks mode=bssid` and an independent raw `WlanGetNetworkBssList`
reader ([Method](#method-raw-wlanapi-reader)):

| Item | Result |
|---|---|
| Adapter | Listed once, `windows:6e27177d-…`, "MediaTek Wi-Fi 7 MT7925 Wireless LAN Card"; bands 2.4/5 supported, 6 GHz unknown (correct: PHY types can't prove 6 GHz); `scanSpacingMs` 4000 |
| SSIDs | Match, including the non-ASCII `Netið` (UTF-8 decoded correctly) and a hidden SSID |
| BSSIDs | Match. The number varies a lot from scan to scan here (7–19); netsh and Fresnel agree when taken back to back |
| Channel | Fresnel's channel is the **primary** channel and matches netsh for every BSSID (e.g. 78:20:51:37:70:62 ch 64 / 5320 MHz; 9C:53:22:18:BF:6E ch 44 / 5220 MHz; 22:20:61:05:48:D0 moved ch 36 → 40 during the session, and both tools followed) |
| Width/centre (from IEs) | Plausible: ch 64 @160 → centre 5250; ch 100 @160 → 5570; ch 44 @80 → 5210; ch 149 @80 → 5775 |
| Signal | dBm and % match netsh's `Rssi`/`Signal` for the connected AP (−33/−34 dBm, 98–99 %) |
| Security | `Wpa2Personal`, `Wpa3Personal`, and `Wpa2Wpa3Personal` for the H168 AP that netsh calls WPA3-Personal (transition mode, plausible) |
| Utilisation/STAs | Present where the AP sends a BSS Load IE, matching netsh |
| Scan duration | 3.7–3.9 s triggered; cached read 11–22 ms |
| Notice | None (dBm correctly not suppressed) |

### Bugs found and fixed (branch `windows-testing`, commit b5c06de)

Found in the probe's connection output and checked against `netsh wlan show interfaces` and
`Get-NetIPConfiguration`:

| Bug | Before | After |
|---|---|---|
| Link rate 1000× too low: `WLAN_ASSOCIATION_ATTRIBUTES.ulTx/RxRate` are kbit/s, but were divided by 1000 as if bit/s | `bitrateKbps: 2882` (2.9 Mbps) | `2882400` (netsh: 2882.4 Mbps) |
| IPv4 address/gateway always empty: IP Helper's `AdapterName` is `{GUID}`; braces were stripped from the wrong side | `ipv4Addresses: []`, gateway `null` | `192.168.1.113/24`, gateway `192.168.1.254` |
| Connected BSS never marked; `connectedSsid` never set (the survey's "heard only the associated AP" check could not fire on Windows) | no `*`, `connectedSsid: null` | `*` on 78:20:51:37:70:62, `connectedSsid: "Netið"` |
| Connection frequency/band null on 5 GHz once Windows drops the associated BSS from its cache; the channel fallback only knew 2.4 GHz | `frequencyMhz: null, band: null` (ch 64) | ch 32–144 multiples of 4 map to 5 GHz (they can't be 6 GHz); 149–177 stay unknown |

The Tools page's "Via Wi-Fi" binding uses its own correct GUID match (`adapter_guid_matches`), so
the IPv4 bug did not affect it.

### Open question 1: is `lRssi` real dBm or derived from `uLinkQuality`?

**Real dBm.** 143 (lRssi, uLinkQuality) pairs from 20 BSSIDs over 8 scans:

| lRssi (dBm) | quality | quality/2 − 100 would give |
|---|---|---|
| −90 | 15 | −93 |
| −85 | 30 | −85 |
| −79 | 48 | −76 |
| −75 | 60 | −70 |
| −68 / −67 | 77 / 77 | −62 |
| −64 / −63 / −62 | 79 / 80 / 80 | −61 / −60 / −60 |
| −60 / −59 | 82 / 82 | −59 |
| −48 / −47 | 89 / 90 | −56 / −55 |
| −38 / −37 | 95 / 96 | −53 / −52 |
| −34 / −33 | 98 / 99 | −51 / −51 |

Quality is a monotonic, **non-linear** function of RSSI (steep below −75 dBm, flat above −65 dBm).
dBm keeps 1 dB resolution where quality doesn't change (−68 and −67 dBm both give 77; −60 and −59
both give 82). So quality is derived from RSSI, not the other way round, and
`rssi_is_quality_derived` correctly returns false (no "derived from quality" notice). Raw
samples: `rssi-quality-samples.csv`.

Side observation: some BSSIDs swing a lot between consecutive scans (BE:53:22:18:BF:6F
−86…−68 dBm, 78:20:51:37:70:60 −64…−37 dBm) while their neighbours on the same AP stay within
1 dB. This may be off-channel/partial-dwell measurements; worth watching in the Live view.

### Open question 2: units of `ullHostTimestamp`

**FILETIME: 100 ns ticks since 1601-01-01 UTC**, as `windows_convert.rs` assumes. Right after a
3.7 s scan, `now − ullHostTimestamp` was 59 ms … 3.0 s for all entries; Fresnel shows ages of
0.1–3.7 s. 44 s later (cached read) ages were 44–47 s. `ullTimestamp` (beacon TSF, µs since the
AP booted) is per-AP and unrelated, as expected.

"A few minutes later": while disconnected, about 3.5 min after the last scan
`WlanGetNetworkBssList` returned **0 entries** (netsh: 1 network). Windows ages entries out of the
list, the associated BSS included when connected. Cached reads on an idle adapter are therefore
empty or nearly so. That's expected, but the UI should explain it rather than show an empty table.

### Scan spacing (driver level, `WlanScan` → `wlan_notification_acm_scan_complete`)

4 scans per series, adapter disconnected; the gap is from scan-complete to the next `WlanScan`
(the same definition as `min_scan_interval`); 12 s idle before each series. "Fresh" = entries
whose `ullHostTimestamp` is later than the `WlanScan` call.

| Gap | Durations (ms) | Total BSSIDs | Fresh BSSIDs | Failures |
|---|---|---|---|---|
| 0 s | 3799, 3921, 3582, 3642 | 10, 12, 15, 16 | 10, 10, 14, 13 | 0 |
| 1 s | 3585, 3600, 3712, 3742 | 16, 16, 16, 17 | 13, 14, 8, 15 | 0 |
| 2 s | 3824, 3586, 3633, 3704 | 19, 18, 19, 19 | 16, 15, 16, 11 | 0 |
| 4 s | 3669, 3777, 3609, 3683 | 19, 19, 19, 19 | 17, 14, 16, 11 | 0 |
| 5 s | 3796, 3798, 3639, 3661 | 17, 15, 16, 18 | 12, 11, 13, 13 | 0 |
| 8 s | 3605, 3711, 3659, 3637 | 17, 16, 15, 17 | 12, 11, 11, 14 | 0 |
| 10 s | 3610, 3615, 3645, 3630 | 16, 15, 12, 12 | 11, 11, 10, 12 | 0 |

Every scan completed (notification 7, never 8) in 3.6–3.9 s whatever the gap. Back-to-back scans
were never rejected and didn't return fewer fresh BSSIDs; the variation follows the environment.
**The MT7925 / 5.4.0.1920 driver needs no spacing**: the scan itself limits the rate to about one
every 3.7 s. Lowering the Windows `MIN_SCAN_GAP` to 0–1 s looks safe here. One driver is not
enough evidence to set 0 for all Windows hardware, so `MIN_SCAN_GAP` was not changed.

The app's log shows triggered scans finishing in 0.6–3.3 s while connected (e.g. `count=16
elapsed_ms=559`). Windows appears to complete a requested scan early when it has just finished
its own background scan.

## B. Error cases

| Case | Result |
|---|---|
| Location services off / desktop-app access off | Not run yet (needs the Settings toggles) |
| Wi-Fi off / airplane mode / adapter disabled | Not run yet |
| WLAN AutoConfig stopped and started | Not run yet (needs admin) |
| USB adapter unplugged mid-scan | Skipped: no USB Wi-Fi adapter |

## C. The app

| Check | Result |
|---|---|
| `npm run tauri dev` | Builds and launches (45.6 s incremental). The installed 0.5.0 was already running, so the single-instance plugin focused it ("another launch; focusing the existing window") and the dev process exited. Close the installed copy before testing the dev build. |
| Tools, Ping via Wi-Fi with Ethernet up | **PASS**: refused with "Adapter 'Wi-Fi' is unavailable: traffic to 1.1.1.1 would leave through “Ethernet 2”, not the Wi-Fi interface; the test was not run" (installed 0.5.0, from its log) |
| Traceroute, ICMP (core, `real_trace` test) | **PASS**: 1.1.1.1 in 5 hops and 8.8.8.8 in 8 hops. The same addresses and PTR names as `tracert` (`rix-mh-gw.mila.network`, `be201.am5.ams.nl.ip.siminn.is`, `dns.google`, …), timings within 1 ms, destination reached |
| iperf3, server behind a firewall (192.168.1.12) | **PASS** (error handling): "Timed out: no answer from 192.168.1.12:5201" after 5.0 s, no hang, with the hint to allow inbound TCP 5201 on the server. Correct: ping and TCP 22 answered, every other port was silently dropped. Disabling the guest firewall did not help; another layer (probably Proxmox, MAC `bc:24:11:…`) still drops |
| iperf3, core client vs 192.168.1.28 (`iperf3 -s`, over Wi-Fi) | **PASS**: Fresnel's client, upload then download back to back as "Both" does (4 streams, 3 s + 1 s omit), 4 runs with 0–1000 ms pauses: 1482–1670 Mbit/s both directions, no failures |
| iperf3 in the app vs 192.168.1.28 | **Unresolved**: two runs OK (19:55:51, 19:56:01), but several failed with "the iperf3 server closed the connection during the start of the test" (19:53:06, 19:53:59, 19:56:19, 19:56:30; also 19:48:53 against 192.168.1.3). The tester reports upload works and download doesn't. The 19:56 failures overlapped diagnostic runs against the same server (one got "busy with another test"), but the 19:53 ones did not. The error comes before upload and download differ (no `PARAM_EXCHANGE` after the cookie). Still to check: a Download-only run with nothing else on the server, and Via Wi-Fi vs System route |
| Live/Networks, Settings spacing, Survey, heatmaps, point adapter model, ping/TCP ping, MTR, DNS, port check, point tests, report → PDF, 125/150 % scaling, touch | Not run yet |

## D. Installer

| Check | Result |
|---|---|
| Download `Fresnel_0.5.0_x64-setup.exe` (v0.5.0 release) | 219,852,099 bytes; SHA-256 `08ae3ac4…f401d1` matches the release's `SHA256SUMS` |
| Size | 220 MB because `tauri.windows.conf.json` bundles the offline WebView2 installer (intentional, allows an offline install) |
| Authenticode signature | **NotSigned** (installer and the installed `fresnel.exe`) |
| Installed 0.5.0 app under Smart App Control | Ran (started 13:21 and 13:53 while SAC was enforcing); how it was allowed is unclear (SAC cloud reputation, or installed before SAC switched from evaluation to on) |
| **Uninstall under Smart App Control** | **Blocked.** CodeIntegrity 3077 at 13:31:58 and 13:32:12: `…\Fresnel\uninstall.exe` attempted to load `%TEMP%\~nsu1.tmp\Un.exe` that "did not meet the Enterprise signing level requirements". NSIS copies the uninstaller to %TEMP% and runs the copy; SAC blocks the new, unsigned file. Users with SAC on cannot uninstall Fresnel. Code signing (installer and uninstaller) is the fix. |
| Install offline / SmartScreen / upgrade from 0.4.1 / uninstall keeps `%LOCALAPPDATA%\io.fresnel.app` / non-ASCII user name | Not run yet |

## Method: raw wlanapi reader

A small C# helper loaded with PowerShell `Add-Type` (no repo changes) called `WlanOpenHandle`,
`WlanRegisterNotification` (ACM), `WlanScan` and `WlanGetNetworkBssList`, and read
`WLAN_BSS_ENTRY` fields at their documented x64 offsets (lRssi +56, uLinkQuality +60,
ullTimestamp +72, ullHostTimestamp +80, ulChCenterFrequency +92; 360-byte entries).

## Attachments

In [`windows-test-results-2026-10-04/`](windows-test-results-2026-10-04/):

- Probe: `probe-scan.txt`, `probe-cached.txt` (before the fixes), `probe-scan-fixed.txt`,
  `probe-cached-fixed.txt` (after), `windows-scan.json` (`--record`, after the fixes)
- netsh: `netsh-networks-1.txt` (start of session, disconnected), `netsh-networks-2.txt` (connected)
- Raw reader: `WlanRaw.cs`, `raw-dump.ps1`, `raw-after-scan.txt`, `raw-no-scan-later.txt`,
  `rssi-quality-samples.csv`, `scan-spacing.csv`
- Traceroute: `fresnel-trace.txt`, `tracert.txt`
- `cargo-test.log` (passing run), `fresnel.2026-10-04.log` (Fresnel's log; it covers both the installed 0.5.0 and the dev build)

Still missing: "Copy diagnostics" output.
