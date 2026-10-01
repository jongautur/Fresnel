//! Frequency ↔ channel ↔ band conversion (IEEE 802.11 channelisation).

use super::models::Band;

pub fn band_for_frequency(mhz: u32) -> Band {
    match mhz {
        2400..=2500 => Band::Band2_4GHz,
        // 4.9 GHz (Japan / public safety) is operated as part of the 5 GHz band.
        4900..=5925 => Band::Band5GHz,
        5926..=7125 => Band::Band6GHz,
        57000..=71000 => Band::Band60GHz,
        _ => Band::Unknown,
    }
}

/// Primary channel number for a centre frequency, or `None` if the frequency
/// is not on the channel raster for its band.
pub fn channel_for_frequency(mhz: u32) -> Option<u16> {
    let on_raster = |base: u32, spacing: u32| -> Option<u16> {
        let off = mhz.checked_sub(base)?;
        (off % spacing == 0).then(|| (off / spacing) as u16)
    };
    match band_for_frequency(mhz) {
        Band::Band2_4GHz => match mhz {
            2484 => Some(14),
            2412..=2472 => on_raster(2407, 5),
            _ => None,
        },
        Band::Band5GHz => {
            if mhz < 5000 {
                on_raster(4000, 5)
            } else {
                on_raster(5000, 5)
            }
        }
        Band::Band6GHz => match mhz {
            5935 => Some(2),
            _ => on_raster(5950, 5).filter(|c| *c >= 1 && *c <= 233),
        },
        Band::Band60GHz => on_raster(56160, 2160).filter(|c| *c >= 1),
        Band::Unknown => None,
    }
}

/// Centre frequency of the whole occupied channel (primary + secondaries).
///
/// 5 and 6 GHz bonding is fixed by the channelisation: a 40/80/160 MHz block
/// is determined by the primary channel and the width alone. On 2.4 GHz a 40
/// MHz channel can extend up or down (HT40+/−), which only the AP's HT
/// Operation element says (`ht_secondary_offset`). Returns `None` when the
/// centre can't be determined (e.g. 2.4 GHz 40 MHz without the IE, 320 MHz,
/// whose two overlapping channelisations need the EHT Operation element).
pub fn channel_center_mhz(
    primary_mhz: u32,
    width_mhz: Option<u32>,
    ht_secondary_offset: Option<i8>,
) -> Option<u32> {
    let width = width_mhz.unwrap_or(20);
    if width <= 20 {
        return Some(primary_mhz);
    }
    match band_for_frequency(primary_mhz) {
        Band::Band2_4GHz if width == 40 => {
            let offset = ht_secondary_offset? as i32;
            Some((primary_mhz as i32 + offset * 10) as u32)
        }
        Band::Band5GHz | Band::Band6GHz if matches!(width, 40 | 80 | 160) => {
            let channel = channel_for_frequency(primary_mhz)? as u32;
            let (base, base_mhz) = match band_for_frequency(primary_mhz) {
                // 5 GHz blocks are aligned to channel 36 (UNII-1..2e) or 149 (UNII-3).
                Band::Band5GHz if channel >= 149 => (149, 5745),
                Band::Band5GHz if channel >= 36 => (36, 5180),
                Band::Band6GHz => (1, 5955),
                _ => return None,
            };
            let span = width / 5; // channel numbers per block
            let start = base + (channel - base) / span * span;
            // Centre channel = start + (span - 4) / 2; 5 MHz per channel number.
            let center_channel = start + (span - 4) / 2;
            Some(base_mhz + (center_channel - base) * 5)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centers() {
        // 20 MHz: primary.
        assert_eq!(channel_center_mhz(2412, Some(20), None), Some(2412));
        assert_eq!(channel_center_mhz(5180, None, None), Some(5180));
        // 2.4 GHz HT40 needs the offset.
        assert_eq!(channel_center_mhz(2412, Some(40), Some(1)), Some(2422));
        assert_eq!(channel_center_mhz(2462, Some(40), Some(-1)), Some(2452));
        assert_eq!(channel_center_mhz(2412, Some(40), None), None);
        // 5 GHz: ch 64 @160 → block 36–64, centre ch 50 (5250 MHz).
        assert_eq!(channel_center_mhz(5320, Some(160), None), Some(5250));
        // ch 48 @160 → centre 50; ch 100 @80 → centre 106; ch 44 @80 → 42.
        assert_eq!(channel_center_mhz(5240, Some(160), None), Some(5250));
        assert_eq!(channel_center_mhz(5500, Some(80), None), Some(5530));
        assert_eq!(channel_center_mhz(5220, Some(80), None), Some(5210));
        // ch 149 @80 → centre 155; ch 140 @40 → centre 142.
        assert_eq!(channel_center_mhz(5745, Some(80), None), Some(5775));
        assert_eq!(channel_center_mhz(5700, Some(40), None), Some(5710));
        // 6 GHz: ch 37 @160 → centre 47 (6185 MHz); ch 5 @80 → centre 7.
        assert_eq!(channel_center_mhz(6135, Some(160), None), Some(6185));
        assert_eq!(channel_center_mhz(5975, Some(80), None), Some(5985));
        // 320 MHz is ambiguous.
        assert_eq!(channel_center_mhz(6135, Some(320), None), None);
    }

    #[test]
    fn band_2_4() {
        assert_eq!(band_for_frequency(2412), Band::Band2_4GHz);
        assert_eq!(channel_for_frequency(2412), Some(1));
        assert_eq!(channel_for_frequency(2437), Some(6));
        assert_eq!(channel_for_frequency(2462), Some(11));
        assert_eq!(channel_for_frequency(2472), Some(13));
        assert_eq!(channel_for_frequency(2484), Some(14));
        assert_eq!(channel_for_frequency(2413), None);
    }

    #[test]
    fn band_5() {
        assert_eq!(band_for_frequency(5180), Band::Band5GHz);
        assert_eq!(channel_for_frequency(5180), Some(36));
        assert_eq!(channel_for_frequency(5320), Some(64));
        assert_eq!(channel_for_frequency(5500), Some(100));
        assert_eq!(channel_for_frequency(5745), Some(149));
        assert_eq!(channel_for_frequency(5885), Some(177));
        assert_eq!(channel_for_frequency(4920), Some(184));
    }

    #[test]
    fn band_6() {
        assert_eq!(band_for_frequency(5935), Band::Band6GHz);
        assert_eq!(channel_for_frequency(5935), Some(2));
        assert_eq!(band_for_frequency(5955), Band::Band6GHz);
        assert_eq!(channel_for_frequency(5955), Some(1));
        assert_eq!(channel_for_frequency(5975), Some(5));
        assert_eq!(channel_for_frequency(6115), Some(33));
        assert_eq!(channel_for_frequency(7115), Some(233));
    }

    #[test]
    fn band_60() {
        assert_eq!(channel_for_frequency(58320), Some(1));
        assert_eq!(channel_for_frequency(60480), Some(2));
    }

    #[test]
    fn unknown() {
        assert_eq!(band_for_frequency(900), Band::Unknown);
        assert_eq!(channel_for_frequency(900), None);
    }
}
