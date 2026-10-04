param([switch]$Scan)
Add-Type -Path "$PSScriptRoot\WlanRaw.cs"
$g = [WlanRaw]::Open()
if ($Scan) {
    $code = 0
    $ms = [WlanRaw]::Scan($g, 15000, [ref]$code)
    "scan: code=$code elapsed=${ms}ms"
}
$now = [DateTime]::UtcNow.ToFileTimeUtc()
"now FILETIME = $now ($([DateTime]::UtcNow.ToString('o')))"
[WlanRaw]::List($g) | Sort-Object Bssid | ForEach-Object {
    [pscustomobject]@{
        SSID      = if ($_.Ssid) { $_.Ssid } else { '<hidden>' }
        BSSID     = $_.Bssid
        lRssi     = $_.Rssi
        Quality   = $_.Quality
        'q/2-100' = [int][math]::Floor($_.Quality / 2) - 100
        MHz       = $_.FreqKhz / 1000
        HostTs    = $_.HostTs
        AgeMs     = [math]::Round(($now - [int64]$_.HostTs) / 10000)
        TsfUs     = $_.TsfTs
    }
} | Format-Table -AutoSize | Out-String -Width 250
[WlanRaw]::Close()
