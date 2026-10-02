# Windows hardware testing

Use a Windows 11 laptop (and, if available, Windows 10) with Location services
and **Let desktop apps access your location** enabled in `ms-settings:privacy-location`.
Record the adapter model, driver version, Windows version, and whether either setting
is controlled by policy.

From a Developer PowerShell in the repository run:

```powershell
cargo run -p fresnel-core --example probe -- --scan
cargo run -p fresnel-core --example probe -- --scan --record windows-scan.json
netsh wlan show networks mode=bssid
```

Compare adapter names, SSIDs/BSSIDs, primary channel, signal values and the number of
BSSIDs. `ulChCenterFrequency` is a primary channel: compare it to `netsh`'s channel,
not to an 80/160 MHz centre. Check that the scan's age values are sensible immediately
after scanning and again a few minutes later. If Fresnel says its dBm values are derived
from quality, report that notice and do not treat the dBm as valid.

Repeat while connected, with the radio disabled, with WLAN AutoConfig stopped, and with
Location disabled. Send back the probe output, `windows-scan.json`, `netsh` output,
Fresnel logs, adapter/driver details, and timings for several scans at 1 s, 4 s and 8 s
spacing. This is also how we will replace the conservative 4 s Windows scan interval.
