//! IEEE 802.11 information-element parsing, shared by all providers.
//!
//! Providers hand over the raw elements of a beacon or probe response (nl80211
//! on Linux, the Native Wifi BSS list on Windows) and get back what the
//! normalised model needs: PHY generation, BSS Load, operating channel width
//! and centre, RSN/WPA security detail and the Wi-Fi 7 MLD address.
//!
//! Element IDs and layouts follow IEEE 802.11-2020 (§9.4.2) and 802.11be /
//! 802.11-2024 for EHT and Multi-Link. Element data comes from third-party
//! APs and may be malformed, so everything is bounds-checked: a truncated
//! element ends the walk, and an element whose content doesn't parse or
//! doesn't fit the channelisation yields `None` rather than a guess. Fields
//! the spec lets an AP omit stay `None` too; spec defaults are not filled in.

use super::channel::{
    band_for_frequency, channel_center_mhz, channel_for_frequency, frequency_for_channel,
};
use super::models::{Akm, Band, Cipher, Pmf, Security};
use super::security::classify;

const EID_BSS_LOAD: u8 = 11;
const EID_HT_CAP: u8 = 45;
const EID_RSN: u8 = 48;
const EID_HT_OPERATION: u8 = 61;
const EID_VHT_CAP: u8 = 191;
const EID_VHT_OPERATION: u8 = 192;
const EID_VENDOR: u8 = 221;
const EID_EXTENSION: u8 = 255;
const EXT_HE_CAP: u8 = 35;
const EXT_HE_OPERATION: u8 = 36;
const EXT_EHT_OPERATION: u8 = 106;
const EXT_MULTI_LINK: u8 = 107;
const EXT_EHT_CAP: u8 = 108;

const OUI_IEEE: [u8; 3] = [0x00, 0x0F, 0xAC];
/// Microsoft OUI, used by the pre-RSN WPA element (type 1).
const OUI_MICROSOFT: [u8; 3] = [0x00, 0x50, 0xF2];
const OUI_WFA: [u8; 3] = [0x50, 0x6F, 0x9A];
const MICROSOFT_WPA: u8 = 1;
const WFA_OWE_TRANSITION: u8 = 0x1C;

/// HT Operation element (EID 61) fields that bear on the channel layout.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct HtOperation {
    primary: u8,
    /// Secondary Channel Offset, raw: 0 none, 1 above, 3 below, 2 reserved.
    secondary_offset: u8,
    /// Channel Center Frequency Segment 2 (HT Operation Information bits
    /// 13–20): the 160 MHz centre for Extended NSS BW signalling, 0 if unused.
    ccfs2: u8,
}

/// Channel Width + CCFS0 + CCFS1, the layout shared by the VHT Operation
/// Information (in VHT and HE Operation) and the EHT Operation Information.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct ChannelOperation {
    width: u8,
    ccfs0: u8,
    ccfs1: u8,
}

/// HE Operation's 6 GHz Operation Information.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct He6GhzOperation {
    primary: u8,
    /// Control bits 0–1: 0 = 20, 1 = 40, 2 = 80, 3 = 160 or 80+80 MHz.
    width: u8,
    ccfs0: u8,
    ccfs1: u8,
}

/// Parsed RSN element (EID 48) or WPA vendor element (221, 00-50-F2 type 1).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RsnInfo {
    pub version: u16,
    /// Absent fields stay empty/`None`: the spec's defaults for an omitted
    /// field (CCMP, 802.1X) are not filled in.
    pub group_cipher: Option<Cipher>,
    pub pairwise_ciphers: Vec<Cipher>,
    pub akms: Vec<Akm>,
    /// RSN only: group management (BIP) cipher.
    pub group_mgmt_cipher: Option<Cipher>,
    /// RSN only: from RSN Capabilities MFPC (bit 7) / MFPR (bit 6). `None`
    /// when the field is absent or MFPR is set without MFPC (invalid).
    pub pmf: Option<Pmf>,
}

/// Operating channel from the AP's own operation elements.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ChannelSpan {
    pub width_mhz: Option<u32>,
    /// Centre of the whole occupied channel. `None` for 80+80 MHz, which
    /// has no single centre.
    pub center_mhz: Option<u32>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
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
    /// RSN element present (whether or not it parsed).
    pub rsn_present: bool,
    pub rsn: Option<RsnInfo>,
    /// WPA vendor element present (whether or not it parsed).
    pub wpa_present: bool,
    pub wpa: Option<RsnInfo>,
    /// OWE Transition Mode element (WFA vendor, type 0x1C): the open half
    /// of an OWE transition pair.
    pub owe_transition: bool,
    /// MLD MAC address from the Basic Multi-Link element.
    pub mld_addr: Option<[u8; 6]>,
    ht_operation: Option<HtOperation>,
    vht_operation: Option<ChannelOperation>,
    /// VHT Operation Information carried inside HE Operation.
    he_vht_operation: Option<ChannelOperation>,
    he_6ghz_operation: Option<He6GhzOperation>,
    eht_operation: Option<ChannelOperation>,
}

/// A channel layout as signalled, in channel numbers of the primary's band.
enum Layout {
    Contiguous {
        width: u32,
        center: u32,
    },
    /// 80+80 MHz: the segment holding the primary is centred on `seg0`.
    EightyPlusEighty {
        seg0: u32,
    },
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

    /// MLD MAC address, uppercase and colon-separated like a BSSID.
    pub fn mld_address(&self) -> Option<String> {
        self.mld_addr.map(|mac| {
            mac.iter()
                .map(|b| format!("{b:02X}"))
                .collect::<Vec<_>>()
                .join(":")
        })
    }

    /// Operating width and centre for a BSS whose primary channel is at
    /// `primary_mhz` (as the provider reports it).
    ///
    /// The most specific element present decides: EHT Operation (when it
    /// carries channel information), then HE Operation's 6 GHz information on
    /// 6 GHz, then VHT Operation on 5 GHz (or the copy inside HE Operation),
    /// then HT Operation. If that element is inconsistent (a primary or
    /// centre that doesn't match the frequency or the band's channelisation)
    /// both values are `None`; a less specific element is not consulted, as
    /// it would understate the width. Legacy (non-HT) BSSs get `None`: the
    /// absence of elements is not proof of 20 MHz.
    pub fn channel_span(&self, primary_mhz: u32) -> ChannelSpan {
        self.span(primary_mhz).unwrap_or_default()
    }

