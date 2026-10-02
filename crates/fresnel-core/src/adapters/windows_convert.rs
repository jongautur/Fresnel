//! Small, platform-neutral conversions used by the Native Wifi provider.
//!
//! They intentionally don't mention Windows FFI types so Linux CI exercises
//! the edge cases that hardware normally makes difficult to reproduce.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::wifi::models::{Band, Capability};

/// Native Wifi's `ulChCenterFrequency` is the primary frequency in kHz.
pub fn frequency_khz_to_mhz(khz: u32) -> Option<u32> {
    (khz != 0 && khz.is_multiple_of(1000)).then_some(khz / 1000)
}

/// `ullHostTimestamp` is FILETIME (100 ns ticks since 1601). Only expose an
/// age which is credible for a current scan; a few drivers return another
/// clock domain or zero.
pub fn filetime_age_ms(filetime: u64, now: SystemTime) -> Option<u64> {
    const EPOCH_DELTA_100NS: u64 = 116_444_736_000_000_000;
    let unix_100ns = filetime.checked_sub(EPOCH_DELTA_100NS)?;
    let now_100ns = now.duration_since(UNIX_EPOCH).ok()?.as_nanos() / 100;
    let age_ms = now_100ns.checked_sub(u128::from(unix_100ns))? / 10_000;
    let age_ms = u64::try_from(age_ms).ok()?;
    (age_ms <= 10 * 60 * 1000).then_some(age_ms)
}

/// Some drivers synthesize RSSI exactly as `quality / 2 - 100`. It is not a
/// measurement, so callers must suppress dBm for the entire scan.
pub fn rssi_is_quality_derived(readings: &[(i32, u32)]) -> bool {
    readings.len() >= 3
        && readings.iter().all(|&(rssi, quality)| {
            quality <= 100 && rssi == i32::try_from(quality / 2).unwrap_or_default() - 100
        })
}

/// Bounds-check an IE range relative to the complete BSS-list allocation.
pub fn ie_range(offset: u32, size: u32, allocation_size: usize) -> Option<std::ops::Range<usize>> {
    let start = usize::try_from(offset).ok()?;
    let end = start.checked_add(usize::try_from(size).ok()?)?;
    (end <= allocation_size).then_some(start..end)
}

/// Native Wifi rates are documented in bits per second. Zero means unknown.
pub fn rate_bps_to_kbps(rate_bps: u32) -> Option<u32> {
    (rate_bps != 0).then_some(rate_bps / 1_000)
}

/// PHY types only prove some bands. HT/HE/EHT are multi-band, so they must
/// not be used to claim a 6 GHz capability.
pub fn band_capability_from_phys(phys: &[i32], band: Band) -> Capability {
    let supports = match band {
        Band::Band2_4GHz => phys.iter().any(|p| matches!(*p, 2 | 5 | 6)),
        Band::Band5GHz => phys.iter().any(|p| matches!(*p, 4 | 8)),
        Band::Band6GHz => false,
        _ => false,
    };
    if supports {
        Capability::Supported
    } else if phys.iter().any(|p| matches!(*p, 7 | 10 | 11)) {
        Capability::Unknown
    } else {
        Capability::Unsupported
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frequency_and_ie_bounds() {
        assert_eq!(frequency_khz_to_mhz(5_180_000), Some(5180));
        assert_eq!(frequency_khz_to_mhz(5_180_500), None);
        assert_eq!(ie_range(20, 4, 24), Some(20..24));
        assert_eq!(ie_range(21, 4, 24), None);
    }
    #[test]
    fn rejects_derived_rssi() {
        assert!(rssi_is_quality_derived(&[(-75, 50), (-70, 60), (-60, 80)]));
        assert!(!rssi_is_quality_derived(&[(-75, 50), (-69, 60), (-60, 80)]));
    }
    #[test]
    fn filetime_is_plausibility_checked() {
        let now = UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let ft = 116_444_736_000_000_000 + 1_699_999_995_u64 * 10_000_000;
        assert_eq!(filetime_age_ms(ft, now), Some(5000));
        assert_eq!(filetime_age_ms(0, now), None);
    }
    #[test]
    fn rates_and_phy_band_evidence_are_honest() {
        assert_eq!(rate_bps_to_kbps(866_700_000), Some(866_700));
        assert_eq!(rate_bps_to_kbps(0), None);
        assert_eq!(
            band_capability_from_phys(&[6], Band::Band2_4GHz),
            Capability::Supported
        );
        assert_eq!(
            band_capability_from_phys(&[8], Band::Band5GHz),
            Capability::Supported
        );
        assert_eq!(
            band_capability_from_phys(&[10], Band::Band6GHz),
            Capability::Unknown
        );
    }
}
