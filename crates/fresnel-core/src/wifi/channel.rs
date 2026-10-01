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

#[cfg(test)]
mod tests {
    use super::*;

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