    fn span(&self, primary_mhz: u32) -> Option<ChannelSpan> {
        let band = band_for_frequency(primary_mhz);
        let primary = channel_for_frequency(primary_mhz)? as u32;
        let layout = if let Some(eht) = self.eht_operation {
            eht_layout(eht, primary)?
        } else if band == Band::Band6GHz {
            let he = self.he_6ghz_operation?;
            if he.primary as u32 != primary {
                return None;
            }
            he_6ghz_layout(he, primary)?
        } else if let Some(vht) = self
            .vht_operation
            .or(self.he_vht_operation)
            .filter(|v| band == Band::Band5GHz && v.width != 0)
        {
            let ccfs2 = self.ht_operation.map_or(0, |h| h.ccfs2);
            vht_layout(vht, ccfs2)?
        } else {
            let ht = self.ht_operation?;
            if ht.primary as u32 != primary {
                return None;
            }
            match ht.secondary_offset {
                0 => Layout::Contiguous {
                    width: 20,
                    center: primary,
                },
                1 => Layout::Contiguous {
                    width: 40,
                    center: primary + 2,
                },
                3 => Layout::Contiguous {
                    width: 40,
                    center: primary.checked_sub(2)?,
                },
                _ => return None,
            }
        };
        match layout {
            Layout::Contiguous { width, center } => {
                let center_mhz = frequency_for_channel(band, center)?;
                consistent(primary_mhz, band, width, center_mhz).then_some(ChannelSpan {
                    width_mhz: Some(width),
                    center_mhz: Some(center_mhz),
                })
            }
            Layout::EightyPlusEighty { seg0 } => {
                let seg0_mhz = frequency_for_channel(band, seg0)?;
                consistent(primary_mhz, band, 80, seg0_mhz).then_some(ChannelSpan {
                    width_mhz: Some(160),
                    center_mhz: None,
                })
            }
        }
    }

    /// Security detail from the RSN/WPA elements. `privacy` is the Privacy
    /// bit of the Capability Information field, which isn't an element.
    ///
    /// `None` when an RSN or WPA element is present but malformed: the
    /// caller can't tell what the AP offers and should keep what it has.
    /// Without either element the result is open, WEP (privacy set) or OWE
    /// (transition element).
    pub fn security(&self, privacy: bool) -> Option<Security> {
        if (self.rsn_present && self.rsn.is_none()) || (self.wpa_present && self.wpa.is_none()) {
            return None;
        }
        let mut s = Security::unknown();
        s.privacy = privacy;
        s.rsn = self.rsn_present;
        s.wpa = self.wpa_present;
        // RSN first: it's the one a modern client uses.
        for info in [&self.rsn, &self.wpa].into_iter().flatten() {
            push_unique(&mut s.akms, &info.akms);
            push_unique(&mut s.pairwise_ciphers, &info.pairwise_ciphers);
            push_unique(&mut s.group_ciphers, info.group_cipher.as_slice());
        }
        if self.owe_transition {
            push_unique(&mut s.akms, &[Akm::OweTransition]);
        }
        if let Some(rsn) = &self.rsn {
            s.group_mgmt_cipher = rsn.group_mgmt_cipher;
            s.pmf = rsn.pmf;
        }
        s.kind = classify(&s);
        Some(s)
    }
}

fn push_unique<T: PartialEq + Copy>(into: &mut Vec<T>, items: &[T]) {
    for i in items {
        if !into.contains(i) {
            into.push(*i);
        }
    }
}

/// Whether a `width_mhz` channel centred on `center_mhz` contains the 20 MHz
/// primary at `primary_mhz` and is a valid channel of its band.
fn consistent(primary_mhz: u32, band: Band, width_mhz: u32, center_mhz: u32) -> bool {
    let half = width_mhz / 2;
    let Some(lowest) = (center_mhz + 10).checked_sub(half) else {
        return false;
    };
    let in_block = primary_mhz >= lowest
        && primary_mhz <= center_mhz + half - 10
        && (primary_mhz - lowest).is_multiple_of(20);
    if !in_block {
        return false;
    }
    match (band, width_mhz) {
        (_, 20) => center_mhz == primary_mhz,
        (Band::Band2_4GHz, 40) => true,
        (Band::Band5GHz | Band::Band6GHz, 40 | 80 | 160) => {
            channel_center_mhz(primary_mhz, Some(width_mhz), None) == Some(center_mhz)
        }
        // Two overlapping 320 MHz channelisations: centres 31, 63, 95, 127,
        // 159 and 191 (802.11be Annex E).
        (Band::Band6GHz, 320) => channel_for_frequency(center_mhz)
            .is_some_and(|c| (31..=191).contains(&c) && (c - 31).is_multiple_of(32)),
        _ => false,
    }
}

/// VHT Operation Information (802.11-2020 Table 9-274, §11.40.1). Channel
/// Width 0 (20/40) is left to HT Operation by the caller.
fn vht_layout(op: ChannelOperation, ht_ccfs2: u8) -> Option<Layout> {
    let ccfs0 = op.ccfs0 as u32;
    // Extended NSS BW: the 160 MHz centre may be in HT Operation instead.
    let ccfs1 = match op.ccfs1 {
        0 => ht_ccfs2 as u32,
        c => c as u32,
    };
    match op.width {
        1 => match ccfs1 {
            0 => Some(Layout::Contiguous {
                width: 80,
                center: ccfs0,
            }),
            c if c.abs_diff(ccfs0) == 8 => Some(Layout::Contiguous {
                width: 160,
                center: c,
            }),
            c if c.abs_diff(ccfs0) > 16 => Some(Layout::EightyPlusEighty { seg0: ccfs0 }),
            _ => None,
        },
        // Deprecated encodings, still sent by older APs.
        2 => Some(Layout::Contiguous {
            width: 160,
            center: ccfs0,
        }),
        3 => Some(Layout::EightyPlusEighty { seg0: ccfs0 }),
        _ => None,
    }
}

