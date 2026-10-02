//! Provider-independent security classification.
//!
//! Providers fill [`Security`]'s detail (AKMs, ciphers, element presence)
//! from whatever they have, NetworkManager flags or raw RSN/WPA elements,
//! and derive the summary [`SecurityKind`] here, so the same AP gets the same
//! label whichever way it was read.

use super::models::{Akm, Security, SecurityKind};

fn is_suite_b_192(a: Akm) -> bool {
    matches!(a, Akm::SuiteB192 | Akm::FtSuiteB192)
}

fn is_8021x(a: Akm) -> bool {
    matches!(
        a,
        Akm::Ieee8021x
            | Akm::Ieee8021xSha256
            | Akm::Ieee8021xSha384
            | Akm::FtIeee8021x
            | Akm::SuiteB
            | Akm::FilsSha256
            | Akm::FilsSha384
            | Akm::FtFilsSha256
            | Akm::FtFilsSha384
    )
}

fn is_sae(a: Akm) -> bool {
    matches!(a, Akm::Sae | Akm::SaeExtKey | Akm::FtSae | Akm::FtSaeExtKey)
}

fn is_psk(a: Akm) -> bool {
    matches!(
        a,
        Akm::Psk | Akm::PskSha256 | Akm::PskSha384 | Akm::FtPsk | Akm::FtPskSha384
    )
}

fn is_owe(a: Akm) -> bool {
    matches!(a, Akm::Owe | Akm::OweTransition)
}

/// Summary label from the privacy bit, element presence and AKMs (`kind` is
/// ignored). Enterprise wins over personal when both are offered, as a
/// client picking the strongest suite would; WPA3-Enterprise means the
/// 192-bit (Suite B) mode only, as in [`SecurityKind`]. Ciphers don't change
/// the label: a TKIP-only RSN network is still WPA2 by its AKM.
pub fn classify(s: &Security) -> SecurityKind {
    let has = |f: fn(Akm) -> bool| s.akms.iter().any(|a| f(*a));
    if !s.wpa && !s.rsn && s.akms.is_empty() {
        if s.privacy {
            SecurityKind::Wep
        } else {
            SecurityKind::Open
        }
    } else if has(is_suite_b_192) {
        SecurityKind::Wpa3Enterprise
    } else if has(is_8021x) {
        if s.rsn {
            SecurityKind::Wpa2Enterprise
        } else {
            SecurityKind::WpaEnterprise
        }
    } else if has(is_sae) && has(is_psk) {
        SecurityKind::Wpa2Wpa3Personal
    } else if has(is_sae) {
        SecurityKind::Wpa3Personal
    } else if has(is_psk) {
        if s.rsn {
            SecurityKind::Wpa2Personal
        } else {
            SecurityKind::WpaPersonal
        }
    } else if has(is_owe) {
        SecurityKind::Owe
    } else {
        SecurityKind::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sec(privacy: bool, wpa: bool, rsn: bool, akms: &[Akm]) -> Security {
        Security {
            privacy,
            wpa,
            rsn,
            akms: akms.to_vec(),
            ..Security::unknown()
        }
    }

    #[test]
    fn open_and_wep() {
        assert_eq!(classify(&sec(false, false, false, &[])), SecurityKind::Open);
        assert_eq!(classify(&sec(true, false, false, &[])), SecurityKind::Wep);
        // Open BSS of an OWE transition pair: no RSN element, only the
        // transition element.
        assert_eq!(
            classify(&sec(false, false, false, &[Akm::OweTransition])),
            SecurityKind::Owe
        );
    }

    #[test]
    fn personal() {
        let k = |wpa, rsn, akms: &[Akm]| classify(&sec(true, wpa, rsn, akms));
        assert_eq!(k(true, false, &[Akm::Psk]), SecurityKind::WpaPersonal);
        assert_eq!(k(false, true, &[Akm::Psk]), SecurityKind::Wpa2Personal);
        assert_eq!(k(true, true, &[Akm::Psk]), SecurityKind::Wpa2Personal);
        assert_eq!(
            k(false, true, &[Akm::PskSha256]),
            SecurityKind::Wpa2Personal
        );
        assert_eq!(k(false, true, &[Akm::FtPsk]), SecurityKind::Wpa2Personal);
        assert_eq!(k(false, true, &[Akm::Sae]), SecurityKind::Wpa3Personal);
        assert_eq!(
            k(false, true, &[Akm::SaeExtKey, Akm::FtSaeExtKey]),
            SecurityKind::Wpa3Personal
        );
        assert_eq!(
            k(false, true, &[Akm::Psk, Akm::Sae]),
            SecurityKind::Wpa2Wpa3Personal
        );
        assert_eq!(
            k(false, true, &[Akm::FtPsk, Akm::FtSae]),
            SecurityKind::Wpa2Wpa3Personal
        );
        assert_eq!(k(false, true, &[Akm::Owe]), SecurityKind::Owe);
    }

    #[test]
    fn enterprise() {
        let k = |wpa, rsn, akms: &[Akm]| classify(&sec(true, wpa, rsn, akms));
        assert_eq!(
            k(true, false, &[Akm::Ieee8021x]),
            SecurityKind::WpaEnterprise
        );
        assert_eq!(
            k(false, true, &[Akm::Ieee8021x]),
            SecurityKind::Wpa2Enterprise
        );
        assert_eq!(
            k(
                false,
                true,
                &[Akm::Ieee8021x, Akm::FtIeee8021x, Akm::Ieee8021xSha256]
            ),
            SecurityKind::Wpa2Enterprise
        );
        // Enterprise wins over PSK on a mixed BSS.
        assert_eq!(
            k(false, true, &[Akm::Psk, Akm::Ieee8021x]),
            SecurityKind::Wpa2Enterprise
        );
        assert_eq!(
            k(false, true, &[Akm::SuiteB192]),
            SecurityKind::Wpa3Enterprise
        );
        assert_eq!(
            k(false, true, &[Akm::FtSuiteB192]),
            SecurityKind::Wpa3Enterprise
        );
    }

    #[test]
    fn unknown_suites() {
        assert_eq!(
            classify(&sec(true, false, true, &[Akm::Unknown(0x000F_AC15)])),
            SecurityKind::Unknown
        );
        // RSN element with no AKM list: present but unclassifiable.
        assert_eq!(
            classify(&sec(true, false, true, &[])),
            SecurityKind::Unknown
        );
    }
}
