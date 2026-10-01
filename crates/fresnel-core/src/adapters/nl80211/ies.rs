//! Minimal IEEE 802.11 information-element walker.
//!
//! Only extracts what the normalised model needs (PHY generation and BSS
//! Load). Element data comes from third-party APs and may be malformed, so
//! everything is bounds-checked and a truncated element simply ends the walk.

const EID_BSS_LOAD: u8 = 11;
const EID_HT_CAP: u8 = 45;
const EID_HT_OPERATION: u8 = 61;
const EID_VHT_CAP: u8 = 191;
const EID_EXTENSION: u8 = 255;
const EXT_HE_CAP: u8 = 35;
const EXT_EHT_CAP: u8 = 108;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ElementSummary {
    pub ht: bool,
    pub vht: bool,
    pub he: bool,
    pub eht: bool,
    /// HT Operation secondary channel offset: +1 above, -1 below the primary.
    pub ht_secondary_offset: Option<i8>,
    /// BSS Load element: associated station count.
    pub station_count: Option<u16>,
    /// BSS Load element: channel utilisation, 0–255 scale.
    pub channel_utilization_raw: Option<u8>,
}

impl ElementSummary {
    /// Wi-Fi generation number the AP advertises. VHT elements on 2.4 GHz
    /// (vendor "256-QAM" extensions) don't make an AP Wi-Fi 5.
    pub fn wifi_generation(&self, is_2ghz: bool) -> Option<u8> {
        if self.eht {
            Some(7)
        } else if self.he {
            Some(6)
        } else if self.vht && !is_2ghz {
            Some(5)
        } else if self.ht {
            Some(4)
        } else {
            None
        }
    }

    pub fn phy_type(&self, is_2ghz: bool) -> &'static str {
        match self.wifi_generation(is_2ghz) {
            Some(7) => "802.11be",
            Some(6) => "802.11ax",
            Some(5) => "802.11ac",
            Some(4) => "802.11n",
            _ if is_2ghz => "802.11b/g",
            _ => "802.11a",
        }
    }

    pub fn channel_utilization_pct(&self) -> Option<f32> {
        self.channel_utilization_raw
            .map(|u| u as f32 * 100.0 / 255.0)
    }
}

pub fn summarise(ies: &[u8]) -> ElementSummary {
    let mut s = ElementSummary::default();
    let mut rest = ies;
    while rest.len() >= 2 {
        let (id, len) = (rest[0], rest[1] as usize);
        let Some(data) = rest.get(2..2 + len) else {
            break;
        };
        match id {
            EID_HT_CAP => s.ht = true,
            EID_VHT_CAP => s.vht = true,
            // data[0] = primary channel; data[1] bits 0-1 = secondary offset.
            EID_HT_OPERATION if data.len() >= 2 => {
                s.ht_secondary_offset = match data[1] & 0b11 {
                    1 => Some(1),
                    3 => Some(-1),
                    _ => None,
                };
            }
            EID_BSS_LOAD if data.len() >= 3 => {
                s.station_count = Some(u16::from_le_bytes([data[0], data[1]]));
                s.channel_utilization_raw = Some(data[2]);
            }
            EID_EXTENSION => match data.first() {
                Some(&EXT_HE_CAP) => s.he = true,
                Some(&EXT_EHT_CAP) => s.eht = true,
                _ => {}
            },
            _ => {}
        }
        rest = &rest[2 + len..];
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_generation_and_bss_load() {
        let ies = [
            0, 3, b'a', b'b', b'c', // SSID
            45, 2, 0, 0, // HT cap
            11, 5, 7, 0, 128, 0, 0, // BSS Load: 7 stations, util 128
            191, 1, 0, // VHT cap
            255, 2, 35, 0, // HE cap
        ];
        let s = summarise(&ies);
        assert!(s.ht && s.vht && s.he && !s.eht);
        assert_eq!(s.wifi_generation(false), Some(6));
        assert_eq!(s.phy_type(false), "802.11ax");
        assert_eq!(s.station_count, Some(7));
        assert!((s.channel_utilization_pct().unwrap() - 50.2).abs() < 0.1);
    }

    #[test]
    fn ht_operation_offset() {
        assert_eq!(summarise(&[61, 2, 6, 0b11]).ht_secondary_offset, Some(-1));
        assert_eq!(summarise(&[61, 2, 1, 0b01]).ht_secondary_offset, Some(1));
        assert_eq!(summarise(&[61, 2, 1, 0]).ht_secondary_offset, None);
    }

    #[test]
    fn vht_on_2ghz_is_not_wifi5() {
        let s = summarise(&[45, 1, 0, 191, 1, 0]);
        assert_eq!(s.wifi_generation(true), Some(4));
        assert_eq!(s.wifi_generation(false), Some(5));
    }

    #[test]
    fn truncated_input_is_safe() {
        let s = summarise(&[45, 10, 1, 2]);
        assert!(!s.ht);
        assert_eq!(summarise(&[]), ElementSummary::default());
        assert_eq!(summarise(&[255]), ElementSummary::default());
        assert!(!summarise(&[255, 0]).he);
    }
}