/// HE 6 GHz Operation Information (802.11ax-2021 §9.4.2.249, Table 9-322ar).
fn he_6ghz_layout(op: He6GhzOperation, primary: u32) -> Option<Layout> {
    let (ccfs0, ccfs1) = (op.ccfs0 as u32, op.ccfs1 as u32);
    match op.width {
        0 => Some(Layout::Contiguous {
            width: 20,
            center: if ccfs0 == 0 { primary } else { ccfs0 },
        }),
        1 => Some(Layout::Contiguous {
            width: 40,
            center: ccfs0,
        }),
        2 => Some(Layout::Contiguous {
            width: 80,
            center: ccfs0,
        }),
        3 if ccfs1 != 0 && ccfs1.abs_diff(ccfs0) == 8 => Some(Layout::Contiguous {
            width: 160,
            center: ccfs1,
        }),
        3 if ccfs1 != 0 && ccfs1.abs_diff(ccfs0) > 16 => {
            Some(Layout::EightyPlusEighty { seg0: ccfs0 })
        }
        _ => None,
    }
}

/// EHT Operation Information (802.11be-2024 §9.4.2.311): CCFS0 is the
/// centre for 20/40/80 MHz; for 160 and 320 MHz it is the centre of the
/// primary 80/160 and CCFS1 the centre of the whole channel.
fn eht_layout(op: ChannelOperation, primary: u32) -> Option<Layout> {
    let (ccfs0, ccfs1) = (op.ccfs0 as u32, op.ccfs1 as u32);
    let (width, center) = match op.width {
        0 => (20, if ccfs0 == 0 { primary } else { ccfs0 }),
        1 => (40, ccfs0),
        2 => (80, ccfs0),
        3 if ccfs1.abs_diff(ccfs0) == 8 => (160, ccfs1),
        4 if ccfs1.abs_diff(ccfs0) == 16 => (320, ccfs1),
        _ => return None,
    };
    Some(Layout::Contiguous { width, center })
}

/// Bounds-checked cursor over element data.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, tail) = (self.0.get(..n)?, self.0.get(n..)?);
        self.0 = tail;
        Some(head)
    }

    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }

    fn suite(&mut self) -> Option<u32> {
        self.take(4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Cipher suite selector (802.11-2020 Table 9-149; WPA uses 00-50-F2).
fn cipher(suite: u32) -> Cipher {
    let [a, b, c, kind] = suite.to_be_bytes();
    match ([a, b, c], kind) {
        (OUI_IEEE | OUI_MICROSOFT, 1) => Cipher::Wep40,
        (OUI_IEEE | OUI_MICROSOFT, 2) => Cipher::Tkip,
        (OUI_IEEE | OUI_MICROSOFT, 4) => Cipher::Ccmp,
        (OUI_IEEE | OUI_MICROSOFT, 5) => Cipher::Wep104,
        (OUI_IEEE, 6) => Cipher::BipCmac128,
        (OUI_IEEE, 8) => Cipher::Gcmp,
        (OUI_IEEE, 9) => Cipher::Gcmp256,
        (OUI_IEEE, 10) => Cipher::Ccmp256,
        (OUI_IEEE, 11) => Cipher::BipGmac128,
        (OUI_IEEE, 12) => Cipher::BipGmac256,
        (OUI_IEEE, 13) => Cipher::BipCmac256,
        _ => Cipher::Unknown(suite),
    }
}

/// AKM suite selector (802.11-2020 Table 9-151, 802.11-2024 for 24/25).
fn akm(suite: u32) -> Akm {
    let [a, b, c, kind] = suite.to_be_bytes();
    match ([a, b, c], kind) {
        (OUI_IEEE | OUI_MICROSOFT, 1) => Akm::Ieee8021x,
        (OUI_IEEE | OUI_MICROSOFT, 2) => Akm::Psk,
        (OUI_IEEE, 3) => Akm::FtIeee8021x,
        (OUI_IEEE, 4) => Akm::FtPsk,
        (OUI_IEEE, 5) => Akm::Ieee8021xSha256,
        (OUI_IEEE, 6) => Akm::PskSha256,
        (OUI_IEEE, 8) => Akm::Sae,
        (OUI_IEEE, 9) => Akm::FtSae,
        (OUI_IEEE, 11) => Akm::SuiteB,
        (OUI_IEEE, 12) => Akm::SuiteB192,
        (OUI_IEEE, 13) => Akm::FtSuiteB192,
        (OUI_IEEE, 14) => Akm::FilsSha256,
        (OUI_IEEE, 15) => Akm::FilsSha384,
        (OUI_IEEE, 16) => Akm::FtFilsSha256,
        (OUI_IEEE, 17) => Akm::FtFilsSha384,
        (OUI_IEEE, 18) => Akm::Owe,
        (OUI_IEEE, 19) => Akm::FtPskSha384,
        (OUI_IEEE, 20) => Akm::PskSha384,
        (OUI_IEEE, 23) => Akm::Ieee8021xSha384,
        (OUI_IEEE, 24) => Akm::SaeExtKey,
        (OUI_IEEE, 25) => Akm::FtSaeExtKey,
        _ => Akm::Unknown(suite),
    }
}

/// RSN element body (802.11-2020 §9.4.2.24) or the WPA element body after
/// its OUI and type (same layout up to the AKM list, no capabilities used).
/// Every field after Version is optional, but only at field boundaries: a
/// partial field or a count larger than the data makes the element invalid.
fn parse_rsn(data: &[u8], is_rsn: bool) -> Option<RsnInfo> {
    let mut r = Reader(data);
    let version = r.u16()?;
    if version != 1 {
        return None;
    }
    let mut info = RsnInfo {
        version,
        ..RsnInfo::default()
    };
    if r.is_empty() {
        return Some(info);
    }
    info.group_cipher = Some(cipher(r.suite()?));
    if r.is_empty() {
        return Some(info);
    }
    for _ in 0..r.u16()? {
        info.pairwise_ciphers.push(cipher(r.suite()?));
    }
    if r.is_empty() {
        return Some(info);
    }
    for _ in 0..r.u16()? {
        info.akms.push(akm(r.suite()?));
    }
    // WPA elements may carry a capabilities field too; it has no MFP bits.
    if !is_rsn || r.is_empty() {
        return Some(info);
    }
    let caps = r.u16()?;
    let (mfpr, mfpc) = (caps & 0x0040 != 0, caps & 0x0080 != 0);
    info.pmf = match (mfpc, mfpr) {
        (false, false) => Some(Pmf::Disabled),
        (true, false) => Some(Pmf::Capable),
        (true, true) => Some(Pmf::Required),
        (false, true) => None,
    };
    if r.is_empty() {
        return Some(info);
    }
    let pmkids = r.u16()? as usize;
    r.take(pmkids * 16)?;
    if r.is_empty() {
        return Some(info);
    }
    info.group_mgmt_cipher = Some(cipher(r.suite()?));
    Some(info)
}

/// HE Operation (802.11ax-2021 §9.4.2.249). `data` starts after the
/// Element ID Extension byte.
fn parse_he_operation(data: &[u8], s: &mut ElementSummary) {
    let mut r = Reader(data);
    let Some(params) = r.take(3) else {
        return;
    };
    let params = u32::from_le_bytes([params[0], params[1], params[2], 0]);
    // BSS Color Information (1) + Basic HE-MCS And NSS Set (2).
    if r.take(3).is_none() {
        return;
    }
    if params & (1 << 14) != 0 {
        let Some(v) = r.take(3) else {
            return;
        };
        s.he_vht_operation = Some(ChannelOperation {
            width: v[0],
            ccfs0: v[1],
            ccfs1: v[2],
        });
    }
    // Co-Hosted BSS: Max Co-Hosted BSSID Indicator (1).
    if params & (1 << 15) != 0 && r.take(1).is_none() {
        return;
    }
    if params & (1 << 17) != 0 {
        if let Some(o) = r.take(5) {
            s.he_6ghz_operation = Some(He6GhzOperation {
                primary: o[0],
                width: o[1] & 0b11,
                ccfs0: o[2],
                ccfs1: o[3],
            });
        }
    }
}

/// EHT Operation (802.11be-2024 §9.4.2.311). `data` starts after the
/// Element ID Extension byte.
fn parse_eht_operation(data: &[u8]) -> Option<ChannelOperation> {
    let mut r = Reader(data);
    let params = r.take(1)?[0];
    // Basic EHT-MCS And NSS Set (4).
    r.take(4)?;
    // EHT Operation Information Present.
    if params & 1 == 0 {
        return None;
    }
    let o = r.take(3)?;
    Some(ChannelOperation {
        width: o[0] & 0b111,
        ccfs0: o[1],
        ccfs1: o[2],
    })
}

/// Basic Multi-Link element (802.11be-2024 §9.4.2.312): MLD MAC address
/// from the Common Info. `data` starts after the Element ID Extension byte.
fn parse_mld_address(data: &[u8]) -> Option<[u8; 6]> {
    let mut r = Reader(data);
    let control = r.u16()?;
    // Type 0 = Basic; the others (probe request, reconfiguration, ...) carry
    // no MLD address of the transmitting AP.
    if control & 0b111 != 0 {
        return None;
    }
    let info_len = r.take(1)?[0] as usize;
    // Common Info Length counts itself and the 6-byte MLD MAC address.
    if info_len < 7 || r.0.len() < info_len - 1 {
        return None;
    }
    r.take(6)?.try_into().ok()
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
            // data[0] = primary channel; data[1] bits 0-1 = secondary offset;
            // data[2..4] (operation mode) bits 5-12 = CCFS2.
            EID_HT_OPERATION if data.len() >= 2 && s.ht_operation.is_none() => {
                s.ht_secondary_offset = match data[1] & 0b11 {
                    1 => Some(1),
                    3 => Some(-1),
                    _ => None,
                };
                let ccfs2 = match data.get(2..4) {
                    Some(m) => ((u16::from_le_bytes([m[0], m[1]]) >> 5) & 0xFF) as u8,
                    None => 0,
                };
                s.ht_operation = Some(HtOperation {
                    primary: data[0],
                    secondary_offset: data[1] & 0b11,
                    ccfs2,
                });
            }
            // Channel Width, CCFS0, CCFS1, Basic VHT-MCS And NSS Set (2).
            EID_VHT_OPERATION if data.len() >= 3 && s.vht_operation.is_none() => {
                s.vht_operation = Some(ChannelOperation {
                    width: data[0],
                    ccfs0: data[1],
                    ccfs1: data[2],
                });
            }
            EID_BSS_LOAD if data.len() >= 3 => {
                s.station_count = Some(u16::from_le_bytes([data[0], data[1]]));
                s.channel_utilization_raw = Some(data[2]);
            }
            EID_RSN if !s.rsn_present => {
                s.rsn_present = true;
                s.rsn = parse_rsn(data, true);
            }
            EID_VENDOR => match data.get(..4) {
                Some([a, b, c, MICROSOFT_WPA])
                    if [*a, *b, *c] == OUI_MICROSOFT && !s.wpa_present =>
                {
                    s.wpa_present = true;
                    s.wpa = parse_rsn(&data[4..], false);
                }
                Some([a, b, c, WFA_OWE_TRANSITION]) if [*a, *b, *c] == OUI_WFA => {
                    s.owe_transition = true;
                }
                _ => {}
            },
            EID_EXTENSION => match data.split_first() {
                Some((&EXT_HE_CAP, _)) => s.he = true,
                Some((&EXT_EHT_CAP, _)) => s.eht = true,
                Some((&EXT_HE_OPERATION, body)) if s.he_6ghz_operation.is_none() => {
                    parse_he_operation(body, &mut s)
                }
                Some((&EXT_EHT_OPERATION, body)) if s.eht_operation.is_none() => {
                    s.eht_operation = parse_eht_operation(body)
                }
                Some((&EXT_MULTI_LINK, body)) if s.mld_addr.is_none() => {
                    s.mld_addr = parse_mld_address(body)
                }
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
    use crate::wifi::models::SecurityKind;

    /// Wraps `body` in an element header.
    fn ie(id: u8, body: &[u8]) -> Vec<u8> {
        let mut v = vec![id, body.len() as u8];
        v.extend_from_slice(body);
        v
    }

    fn ext(ext_id: u8, body: &[u8]) -> Vec<u8> {
        let mut b = vec![ext_id];
        b.extend_from_slice(body);
        ie(EID_EXTENSION, &b)
    }

    fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    /// HT Operation for `primary` with raw secondary offset, full 22 bytes.
    fn ht_op(primary: u8, offset: u8, ccfs2: u8) -> Vec<u8> {
        let mut b = vec![0u8; 22];
        b[0] = primary;
        // Secondary offset + STA Channel Width (any) when offset is set.
        b[1] = offset | if offset != 0 { 0b100 } else { 0 };
        let mode = (ccfs2 as u16) << 5;
        b[2..4].copy_from_slice(&mode.to_le_bytes());
        ie(EID_HT_OPERATION, &b)
    }

    fn vht_op(width: u8, ccfs0: u8, ccfs1: u8) -> Vec<u8> {
        ie(EID_VHT_OPERATION, &[width, ccfs0, ccfs1, 0xFC, 0xFF])
    }

    // RSN bodies as an AP would send them.
    const S_CCMP: [u8; 4] = [0x00, 0x0F, 0xAC, 4];
    const S_TKIP: [u8; 4] = [0x00, 0x0F, 0xAC, 2];
    const S_GCMP256: [u8; 4] = [0x00, 0x0F, 0xAC, 9];
    const S_BIP_CMAC: [u8; 4] = [0x00, 0x0F, 0xAC, 6];
    const S_BIP_GMAC256: [u8; 4] = [0x00, 0x0F, 0xAC, 12];

    fn akm_s(kind: u8) -> [u8; 4] {
        [0x00, 0x0F, 0xAC, kind]
    }

    fn rsn(group: [u8; 4], pairwise: &[[u8; 4]], akms: &[[u8; 4]], tail: &[u8]) -> Vec<u8> {
        let mut b = vec![1, 0];
        b.extend_from_slice(&group);
        b.extend_from_slice(&(pairwise.len() as u16).to_le_bytes());
        pairwise.iter().for_each(|s| b.extend_from_slice(s));
        b.extend_from_slice(&(akms.len() as u16).to_le_bytes());
        akms.iter().for_each(|s| b.extend_from_slice(s));
        b.extend_from_slice(tail);
        ie(EID_RSN, &b)
    }

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

    fn span(ies: &[u8], primary_mhz: u32) -> (Option<u32>, Option<u32>) {
        let s = summarise(ies).channel_span(primary_mhz);
        (s.width_mhz, s.center_mhz)
    }

    #[test]
    fn ht_width_2ghz() {
        // Channel 6, secondary above / below / none.
        assert_eq!(span(&ht_op(6, 1, 0), 2437), (Some(40), Some(2447)));
        assert_eq!(span(&ht_op(6, 3, 0), 2437), (Some(40), Some(2427)));
        assert_eq!(span(&ht_op(6, 0, 0), 2437), (Some(20), Some(2437)));
        // Reserved offset, or an HT primary that isn't the reported one.
        assert_eq!(span(&ht_op(6, 2, 0), 2437), (None, None));
        assert_eq!(span(&ht_op(5, 0, 0), 2437), (None, None));
        // HT40- on channel 1 has no channel below.
        assert_eq!(span(&ht_op(1, 3, 0), 2412), (None, None));
        // Vendor VHT Operation on 2.4 GHz is ignored.
        let ies = cat(&[ht_op(6, 0, 0), vht_op(1, 42, 0)]);
        assert_eq!(span(&ies, 2437), (Some(20), Some(2437)));
        // No operation elements: unknown, not 20 MHz.
        assert_eq!(span(&ie(45, &[0; 26]), 2437), (None, None));
    }

    #[test]
    fn vht_80() {
        // Channel 36, 80 MHz block 36–48 centred on 42.
        let ies = cat(&[ht_op(36, 1, 0), vht_op(1, 42, 0)]);
        assert_eq!(span(&ies, 5180), (Some(80), Some(5210)));
        // Channel 149 → centre 155.
        let ies = cat(&[ht_op(149, 1, 0), vht_op(1, 155, 0)]);
        assert_eq!(span(&ies, 5745), (Some(80), Some(5775)));
        // VHT width 0 defers to HT: 40 MHz.
        let ies = cat(&[ht_op(36, 1, 0), vht_op(0, 38, 0)]);
        assert_eq!(span(&ies, 5180), (Some(40), Some(5190)));
        // Centre that doesn't hold the primary, or isn't on the raster.
        let ies = cat(&[ht_op(36, 1, 0), vht_op(1, 58, 0)]);
        assert_eq!(span(&ies, 5180), (None, None));
        let ies = cat(&[ht_op(36, 1, 0), vht_op(1, 46, 0)]);
        assert_eq!(span(&ies, 5180), (None, None));
        // Unknown width value.
        let ies = cat(&[ht_op(36, 1, 0), vht_op(7, 42, 0)]);
        assert_eq!(span(&ies, 5180), (None, None));
    }

    #[test]
    fn vht_160_and_80_80() {
        // Current encoding: CCFS0 = primary 80 centre, CCFS1 = 160 centre.
        let ies = cat(&[ht_op(36, 1, 0), vht_op(1, 42, 50)]);
        assert_eq!(span(&ies, 5180), (Some(160), Some(5250)));
        // Primary in the upper 80: channel 64, CCFS0 58.
        let ies = cat(&[ht_op(64, 3, 0), vht_op(1, 58, 50)]);
        assert_eq!(span(&ies, 5320), (Some(160), Some(5250)));
        // Deprecated width 2.
        let ies = cat(&[ht_op(36, 1, 0), vht_op(2, 50, 0)]);
        assert_eq!(span(&ies, 5180), (Some(160), Some(5250)));
        // Extended NSS BW: 160 MHz centre in HT Operation CCFS2.
        let ies = cat(&[ht_op(36, 1, 50), vht_op(1, 42, 0)]);
        assert_eq!(span(&ies, 5180), (Some(160), Some(5250)));
        // 80+80 (42 + 106): 160 MHz, no single centre.
        let ies = cat(&[ht_op(36, 1, 0), vht_op(1, 42, 106)]);
        assert_eq!(span(&ies, 5180), (Some(160), None));
        let ies = cat(&[ht_op(36, 1, 0), vht_op(3, 42, 106)]);
        assert_eq!(span(&ies, 5180), (Some(160), None));
        // CCFS1 neither 8 nor >16 away: inconsistent.
        let ies = cat(&[ht_op(36, 1, 0), vht_op(1, 42, 54)]);
        assert_eq!(span(&ies, 5180), (None, None));
    }

    /// HE Operation with 6 GHz Operation Information (and optionally the VHT
    /// information and co-hosted indicator before it).
    fn he_op_6ghz(primary: u8, control: u8, ccfs0: u8, ccfs1: u8, extras: bool) -> Vec<u8> {
        let mut params: u32 = 1 << 17;
        if extras {
            params |= (1 << 14) | (1 << 15);
        }
        let mut b = params.to_le_bytes()[..3].to_vec();
        b.extend_from_slice(&[0x01, 0xFC, 0xFF]); // BSS colour, basic MCS
        if extras {
            b.extend_from_slice(&[0, 0, 0]); // VHT Operation Information
            b.push(3); // Max Co-Hosted BSSID Indicator
        }
        b.extend_from_slice(&[primary, control, ccfs0, ccfs1, 6]);
        ext(EXT_HE_OPERATION, &b)
    }

    #[test]
    fn he_6ghz() {
        // Channel 37 (6135 MHz) at 160 MHz: block 33–61, centre 47.
        let ies = he_op_6ghz(37, 3 | 0b100, 39, 47, false);
        assert_eq!(span(&ies, 6135), (Some(160), Some(6185)));
        assert_eq!(
            span(&he_op_6ghz(37, 3, 39, 47, true), 6135),
            (Some(160), Some(6185))
        );
        // 80 MHz on channel 5 → centre 7; 20 MHz on channel 1.
        assert_eq!(
            span(&he_op_6ghz(5, 2, 7, 0, false), 5975),
            (Some(80), Some(5985))
        );
        assert_eq!(
            span(&he_op_6ghz(1, 0, 1, 0, false), 5955),
            (Some(20), Some(5955))
        );
        // Primary mismatch, 160 without CCFS1, misaligned 80.
        assert_eq!(span(&he_op_6ghz(33, 3, 39, 47, false), 6135), (None, None));
        assert_eq!(span(&he_op_6ghz(37, 3, 39, 0, false), 6135), (None, None));
        assert_eq!(span(&he_op_6ghz(37, 2, 35, 0, false), 6135), (None, None));
        // 6 GHz without HE 6 GHz information: unknown.
        assert_eq!(span(&ht_op(37, 0, 0), 6135), (None, None));
        // HE Operation with its own VHT information on 5 GHz (no VHT element).
        let mut b = (1u32 << 14).to_le_bytes()[..3].to_vec();
        b.extend_from_slice(&[0x01, 0xFC, 0xFF, 1, 42, 0]);
        let ies = cat(&[ht_op(36, 1, 0), ext(EXT_HE_OPERATION, &b)]);
        assert_eq!(span(&ies, 5180), (Some(80), Some(5210)));
    }

    fn eht_op(control: u8, ccfs0: u8, ccfs1: u8) -> Vec<u8> {
        ext(
            EXT_EHT_OPERATION,
            &[0x01, 0x44, 0x44, 0x44, 0x44, control, ccfs0, ccfs1],
        )
    }

    #[test]
    fn eht_320() {
        // Channel 37 in 320 MHz channel 63 (33–93); primary 160 centred on 47.
        let ies = cat(&[he_op_6ghz(37, 3, 39, 47, false), eht_op(4, 47, 63)]);
        assert_eq!(span(&ies, 6135), (Some(320), Some(6265)));
        // The other channelisation: 320 MHz channel 31 (1–61).
        let ies = cat(&[he_op_6ghz(37, 3, 39, 47, false), eht_op(4, 47, 31)]);
        assert_eq!(span(&ies, 6135), (Some(320), Some(6105)));
        // EHT 160 on 5 GHz overrides VHT 80.
        let ies = cat(&[ht_op(36, 1, 0), vht_op(1, 42, 0), eht_op(3, 42, 50)]);
        assert_eq!(span(&ies, 5180), (Some(160), Some(5250)));
        // 320 centre not on either raster, or not holding the primary.
        assert_eq!(span(&eht_op(4, 47, 47), 6135), (None, None));
        assert_eq!(span(&eht_op(4, 111, 127), 6135), (None, None));
        // 320 MHz doesn't exist on 5 GHz.
        assert_eq!(span(&eht_op(4, 50, 66), 5180), (None, None));
        // No EHT Operation Information: falls back to HE.
        let no_info = ext(EXT_EHT_OPERATION, &[0x00, 0x44, 0x44, 0x44, 0x44]);
        let ies = cat(&[he_op_6ghz(37, 3, 39, 47, false), no_info]);
        assert_eq!(span(&ies, 6135), (Some(160), Some(6185)));
    }

    #[test]
    fn wpa2_psk() {
        let ies = rsn(S_CCMP, &[S_CCMP], &[akm_s(2)], &[0x00, 0x00]);
        let s = summarise(&ies).security(true).unwrap();
        assert_eq!(s.kind, SecurityKind::Wpa2Personal);
        assert_eq!(s.akms, vec![Akm::Psk]);
        assert_eq!(s.pairwise_ciphers, vec![Cipher::Ccmp]);
        assert_eq!(s.group_ciphers, vec![Cipher::Ccmp]);
        assert_eq!(s.pmf, Some(Pmf::Disabled));
        assert_eq!(s.group_mgmt_cipher, None);
        assert!(s.rsn && !s.wpa && s.privacy);
        assert_eq!(summarise(&ies).rsn.unwrap().version, 1);
    }

    #[test]
    fn wpa2_wpa3_transition_pmf_capable() {
        let ies = rsn(S_CCMP, &[S_CCMP], &[akm_s(2), akm_s(8)], &[0x80, 0x00]);
        let s = summarise(&ies).security(true).unwrap();
        assert_eq!(s.kind, SecurityKind::Wpa2Wpa3Personal);
        assert_eq!(s.akms, vec![Akm::Psk, Akm::Sae]);
        assert_eq!(s.pmf, Some(Pmf::Capable));
    }

    #[test]
    fn wpa3_sae_pmf_required() {
        // Capabilities MFPR|MFPC, no PMKIDs, BIP-CMAC-128.
        let mut tail = vec![0xC0, 0x00, 0x00, 0x00];
        tail.extend_from_slice(&S_BIP_CMAC);
        let ies = rsn(S_CCMP, &[S_CCMP], &[akm_s(8), akm_s(24)], &tail);
        let s = summarise(&ies).security(true).unwrap();
        assert_eq!(s.kind, SecurityKind::Wpa3Personal);
        assert_eq!(s.akms, vec![Akm::Sae, Akm::SaeExtKey]);
        assert_eq!(s.pmf, Some(Pmf::Required));
        assert_eq!(s.group_mgmt_cipher, Some(Cipher::BipCmac128));
        // A PMKID before the group management cipher is skipped.
        let mut tail = vec![0xC0, 0x00, 0x01, 0x00];
        tail.extend_from_slice(&[0xAA; 16]);
        tail.extend_from_slice(&S_BIP_CMAC);
        let ies = rsn(S_CCMP, &[S_CCMP], &[akm_s(8)], &tail);
        let s = summarise(&ies).security(true).unwrap();
        assert_eq!(s.group_mgmt_cipher, Some(Cipher::BipCmac128));
    }

    #[test]
    fn owe_and_transition() {
        let ies = rsn(S_CCMP, &[S_CCMP], &[akm_s(18)], &[0xC0, 0x00]);
        let s = summarise(&ies).security(true).unwrap();
        assert_eq!(s.kind, SecurityKind::Owe);
        assert_eq!(s.akms, vec![Akm::Owe]);
        // Open half: no RSN, OWE Transition Mode element naming the OWE BSS.
        let mut body = vec![0x50, 0x6F, 0x9A, 0x1C, 0x02, 0x11, 0x22, 0x33, 0x44, 0x55];
        body.extend_from_slice(&[4, b'o', b'w', b'e', b'!']);
        let s = summarise(&ie(EID_VENDOR, &body)).security(false).unwrap();
        assert_eq!(s.kind, SecurityKind::Owe);
        assert_eq!(s.akms, vec![Akm::OweTransition]);
        assert!(!s.rsn && !s.wpa);
    }

    #[test]
    fn enterprise() {
        let akms = [akm_s(1), akm_s(3), akm_s(5)];
        let ies = rsn(S_CCMP, &[S_CCMP], &akms, &[0x80, 0x00]);
        let s = summarise(&ies).security(true).unwrap();
        assert_eq!(s.kind, SecurityKind::Wpa2Enterprise);
        assert_eq!(
            s.akms,
            vec![Akm::Ieee8021x, Akm::FtIeee8021x, Akm::Ieee8021xSha256]
        );
        // WPA3-Enterprise 192-bit: GCMP-256, BIP-GMAC-256, PMF required.
        let mut tail = vec![0xC0, 0x00, 0x00, 0x00];
        tail.extend_from_slice(&S_BIP_GMAC256);
        let ies = rsn(S_GCMP256, &[S_GCMP256], &[akm_s(12)], &tail);
        let s = summarise(&ies).security(true).unwrap();
        assert_eq!(s.kind, SecurityKind::Wpa3Enterprise);
        assert_eq!(s.pairwise_ciphers, vec![Cipher::Gcmp256]);
        assert_eq!(s.group_mgmt_cipher, Some(Cipher::BipGmac256));
        assert_eq!(s.pmf, Some(Pmf::Required));
    }

    #[test]
    fn wpa_vendor_element_and_mixed_mode() {
        // WPA: TKIP group, TKIP pairwise, PSK.
        let wpa = ie(
            EID_VENDOR,
            &[
                0x00, 0x50, 0xF2, 0x01, 0x01, 0x00, 0x00, 0x50, 0xF2, 0x02, 0x01, 0x00, 0x00, 0x50,
                0xF2, 0x02, 0x01, 0x00, 0x00, 0x50, 0xF2, 0x02,
            ],
        );
        let s = summarise(&wpa).security(true).unwrap();
        assert_eq!(s.kind, SecurityKind::WpaPersonal);
        assert_eq!(s.akms, vec![Akm::Psk]);
        assert_eq!(s.pairwise_ciphers, vec![Cipher::Tkip]);
        assert_eq!(s.pmf, None);
        assert!(s.wpa && !s.rsn);
        // WPA/WPA2 mixed: RSN with TKIP group and CCMP+TKIP pairwise.
        let ies = cat(&[rsn(S_TKIP, &[S_CCMP, S_TKIP], &[akm_s(2)], &[0, 0]), wpa]);
        let s = summarise(&ies).security(true).unwrap();
        assert_eq!(s.kind, SecurityKind::Wpa2Personal);
        assert_eq!(s.akms, vec![Akm::Psk]);
        assert_eq!(s.pairwise_ciphers, vec![Cipher::Ccmp, Cipher::Tkip]);
        assert_eq!(s.group_ciphers, vec![Cipher::Tkip]);
        assert!(s.wpa && s.rsn);
        // Other vendor elements (WMM) are not WPA.
        let wmm = ie(EID_VENDOR, &[0x00, 0x50, 0xF2, 0x02, 0x01, 0x01, 0x00]);
        assert!(!summarise(&wmm).wpa_present);
    }

    #[test]
    fn unknown_suites_are_kept() {
        let ies = rsn(
            [0x00, 0x0F, 0xAC, 7],
            &[[0x00, 0x0F, 0xAC, 0x63]],
            &[akm_s(21), [0x00, 0x40, 0x96, 0x00]],
            &[],
        );
        let s = summarise(&ies).security(true).unwrap();
        assert_eq!(
            s.akms,
            vec![Akm::Unknown(0x000F_AC15), Akm::Unknown(0x0040_9600)]
        );
        assert_eq!(s.pairwise_ciphers, vec![Cipher::Unknown(0x000F_AC63)]);
        assert_eq!(s.group_ciphers, vec![Cipher::Unknown(0x000F_AC07)]);
        // No capabilities field: PMF unknown, not "disabled".
        assert_eq!(s.pmf, None);
        assert_eq!(s.kind, SecurityKind::Unknown);
        let json = serde_json::to_string(&s.akms).unwrap();
        assert_eq!(json, "[1027093,4232704]");
        assert_eq!(serde_json::from_str::<Vec<Akm>>(&json).unwrap(), s.akms);
        assert_eq!(
            serde_json::to_string(&Akm::SaeExtKey).unwrap(),
            "\"sae_ext_key\""
        );
    }

    #[test]
    fn open_and_wep_without_elements() {
        let s = summarise(&[0, 0]).security(false).unwrap();
        assert_eq!(s.kind, SecurityKind::Open);
        let s = summarise(&[]).security(true).unwrap();
        assert_eq!(s.kind, SecurityKind::Wep);
    }

    #[test]
    fn malformed_rsn() {
        let good = rsn(S_CCMP, &[S_CCMP], &[akm_s(2)], &[0x00, 0x00]);
        // Version 2.
        let mut v2 = good.clone();
        v2[2] = 2;
        let s = summarise(&v2);
        assert!(s.rsn_present && s.rsn.is_none());
        assert_eq!(s.security(true), None);
        // Pairwise count larger than the data.
        let mut b = vec![1, 0];
        b.extend_from_slice(&S_CCMP);
        b.extend_from_slice(&[5, 0]);
        b.extend_from_slice(&S_CCMP);
        assert_eq!(summarise(&ie(EID_RSN, &b)).rsn, None);
        // Every truncation inside a field is rejected, every truncation at a
        // field boundary accepted (re-wrapped so the element length matches).
        let body = &good[2..];
        for n in 0..=body.len() {
            let s = summarise(&ie(EID_RSN, &body[..n]));
            let ok = matches!(n, 2 | 6 | 12 | 18 | 20);
            assert_eq!(s.rsn.is_some(), ok, "length {n}");
        }
        // Version only: present and valid, but nothing to classify.
        let s = summarise(&ie(EID_RSN, &[1, 0])).security(true).unwrap();
        assert_eq!(s.kind, SecurityKind::Unknown);
        assert!(s.akms.is_empty() && s.rsn);
        // MFPR without MFPC is invalid: PMF unknown.
        let ies = rsn(S_CCMP, &[S_CCMP], &[akm_s(8)], &[0x40, 0x00]);
        assert_eq!(summarise(&ies).rsn.unwrap().pmf, None);
        // Huge PMKID count.
        let ies = rsn(S_CCMP, &[S_CCMP], &[akm_s(8)], &[0xC0, 0x00, 0xFF, 0xFF]);
        assert_eq!(summarise(&ies).rsn, None);
    }

    fn ml(control: [u8; 2], common: &[u8]) -> Vec<u8> {
        let mut b = control.to_vec();
        b.extend_from_slice(common);
        ext(EXT_MULTI_LINK, &b)
    }

    #[test]
    fn multi_link_mld_address() {
        // Basic, Link ID Info + BSS Parameters Change Count present.
        let common = [9, 0x02, 0x1A, 0x2B, 0x3C, 0x4D, 0x5E, 0x01, 0x00];
        let s = summarise(&ml([0x30, 0x00], &common));
        assert_eq!(s.mld_address().as_deref(), Some("02:1A:2B:3C:4D:5E"));
        // Probe Request variant (type 1): no address.
        assert_eq!(summarise(&ml([0x31, 0x00], &common)).mld_addr, None);
        // Common Info too short to hold the address, or truncated.
        assert_eq!(summarise(&ml([0x00, 0x00], &[3, 1, 2])).mld_addr, None);
        assert_eq!(summarise(&ml([0x30, 0x00], &common[..5])).mld_addr, None);
        assert_eq!(
            summarise(&ml([0x30, 0x00], &[12, 2, 0, 0, 0, 0, 0])).mld_addr,
            None
        );
        assert_eq!(summarise(&ext(EXT_MULTI_LINK, &[0x00])).mld_addr, None);
        assert_eq!(summarise(&[]).mld_address(), None);
    }

    #[test]
    fn truncated_input_is_safe() {
        let s = summarise(&[45, 10, 1, 2]);
        assert!(!s.ht);
        assert_eq!(summarise(&[]), ElementSummary::default());
        assert_eq!(summarise(&[255]), ElementSummary::default());
        assert!(!summarise(&[255, 0]).he);
        // Short operation elements are ignored, not misread.
        assert_eq!(span(&ie(EID_VHT_OPERATION, &[1, 42]), 5180), (None, None));
        assert_eq!(
            span(&ext(EXT_HE_OPERATION, &[0, 0, 2, 1]), 6135),
            (None, None)
        );
        assert_eq!(
            span(&ext(EXT_EHT_OPERATION, &[1, 0, 0, 0, 0, 4]), 6135),
            (None, None)
        );
        assert_eq!(span(&ext(EXT_HE_OPERATION, &[]), 6135), (None, None));
    }

    /// Every prefix and a stream of pseudo-random bytes must parse without
    /// panicking, whatever frequency the result is evaluated against.
    #[test]
    fn garbage_never_panics() {
        let mut tail = vec![0xC0, 0x00, 0x00, 0x00];
        tail.extend_from_slice(&S_BIP_CMAC);
        let rich = cat(&[
            ht_op(36, 1, 50),
            vht_op(1, 42, 50),
            he_op_6ghz(37, 3, 39, 47, true),
            eht_op(4, 47, 63),
            rsn(S_CCMP, &[S_CCMP], &[akm_s(8), akm_s(24)], &tail),
            ml([0x30, 0x00], &[9, 2, 3, 4, 5, 6, 7, 1, 0]),
        ]);
        let freqs = [0, 2412, 2484, 5180, 5935, 6135, 7115, 58320, u32::MAX];
        let check = |bytes: &[u8]| {
            let s = summarise(bytes);
            for f in freqs {
                let _ = s.channel_span(f);
            }
            let _ = s.security(true);
            let _ = s.mld_address();
        };
        for n in 0..=rich.len() {
            check(&rich[..n]);
            check(&rich[n..]);
        }
        let mut x: u32 = 0x1234_5678;
        let mut buf = Vec::with_capacity(512);
        for round in 0..3000 {
            buf.clear();
            for _ in 0..(round % 300) {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                // Bias towards the element IDs that have parsers.
                let b = match x % 7 {
                    0 => [48, 61, 192, 221, 255][(x >> 8) as usize % 5],
                    1 => [36, 106, 107][(x >> 8) as usize % 3],
                    _ => (x >> 16) as u8,
                };
                buf.push(b);
            }
            check(&buf);
        }
    }
}
