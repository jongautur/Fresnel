//! Rogue / evil-twin detection over a project's stored survey data.
//!
//! Everything here is a pure function of what was measured: placed APs and
//! their linked BSSIDs, the samples at every point, the user's BSSID marks
//! and the readings Measure Here set aside (one BSSID on two frequencies in
//! one scan). Loading lives in `database::findings`; the result is a plain
//! serialisable [`Findings`] for the UI and the report.
//!
//! Terms:
//!
//! * **Linked**: a BSSID linked to a placed AP anywhere in the project.
//! * **Probable**: not linked, but shares the Wi-Fi 7 MLD address of a
//!   linked BSSID, or else differs from one only in the first and/or last
//!   octet (the `apKey` heuristic from `src/lib/heatmap.ts`; labelled as a
//!   heuristic wherever it is shown).
//! * **Yours**: linked, probable, or marked "ours, not placed yet".
//! * **Project SSIDs**: SSIDs (raw bytes) broadcast by linked BSSIDs.
//!
//! BSSIDs marked "neighbour" or "ignored" are left out of every rule. A link
//! to a placed AP wins over a stale mark.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::models::{PlacedAp, Sample, SurveyPoint};
use crate::wifi::models::{Akm, Band, Cipher, Pmf, SecurityKind, Signal};

// ---------------------------------------------------------------------------
// Stored inputs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnomalyKind {
    /// The BSSID was listed on another frequency too, in the same scan.
    MultiFrequency,
}

/// A reading Measure Here dropped while keeping one sample per BSSID that
/// is evidence in itself. The kept reading is the point's sample.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PointAnomaly {
    pub bssid: String,
    pub kind: AnomalyKind,
    pub frequency_mhz: u32,
    pub channel: Option<u16>,
    pub band: Band,
    pub ssid_raw: Vec<u8>,
    pub signal: Signal,
}

/// How the user classified a BSSID, once per project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkStatus {
    /// One of ours that isn't placed on a plan (yet).
    OursUnplaced,
    Neighbour,
    Ignored,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BssidMark {
    /// Uppercase, colon-separated.
    pub bssid: String,
    pub status: MarkStatus,
    pub note: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// What to report on. Ownership (linked, project SSIDs) is always decided
/// project-wide; the scope only limits where evidence must come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FindingScope {
    Project { id: i64 },
    Building { id: i64 },
    Floor { id: i64 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FloorRef {
    pub id: i64,
    pub name: String,
    pub building_id: i64,
    pub building_name: String,
}

/// Everything the analysis reads, for one project.
#[derive(Debug, Clone, Default)]
pub struct ProjectSurvey {
    pub project_id: i64,
    pub floors: Vec<FloorRef>,
    /// Oldest first (`measured_at`, then id), as the UI numbers them.
    pub points: Vec<SurveyPoint>,
    pub aps: Vec<PlacedAp>,
    pub marks: Vec<BssidMark>,
    /// (point id, anomaly).
    pub anomalies: Vec<(i64, PointAnomaly)>,
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    UnknownTransmitter,
    SecurityMismatch,
    MultiFrequency,
    BssidInconsistent,
    UnlinkedRadio,
}

/// Warnings sort first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Warning,
    Info,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ownership {
    Linked,
    Probable,
    MarkedOurs,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchBasis {
    /// The BSSID is linked to the AP.
    Linked,
    /// Same Wi-Fi 7 MLD address as one of the AP's linked BSSIDs.
    SameMld,
    /// Differs from one of the AP's BSSIDs only in the first/last octet.
    Heuristic,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApMatch {
    pub ap_id: i64,
    pub ap_name: String,
    pub floor_id: i64,
    pub basis: MatchBasis,
    /// The linked BSSID that matched (for `SameMld` / `Heuristic`).
    pub via_bssid: Option<String>,
}

/// Security as recorded for one reading. `recorded == false`: the point
/// predates security detail (schema v6), only `kind` is known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecurityProfile {
    pub kind: SecurityKind,
    pub recorded: bool,
    pub akms: Vec<Akm>,
    pub pairwise_ciphers: Vec<Cipher>,
    pub group_ciphers: Vec<Cipher>,
    pub group_mgmt_cipher: Option<Cipher>,
    pub pmf: Option<Pmf>,
    /// e.g. "WPA2/WPA3-Personal · PSK, SAE · CCMP · PMF capable".
    pub summary: String,
}

/// One reading of the BSSID a finding is about.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Heard {
    pub point_id: i64,
    pub floor_id: i64,
    pub floor_name: String,
    pub building_name: String,
    /// 1-based, in measurement order on its floor (as on the plan).
    pub point_number: usize,
    pub x: f64,
    pub y: f64,
    pub measured_at: DateTime<Utc>,
    pub bssid: String,
    pub ssid: Option<String>,
    pub frequency_mhz: u32,
    pub channel: Option<u16>,
    pub band: Band,
    pub signal: Signal,
    /// `None` for a reading that was set aside (not stored as a sample).
    pub security: Option<SecurityProfile>,
    pub note: Option<String>,
}

/// One of your BSSIDs a security comparison was made against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reference {
    pub bssid: String,
    pub ap_name: Option<String>,
    pub band: Band,
    pub security: SecurityProfile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    /// Stable within a project, e.g. `unknown_transmitter:AA:BB:...`.
    pub key: String,
    pub kind: FindingKind,
    pub severity: Severity,
    pub title: String,
    /// What the finding is based on, in plain words.
    pub explanation: String,
    pub bssid: String,
    /// The SSID(s) concerned, lossy UTF-8; `None` for a hidden SSID.
    pub ssid: Option<String>,
    pub band: Option<Band>,
    pub ownership: Ownership,
    pub ap: Option<ApMatch>,
    /// In scope, strongest first.
    pub heard: Vec<Heard>,
    pub channels: Vec<u16>,
    pub first_seen: Option<DateTime<Utc>>,
    pub last_seen: Option<DateTime<Utc>>,
    pub compared_with: Vec<Reference>,
    /// Compared by security type only: some readings predate the detail.
    pub coarse: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Findings {
    pub project_id: i64,
    pub scope: FindingScope,
    pub generated_at: DateTime<Utc>,
    /// Survey points in scope.
    pub points: usize,
    /// BSSIDs linked to placed APs, project-wide.
    pub linked_bssids: usize,
    pub project_ssids: Vec<String>,
    /// Warnings first.
    pub findings: Vec<Finding>,
    pub marks: Vec<BssidMark>,
    /// What the checks can't see, for this data.
    pub limits: Vec<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Same physical AP across bands/SSIDs: BSSIDs derived from one base MAC
/// usually differ only in the first and/or last octet. Heuristic; mirrors
/// `apKey` in `src/lib/heatmap.ts`.
pub fn ap_key(bssid: &str) -> String {
    bssid
        .to_ascii_lowercase()
        .split(':')
        .skip(1)
        .take(4)
        .collect::<Vec<_>>()
        .join(":")
}

fn ssid_label(raw: &[u8]) -> String {
    if raw.is_empty() {
        "(hidden)".into()
    } else {
        format!("\"{}\"", String::from_utf8_lossy(raw))
    }
}

fn ssid_text(raw: &[u8]) -> Option<String> {
    (!raw.is_empty()).then(|| String::from_utf8_lossy(raw).into_owned())
}

fn kind_label(k: SecurityKind) -> &'static str {
    match k {
        SecurityKind::Open => "Open",
        SecurityKind::Owe => "OWE",
        SecurityKind::Wep => "WEP",
        SecurityKind::WpaPersonal => "WPA-Personal",
        SecurityKind::Wpa2Personal => "WPA2-Personal",
        SecurityKind::Wpa3Personal => "WPA3-Personal",
        SecurityKind::Wpa2Wpa3Personal => "WPA2/WPA3-Personal",
        SecurityKind::WpaEnterprise => "WPA-Enterprise",
        SecurityKind::Wpa2Enterprise => "WPA2-Enterprise",
        SecurityKind::Wpa3Enterprise => "WPA3-Enterprise 192-bit",
        SecurityKind::Unknown => "unknown security",
    }
}

/// `ft_psk` → `FT-PSK`; unknown suites as hex.
fn suite_label<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s.to_ascii_uppercase().replace('_', "-"),
        Ok(serde_json::Value::Number(n)) => match n.as_u64() {
            Some(n) => format!("suite {n:#010X}"),
            None => format!("suite {n}"),
        },
        _ => "?".into(),
    }
}

fn join_labels<T: Serialize>(items: &[T]) -> String {
    items.iter().map(suite_label).collect::<Vec<_>>().join(", ")
}

impl SecurityProfile {
    pub fn of(sample: &Sample) -> Self {
        let mut p = Self {
            kind: sample.security,
            recorded: false,
            akms: vec![],
            pairwise_ciphers: vec![],
            group_ciphers: vec![],
            group_mgmt_cipher: None,
            pmf: None,
            summary: String::new(),
        };
        if let Some(d) = &sample.detail {
            p.recorded = true;
            p.akms = d.akms.clone();
            p.pairwise_ciphers = d.pairwise_ciphers.clone();
            p.group_ciphers = d.group_ciphers.clone();
            p.group_mgmt_cipher = d.group_mgmt_cipher;
            p.pmf = d.pmf;
        }
        p.summary = p.describe();
        p
    }

    fn describe(&self) -> String {
        let mut parts = vec![kind_label(self.kind).to_string()];
        if !self.recorded {
            parts.push("detail not recorded".into());
            return parts.join(" · ");
        }
        if self.is_owe_transition_open() {
            parts.push("open half of an OWE transition pair".into());
        }
        if !self.akms.is_empty() {
            parts.push(join_labels(&self.akms));
        }
        if !self.pairwise_ciphers.is_empty() {
            parts.push(join_labels(&self.pairwise_ciphers));
        }
        match self.pmf {
            Some(Pmf::Required) => parts.push("PMF required".into()),
            Some(Pmf::Capable) => parts.push("PMF capable".into()),
            Some(Pmf::Disabled) => parts.push("PMF off".into()),
            None => {}
        }
        parts.join(" · ")
    }

    /// The open BSS of an OWE transition pair: only the transition element.
    fn is_owe_transition_open(&self) -> bool {
        self.recorded && self.akms.contains(&Akm::OweTransition) && !self.akms.contains(&Akm::Owe)
    }

    fn legacy_ciphers(&self) -> BTreeSet<&'static str> {
        let mut out = BTreeSet::new();
        for c in self.pairwise_ciphers.iter().chain(&self.group_ciphers) {
            match c {
                Cipher::Tkip => out.insert("TKIP"),
                Cipher::Wep40 | Cipher::Wep104 => out.insert("WEP"),
                _ => false,
            };
        }
        out
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Open,
    Owe,
    Personal,
    Enterprise,
}

/// Comparable strength within and across families; `None` when it can't be
/// compared. A coarse (pre-v6) OWE reading can't be told apart from the
/// open half of a transition pair, so it isn't compared.
fn strength(p: &SecurityProfile) -> Option<(Family, u8)> {
    if p.is_owe_transition_open() {
        // As open as plain open, for clients without OWE.
        return Some((Family::Open, 0));
    }
    Some(match p.kind {
        SecurityKind::Open => (Family::Open, 0),
        SecurityKind::Wep => (Family::Open, 1),
        SecurityKind::Owe if p.recorded => (Family::Owe, 2),
        SecurityKind::Owe => return None,
        SecurityKind::WpaPersonal => (Family::Personal, 3),
        SecurityKind::Wpa2Personal => (Family::Personal, 4),
        // Downgradable to PSK, but offers SAE: weaker than SAE-only.
        SecurityKind::Wpa2Wpa3Personal => (Family::Personal, 5),
        SecurityKind::Wpa3Personal => (Family::Personal, 6),
        SecurityKind::WpaEnterprise => (Family::Enterprise, 3),
        SecurityKind::Wpa2Enterprise => (Family::Enterprise, 5),
        SecurityKind::Wpa3Enterprise => (Family::Enterprise, 7),
        SecurityKind::Unknown => return None,
    })
}

fn band_label(b: Band) -> String {
    b.to_string()
}

fn stronger_first(a: &Heard, b: &Heard) -> Ordering {
    b.signal
        .sort_key()
        .cmp(&a.signal.sort_key())
        .then(a.measured_at.cmp(&b.measured_at))
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

// ---------------------------------------------------------------------------
// Analysis
// ---------------------------------------------------------------------------

/// One stored sample with where it was taken.
struct Reading<'a> {
    point: &'a SurveyPoint,
    number: usize,
    sample: &'a Sample,
    in_scope: bool,
}

#[derive(Clone, Copy)]
enum Owner<'a> {
    Linked(&'a PlacedAp),
    Probable(&'a PlacedAp, MatchBasis, &'a str),
    MarkedOurs,
    Excluded,
    Unknown,
}

impl Owner<'_> {
    fn is_ours(&self) -> bool {
        matches!(
            self,
            Self::Linked(_) | Self::Probable(..) | Self::MarkedOurs
        )
    }

    fn ownership(&self) -> Ownership {
        match self {
            Self::Linked(_) => Ownership::Linked,
            Self::Probable(..) => Ownership::Probable,
            Self::MarkedOurs => Ownership::MarkedOurs,
            Self::Excluded | Self::Unknown => Ownership::Unknown,
        }
    }

    fn ap_match(&self) -> Option<ApMatch> {
        match *self {
            Self::Linked(ap) => Some(ApMatch {
                ap_id: ap.id,
                ap_name: ap.name.clone(),
                floor_id: ap.floor_id,
                basis: MatchBasis::Linked,
                via_bssid: None,
            }),
            Self::Probable(ap, basis, via) => Some(ApMatch {
                ap_id: ap.id,
                ap_name: ap.name.clone(),
                floor_id: ap.floor_id,
                basis,
                via_bssid: Some(via.to_string()),
            }),
            _ => None,
        }
    }

    fn ap_name(&self) -> Option<String> {
        self.ap_match().map(|m| m.ap_name)
    }

    /// "linked to AP 3", "probably a radio of AP 3 (heuristic)", ...
    fn describe(&self) -> String {
        match *self {
            Self::Linked(ap) => format!("linked to {}", ap.name),
            Self::Probable(ap, MatchBasis::SameMld, _) => {
                format!("probably a radio of {} (same MLD address)", ap.name)
            }
            Self::Probable(ap, _, _) => {
                format!("probably a radio of {} (heuristic: similar MAC)", ap.name)
            }
            Self::MarkedOurs => "marked as yours, not placed yet".into(),
            Self::Excluded | Self::Unknown => "not linked to a placed AP".into(),
        }
    }
}

struct Ctx<'a> {
    floors: HashMap<i64, &'a FloorRef>,
    readings: Vec<Reading<'a>>,
    /// BSSID → reading indices, in measurement order.
    by_bssid: BTreeMap<&'a str, Vec<usize>>,
    owners: HashMap<&'a str, Owner<'a>>,
}

impl<'a> Ctx<'a> {
    fn new(data: &'a ProjectSurvey, scope: &FindingScope) -> Self {
        let floors: HashMap<i64, &FloorRef> = data.floors.iter().map(|f| (f.id, f)).collect();
        let in_scope = |floor_id: i64| match *scope {
            FindingScope::Project { .. } => floors.contains_key(&floor_id),
            FindingScope::Building { id } => {
                floors.get(&floor_id).is_some_and(|f| f.building_id == id)
            }
            FindingScope::Floor { id } => floor_id == id,
        };
        let mut numbers: HashMap<i64, usize> = HashMap::new();
        let mut readings = Vec::new();
        for point in &data.points {
            let n = numbers.entry(point.floor_id).or_insert(0);
            *n += 1;
            for sample in &point.samples {
                readings.push(Reading {
                    point,
                    number: *n,
                    sample,
                    in_scope: in_scope(point.floor_id),
                });
            }
        }
        let mut by_bssid: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (i, r) in readings.iter().enumerate() {
            by_bssid.entry(r.sample.bssid.as_str()).or_default().push(i);
        }
        let mut ctx = Self {
            floors,
            readings,
            by_bssid,
            owners: HashMap::new(),
        };
        ctx.owners = ctx.classify(data);
        ctx
    }

    fn classify(&self, data: &'a ProjectSurvey) -> HashMap<&'a str, Owner<'a>> {
        // Oldest AP first, so a BSSID linked in two buildings names one AP
        // consistently.
        let mut aps: Vec<&PlacedAp> = data.aps.iter().collect();
        aps.sort_by_key(|a| a.id);
        let mut linked: HashMap<&str, &PlacedAp> = HashMap::new();
        let mut by_key: HashMap<String, (&PlacedAp, &str)> = HashMap::new();
        for ap in &aps {
            for b in &ap.bssids {
                linked.entry(b.as_str()).or_insert(ap);
                by_key.entry(ap_key(b)).or_insert((ap, b.as_str()));
            }
        }
        let mld_of = |bssid: &str| -> Option<&'a str> {
            self.by_bssid.get(bssid)?.iter().rev().find_map(|&i| {
                self.readings[i]
                    .sample
                    .detail
                    .as_ref()?
                    .mld_address
                    .as_deref()
            })
        };
        let mut by_mld: HashMap<&str, (&PlacedAp, &str)> = HashMap::new();
        for ap in &aps {
            for b in &ap.bssids {
                if let Some(mld) = mld_of(b) {
                    by_mld.entry(mld).or_insert((ap, b.as_str()));
                }
            }
        }
        let marks: HashMap<&str, MarkStatus> = data
            .marks
            .iter()
            .map(|m| (m.bssid.as_str(), m.status))
            .collect();

        let mut owners = HashMap::new();
        for &bssid in self.by_bssid.keys() {
            let owner = if let Some(ap) = linked.get(bssid) {
                Owner::Linked(ap)
            } else if matches!(
                marks.get(bssid),
                Some(MarkStatus::Neighbour | MarkStatus::Ignored)
            ) {
                Owner::Excluded
            } else if let Some((ap, via)) = mld_of(bssid).and_then(|m| by_mld.get(m)) {
                Owner::Probable(ap, MatchBasis::SameMld, via)
            } else if let Some((ap, via)) = by_key.get(&ap_key(bssid)) {
                Owner::Probable(ap, MatchBasis::Heuristic, via)
            } else if marks.get(bssid) == Some(&MarkStatus::OursUnplaced) {
                Owner::MarkedOurs
            } else {
                Owner::Unknown
            };
            owners.insert(bssid, owner);
        }
        owners
    }

    fn owner(&self, bssid: &str) -> Owner<'a> {
        self.owners.get(bssid).copied().unwrap_or(Owner::Unknown)
    }

    fn heard(&self, i: usize, note: Option<String>) -> Heard {
        let r = &self.readings[i];
        let floor = self.floors.get(&r.point.floor_id);
        Heard {
            point_id: r.point.id,
            floor_id: r.point.floor_id,
            floor_name: floor.map(|f| f.name.clone()).unwrap_or_default(),
            building_name: floor.map(|f| f.building_name.clone()).unwrap_or_default(),
            point_number: r.number,
            x: r.point.x,
            y: r.point.y,
            measured_at: r.point.measured_at,
            bssid: r.sample.bssid.clone(),
            ssid: r.sample.ssid.clone(),
            frequency_mhz: r.sample.frequency_mhz,
            channel: r.sample.channel,
            band: r.sample.band,
            signal: r.sample.signal,
            security: Some(SecurityProfile::of(r.sample)),
            note,
        }
    }

    /// Finding skeleton from its evidence; `None` if nothing is in scope.
    #[allow(clippy::too_many_arguments)]
    fn finding(
        &self,
        kind: FindingKind,
        severity: Severity,
        bssid: &str,
        ssid: Option<String>,
        band: Option<Band>,
        title: String,
        explanation: String,
        mut heard: Vec<Heard>,
    ) -> Option<Finding> {
        if heard.is_empty() {
            return None;
        }
        heard.sort_by(stronger_first);
        let channels: BTreeSet<u16> = heard.iter().filter_map(|h| h.channel).collect();
        let owner = self.owner(bssid);
        Some(Finding {
            key: format!(
                "{}:{bssid}:{}:{}",
                serde_json::to_value(kind)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default(),
                ssid.as_deref().unwrap_or(""),
                band.map(band_label).unwrap_or_default()
            ),
            kind,
            severity,
            title,
            explanation,
            bssid: bssid.to_string(),
            ssid,
            band,
            ownership: owner.ownership(),
            ap: owner.ap_match(),
            first_seen: heard.iter().map(|h| h.measured_at).min(),
            last_seen: heard.iter().map(|h| h.measured_at).max(),
            heard,
            channels: channels.into_iter().collect(),
            compared_with: vec![],
            coarse: false,
        })
    }

    fn in_scope(&self, idx: &[usize]) -> Vec<usize> {
        idx.iter()
            .copied()
            .filter(|&i| self.readings[i].in_scope)
            .collect()
    }
}

/// Run every rule over `data` and report what's in `scope`.
pub fn analyse(data: &ProjectSurvey, scope: FindingScope) -> Findings {
    let ctx = Ctx::new(data, &scope);
    let linked_bssids: HashSet<&str> = data
        .aps
        .iter()
        .flat_map(|a| a.bssids.iter().map(String::as_str))
        .collect();
    let project_ssids: BTreeSet<&[u8]> = ctx
        .readings
        .iter()
        .filter(|r| matches!(ctx.owner(&r.sample.bssid), Owner::Linked(_)))
        .map(|r| r.sample.ssid_raw.as_slice())
        .filter(|s| !s.is_empty())
        .collect();

    let mut findings = Vec::new();
    findings.extend(unknown_transmitters(&ctx, &project_ssids));
    findings.extend(unlinked_radios(&ctx));
    findings.extend(security_mismatches(&ctx));
    findings.extend(multi_frequency(&ctx, data));
    findings.extend(inconsistent_bssids(&ctx));
    findings.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then(a.kind.cmp(&b.kind))
            .then(a.bssid.cmp(&b.bssid))
            .then(a.key.cmp(&b.key))
    });

    let scoped: Vec<&Reading> = ctx.readings.iter().filter(|r| r.in_scope).collect();
    let points = {
        let ids: HashSet<i64> = data
            .points
            .iter()
            .filter(|p| match scope {
                FindingScope::Project { .. } => ctx.floors.contains_key(&p.floor_id),
                FindingScope::Building { id } => ctx
                    .floors
                    .get(&p.floor_id)
                    .is_some_and(|f| f.building_id == id),
                FindingScope::Floor { id } => p.floor_id == id,
            })
            .map(|p| p.id)
            .collect();
        ids.len()
    };

    let mut limits = Vec::new();
    if linked_bssids.is_empty() {
        limits.push(
            "No placed access point has linked BSSIDs, so nothing counts as yours yet: the \
             unknown-transmitter and security checks need at least one linked BSSID."
                .to_string(),
        );
    }
    let coarse = scoped.iter().filter(|r| r.sample.detail.is_none()).count();
    if coarse > 0 {
        limits.push(format!(
            "{} of {} readings were measured before security detail was recorded: for \
             those, security is compared by type only (coarse), and two channels in one \
             scan weren't kept.",
            coarse,
            scoped.len()
        ));
    }
    limits.push(
        "Hidden networks are only checked where a probe response revealed their SSID.".into(),
    );
    limits.push(
        "A clone that copies a BSSID together with its SSID and security looks identical in \
         scan data; it only shows up when heard on two channels in one scan, or with a \
         different SSID or security than usual."
            .into(),
    );
    if !data.marks.is_empty() {
        limits.push("BSSIDs marked as neighbour or ignored are left out of every check.".into());
    }

    Findings {
        project_id: data.project_id,
        scope,
        generated_at: Utc::now(),
        points,
        linked_bssids: linked_bssids.len(),
        project_ssids: project_ssids
            .iter()
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect(),
        findings,
        marks: data.marks.clone(),
        limits,
    }
}

/// A BSSID that isn't yours broadcasting one of the project's SSIDs.
fn unknown_transmitters(ctx: &Ctx, project_ssids: &BTreeSet<&[u8]>) -> Vec<Finding> {
    let mut out = Vec::new();
    for (&bssid, idx) in &ctx.by_bssid {
        if !matches!(ctx.owner(bssid), Owner::Unknown) {
            continue;
        }
        let ssids: BTreeSet<&[u8]> = idx
            .iter()
            .map(|&i| ctx.readings[i].sample.ssid_raw.as_slice())
            .filter(|s| project_ssids.contains(s))
            .collect();
        if ssids.is_empty() {
            continue;
        }
        let scoped = ctx.in_scope(idx);
        let names = ssids
            .iter()
            .map(|s| ssid_label(s))
            .collect::<Vec<_>>()
            .join(", ");
        let points: HashSet<i64> = scoped.iter().map(|&i| ctx.readings[i].point.id).collect();
        let explanation = format!(
            "Unknown transmitter using {names}: not linked to a placed AP (could be one of \
             yours not yet placed, a neighbour, or a rogue). {names} is broadcast by BSSIDs \
             linked to your placed APs; {bssid} is not linked, doesn't share an MLD address \
             with them and isn't marked. Heard in {} at {}.",
            plural(scoped.len(), "reading", "readings"),
            plural(points.len(), "point", "points"),
        );
        let heard = scoped.iter().map(|&i| ctx.heard(i, None)).collect();
        out.extend(
            ctx.finding(
                FindingKind::UnknownTransmitter,
                Severity::Warning,
                bssid,
                Some(
                    ssids
                        .iter()
                        .filter_map(|s| ssid_text(s))
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
                None,
                format!("Unknown transmitter using {names}"),
                explanation,
                heard,
            ),
        );
    }
    out
}

/// Probably a radio of a placed AP, but not linked to it.
fn unlinked_radios(ctx: &Ctx) -> Vec<Finding> {
    let mut out = Vec::new();
    for (&bssid, idx) in &ctx.by_bssid {
        let Owner::Probable(ap, basis, via) = ctx.owner(bssid) else {
            continue;
        };
        let why = match basis {
            MatchBasis::SameMld => format!(
                "It shares its Wi-Fi 7 MLD address with {via}, which is linked to {}.",
                ap.name
            ),
            _ => format!(
                "Its MAC differs from {via} (linked to {}) only in the first and/or last \
                 octet, which is how most vendors number one AP's radios. This is a \
                 heuristic, not proof.",
                ap.name
            ),
        };
        let ssids: BTreeSet<String> = idx
            .iter()
            .filter_map(|&i| ssid_text(&ctx.readings[i].sample.ssid_raw))
            .collect();
        let heard = ctx
            .in_scope(idx)
            .into_iter()
            .map(|i| ctx.heard(i, None))
            .collect();
        out.extend(ctx.finding(
            FindingKind::UnlinkedRadio,
            Severity::Info,
            bssid,
            (!ssids.is_empty()).then(|| ssids.into_iter().collect::<Vec<_>>().join(", ")),
            None,
            format!("Probably a radio of {}, not linked", ap.name),
            format!("{why} Link it to the AP if it is one of its radios."),
            heard,
        ));
    }
    out
}

struct Reason {
    severity: Severity,
    title: &'static str,
    text: String,
}

/// Compare one BSSID's security with your BSSIDs on the same SSID.
fn compare(c: &SecurityProfile, refs: &[&SecurityProfile]) -> Vec<Reason> {
    let mut out = Vec::new();
    let rs: Vec<(Family, u8)> = refs.iter().filter_map(|r| strength(r)).collect();
    if let (Some(cs), Some(weakest)) = (strength(c), rs.iter().map(|r| r.1).min()) {
        let shares_family = rs.iter().any(|r| r.0 == cs.0);
        let protected = |f: Family| matches!(f, Family::Personal | Family::Enterprise);
        if cs.1 < weakest {
            let weakest_label = refs
                .iter()
                .filter(|r| strength(r).map(|s| s.1) == Some(weakest))
                .map(|r| kind_label(r.kind))
                .next()
                .unwrap_or("?");
            match cs.0 {
                Family::Open if cs.1 == 1 => out.push(Reason {
                    severity: Severity::Warning,
                    title: "WEP copy",
                    text: format!("It uses WEP where yours use at least {weakest_label}."),
                }),
                Family::Open => out.push(Reason {
                    severity: Severity::Warning,
                    title: "Open copy",
                    text: format!(
                        "It is open{} where yours use at least {weakest_label}.",
                        if c.is_owe_transition_open() {
                            " (the open half of an OWE transition pair)"
                        } else {
                            ""
                        }
                    ),
                }),
                Family::Owe => out.push(Reason {
                    severity: Severity::Warning,
                    title: "Unauthenticated (OWE) copy",
                    text: format!(
                        "It uses OWE (encrypted, but anyone can join) where yours use at least \
                         {weakest_label}."
                    ),
                }),
                _ if shares_family => out.push(Reason {
                    severity: Severity::Warning,
                    title: "Weaker security",
                    text: format!(
                        "It offers {} where yours use at least {weakest_label}{}.",
                        kind_label(c.kind),
                        if c.recorded {
                            format!(" (AKMs: {})", join_labels(&c.akms))
                        } else {
                            String::new()
                        }
                    ),
                }),
                _ => {}
            }
        }
        if protected(cs.0) && !shares_family && rs.iter().any(|r| protected(r.0)) {
            out.push(Reason {
                severity: Severity::Info,
                title: "Different security type",
                text: format!(
                    "It uses {} while yours use {} (personal vs enterprise): not weaker as \
                     such, but not the same network.",
                    kind_label(c.kind),
                    kind_label(refs[0].kind)
                ),
            });
        }
    }

    // Detail checks, against your BSSIDs whose detail was recorded.
    let detailed: Vec<&&SecurityProfile> = refs.iter().filter(|r| r.recorded).collect();
    if !c.recorded || detailed.is_empty() {
        return out;
    }
    let legacy = c.legacy_ciphers();
    if !legacy.is_empty() && detailed.iter().all(|r| r.legacy_ciphers().is_empty()) {
        out.push(Reason {
            severity: Severity::Warning,
            title: "Legacy cipher",
            text: format!(
                "It allows {} where yours don't.",
                legacy.into_iter().collect::<Vec<_>>().join(" and ")
            ),
        });
    }
    let pmf: Vec<Pmf> = detailed.iter().filter_map(|r| r.pmf).collect();
    if !pmf.is_empty() {
        if c.pmf == Some(Pmf::Disabled) && pmf.iter().all(|p| *p != Pmf::Disabled) {
            out.push(Reason {
                severity: Severity::Warning,
                title: "PMF disabled",
                text: "Protected management frames (802.11w) are off where yours offer or \
                       require them."
                    .into(),
            });
        } else if c.pmf == Some(Pmf::Capable) && pmf.iter().all(|p| *p == Pmf::Required) {
            out.push(Reason {
                severity: Severity::Info,
                title: "PMF optional",
                text: "Protected management frames are optional where yours require them.".into(),
            });
        }
    }
    out
}

/// Per (SSID, band): BSSIDs whose security is weaker than (or differs from)
/// your BSSIDs on that SSID.
fn security_mismatches(ctx: &Ctx) -> Vec<Finding> {
    // (ssid, band) → bssid → reading indices.
    type Group<'a> = BTreeMap<&'a str, Vec<usize>>;
    let mut groups: BTreeMap<(&[u8], Band), Group> = BTreeMap::new();
    for (i, r) in ctx.readings.iter().enumerate() {
        let s = r.sample;
        if s.ssid_raw.is_empty() || matches!(ctx.owner(&s.bssid), Owner::Excluded) {
            continue;
        }
        groups
            .entry((s.ssid_raw.as_slice(), s.band))
            .or_default()
            .entry(s.bssid.as_str())
            .or_default()
            .push(i);
    }
    // The profile a BSSID shows in a group: its latest reading with detail,
    // else its latest reading.
    let profile = |idx: &[usize]| -> SecurityProfile {
        let pick = idx
            .iter()
            .rev()
            .find(|&&i| ctx.readings[i].sample.detail.is_some())
            .or(idx.last())
            .copied()
            .unwrap_or_default();
        SecurityProfile::of(ctx.readings[pick].sample)
    };

    let mut out = Vec::new();
    for (&(ssid, band), members) in &groups {
        for (&bssid, idx) in members {
            let scoped = ctx.in_scope(idx);
            if scoped.is_empty() {
                continue;
            }
            let ours_in = |g: &'_ Group<'_>, b: Band| -> Vec<Reference> {
                g.iter()
                    .filter(|(&other, _)| other != bssid && ctx.owner(other).is_ours())
                    .map(|(&other, idx)| Reference {
                        bssid: other.to_string(),
                        ap_name: ctx.owner(other).ap_name(),
                        band: b,
                        security: profile(idx),
                    })
                    .collect()
            };
            let mut refs = ours_in(members, band);
            let mut other_bands = false;
            if refs.is_empty() {
                // None of yours on this band: compare with other bands, but
                // never hold a 2.4/5 GHz BSSID to 6 GHz rules (6 GHz only
                // allows SAE/OWE, so a transition network's 6 GHz BSSIDs
                // are SAE-only by design).
                other_bands = true;
                for (&(s2, b2), g2) in &groups {
                    if s2 == ssid && b2 != band && (band == Band::Band6GHz || b2 != Band::Band6GHz)
                    {
                        refs.extend(ours_in(g2, b2));
                    }
                }
            }
            if refs.is_empty() {
                continue;
            }
            let mine = profile(idx);
            let ref_profiles: Vec<&SecurityProfile> = refs.iter().map(|r| &r.security).collect();
            let mut reasons = compare(&mine, &ref_profiles);
            if reasons.is_empty() {
                continue;
            }
            reasons.sort_by_key(|r| r.severity);
            let coarse = !mine.recorded || refs.iter().any(|r| !r.security.recorded);
            let owner = ctx.owner(bssid);
            let label = ssid_label(ssid);
            let mut explanation = format!(
                "{bssid} ({}) broadcasts {label} on {band} with {}. ",
                owner.describe(),
                mine.summary
            );
            for r in &reasons {
                explanation.push_str(&r.text);
                explanation.push(' ');
            }
            explanation.push_str(&if other_bands {
                format!(
                    "Compared with {} of yours on other bands (none of yours broadcasts \
                     {label} on {band}).",
                    plural(refs.len(), "BSSID", "BSSIDs")
                )
            } else {
                format!(
                    "Compared with {} of yours broadcasting {label} on {band}.",
                    plural(refs.len(), "BSSID", "BSSIDs")
                )
            });
            if coarse {
                explanation.push_str(
                    " Some of these readings predate security detail, so the comparison is \
                     coarse (security type only).",
                );
            }
            let heard = scoped.iter().map(|&i| ctx.heard(i, None)).collect();
            let severity = reasons[0].severity;
            if let Some(mut f) = ctx.finding(
                FindingKind::SecurityMismatch,
                severity,
                bssid,
                ssid_text(ssid),
                Some(band),
                format!("{} of {label} on {band}", reasons[0].title),
                explanation,
                heard,
            ) {
                f.compared_with = refs;
                f.coarse = coarse;
                out.push(f);
            }
        }
    }
    out
}

/// One BSSID listed on two frequencies in one scan (stored anomalies).
fn multi_frequency(ctx: &Ctx, data: &ProjectSurvey) -> Vec<Finding> {
    let mut by_bssid: BTreeMap<&str, Vec<(i64, &PointAnomaly)>> = BTreeMap::new();
    for (point_id, a) in &data.anomalies {
        if a.kind == AnomalyKind::MultiFrequency {
            by_bssid
                .entry(a.bssid.as_str())
                .or_default()
                .push((*point_id, a));
        }
    }
    let mut out = Vec::new();
    for (bssid, anomalies) in by_bssid {
        let owner = ctx.owner(bssid);
        if matches!(owner, Owner::Excluded) {
            continue;
        }
        let kept = ctx.by_bssid.get(bssid).cloned().unwrap_or_default();
        let mut heard = Vec::new();
        for (point_id, a) in &anomalies {
            let Some(&i) = kept
                .iter()
                .find(|&&i| ctx.readings[i].point.id == *point_id && ctx.readings[i].in_scope)
            else {
                continue;
            };
            let mut other = ctx.heard(
                i,
                Some(format!(
                    "set aside: the same scan also listed it on {} MHz",
                    ctx.readings[i].sample.frequency_mhz
                )),
            );
            other.frequency_mhz = a.frequency_mhz;
            other.channel = a.channel;
            other.band = a.band;
            other.signal = a.signal;
            other.ssid = ssid_text(&a.ssid_raw);
            other.security = None;
            heard.push(ctx.heard(
                i,
                Some(format!(
                    "kept: the same scan also listed it on {} MHz",
                    a.frequency_mhz
                )),
            ));
            heard.push(other);
        }
        let points: HashSet<i64> = heard.iter().map(|h| h.point_id).collect();
        let severity = if owner.is_ours() {
            Severity::Warning
        } else {
            Severity::Info
        };
        let explanation = format!(
            "{bssid} ({}) was listed on two frequencies within one scan at {}. A BSSID \
             belongs to one radio on one channel, so two transmitters may be using it \
             (a clone{}), or the AP changed channel during the scan (DFS or a channel \
             switch). The point keeps one reading as its sample; the other is kept here.",
            owner.describe(),
            plural(points.len(), "point", "points"),
            owner
                .ap_name()
                .map(|n| format!(" of {n}"))
                .unwrap_or_default(),
        );
        out.extend(ctx.finding(
            FindingKind::MultiFrequency,
            severity,
            bssid,
            None,
            None,
            format!("{bssid} heard on two channels in one scan"),
            explanation,
            heard,
        ));
    }
    out
}

/// The most frequent value (ties: the one seen first).
fn usual<T: PartialEq + Clone>(values: impl Iterator<Item = T>) -> Option<T> {
    let mut counts: Vec<(T, usize)> = Vec::new();
    for v in values {
        match counts.iter_mut().find(|(c, _)| *c == v) {
            Some((_, n)) => *n += 1,
            None => counts.push((v, 1)),
        }
    }
    let max = counts.iter().map(|c| c.1).max()?;
    counts.into_iter().find(|c| c.1 == max).map(|c| c.0)
}

/// One of your BSSIDs heard with a different SSID or security than usual.
///
/// Security is compared by the summary type (which every provider derives
/// the same way) and by PMF where both readings recorded it, not by the
/// AKM list: its exact suites depend on how the provider read it.
fn inconsistent_bssids(ctx: &Ctx) -> Vec<Finding> {
    let mut out = Vec::new();
    for (&bssid, idx) in &ctx.by_bssid {
        let owner = ctx.owner(bssid);
        if !matches!(owner, Owner::Linked(_) | Owner::MarkedOurs) || idx.len() < 2 {
            continue;
        }
        let sample = |i: usize| ctx.readings[i].sample;
        let named: Vec<usize> = idx
            .iter()
            .copied()
            .filter(|&i| !sample(i).ssid_raw.is_empty())
            .collect();
        let usual_ssid = usual(named.iter().map(|&i| sample(i).ssid_raw.as_slice()));
        let kinds: Vec<usize> = idx
            .iter()
            .copied()
            .filter(|&i| sample(i).security != SecurityKind::Unknown)
            .collect();
        let usual_kind = usual(kinds.iter().map(|&i| sample(i).security));
        let pmf_of = |i: usize| sample(i).detail.as_ref().and_then(|d| d.pmf);
        let usual_pmf = usual(idx.iter().filter_map(|&i| pmf_of(i)));

        let mut heard = Vec::new();
        for i in ctx.in_scope(idx) {
            let s = sample(i);
            let mut diffs = Vec::new();
            if let Some(u) = usual_ssid {
                if !s.ssid_raw.is_empty() && s.ssid_raw != u {
                    diffs.push(format!(
                        "SSID {} instead of {}",
                        ssid_label(&s.ssid_raw),
                        ssid_label(u)
                    ));
                }
            }
            if let Some(u) = usual_kind {
                if s.security != SecurityKind::Unknown && s.security != u {
                    diffs.push(format!(
                        "{} instead of {}",
                        kind_label(s.security),
                        kind_label(u)
                    ));
                }
            }
            if let (Some(u), Some(p)) = (usual_pmf, pmf_of(i)) {
                if p != u {
                    diffs.push(format!(
                        "PMF {} instead of {}",
                        suite_label(&p).to_lowercase(),
                        suite_label(&u).to_lowercase()
                    ));
                }
            }
            if !diffs.is_empty() {
                heard.push(ctx.heard(i, Some(diffs.join("; "))));
            }
        }
        if heard.is_empty() {
            continue;
        }
        let usual_desc = format!(
            "{}{}",
            usual_ssid
                .map(ssid_label)
                .unwrap_or_else(|| "(hidden)".into()),
            usual_kind
                .map(|k| format!(", {}", kind_label(k)))
                .unwrap_or_default()
        );
        let explanation = format!(
            "{bssid} ({}) is usually heard as {usual_desc} ({} in total), but {} differ. \
             One BSSID belongs to one radio, so this can be a clone copying its MAC (an evil \
             twin), or the AP was reconfigured between measurements: compare the times.",
            owner.describe(),
            plural(idx.len(), "reading", "readings"),
            plural(heard.len(), "reading", "readings"),
        );
        out.extend(ctx.finding(
            FindingKind::BssidInconsistent,
            Severity::Warning,
            bssid,
            usual_ssid.and_then(ssid_text),
            None,
            format!("{bssid} heard with a different SSID or security than usual"),
            explanation,
            heard,
        ));
    }
    out
}

/// What the UI needs to turn a BSSID into one of yours: the SSIDs it
/// broadcast, related BSSIDs to link with it, the AP it probably belongs
/// to, and where it was heard best per floor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkOptions {
    pub bssid: String,
    pub ssids: Vec<String>,
    /// Unlinked BSSIDs that look like other radios of the same AP (same MLD
    /// address, else the heuristic), including `bssid` itself, sorted.
    pub related_bssids: Vec<String>,
    pub probable_ap: Option<ApMatch>,
    /// Strongest reading per floor, strongest floor first: a starting
    /// position for placing it.
    pub strongest_per_floor: Vec<Heard>,
    /// Every placed AP in the project, oldest first.
    pub aps: Vec<PlacedAp>,
    pub floors: Vec<FloorRef>,
}

pub fn link_options(data: &ProjectSurvey, bssid: &str) -> LinkOptions {
    let ctx = Ctx::new(
        data,
        &FindingScope::Project {
            id: data.project_id,
        },
    );
    let owner = ctx.owner(bssid);
    let idx = ctx.by_bssid.get(bssid).cloned().unwrap_or_default();
    let ssids: BTreeSet<String> = idx
        .iter()
        .filter_map(|&i| ssid_text(&ctx.readings[i].sample.ssid_raw))
        .collect();
    let mld = idx
        .iter()
        .rev()
        .find_map(|&i| ctx.readings[i].sample.detail.as_ref()?.mld_address.clone());
    let key = ap_key(bssid);
    let related: BTreeSet<String> = ctx
        .by_bssid
        .iter()
        .filter(|(&b, _)| {
            matches!(
                ctx.owner(b),
                Owner::Unknown | Owner::MarkedOurs | Owner::Probable(..)
            )
        })
        .filter(|(&b, other)| {
            b == bssid
                || match &mld {
                    Some(m) => other.iter().any(|&i| {
                        ctx.readings[i]
                            .sample
                            .detail
                            .as_ref()
                            .and_then(|d| d.mld_address.as_ref())
                            == Some(m)
                    }),
                    None => ap_key(b) == key,
                }
        })
        .map(|(&b, _)| b.to_string())
        .collect();
    let mut best: BTreeMap<i64, Heard> = BTreeMap::new();
    for &i in &idx {
        let h = ctx.heard(i, None);
        match best.get(&h.floor_id) {
            Some(b) if stronger_first(b, &h) != Ordering::Greater => {}
            _ => {
                best.insert(h.floor_id, h);
            }
        }
    }
    let mut strongest: Vec<Heard> = best.into_values().collect();
    strongest.sort_by(stronger_first);
    let mut aps = data.aps.clone();
    aps.sort_by_key(|a| a.id);
    LinkOptions {
        bssid: bssid.to_string(),
        ssids: ssids.into_iter().collect(),
        related_bssids: related.into_iter().collect(),
        probable_ap: match owner {
            Owner::Probable(..) => owner.ap_match(),
            _ => None,
        },
        strongest_per_floor: strongest,
        aps,
        floors: data.floors.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::survey::models::{MeasuringAdapter, SampleDetail};
    use crate::wifi::models::AdapterId;
    use chrono::TimeZone;

    const AP1_5: &str = "02:11:22:33:44:51";
    const AP1_24: &str = "00:11:22:33:44:50";
    const ROGUE: &str = "DE:AD:BE:EF:00:01";

    fn t(min: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 1, 9, min, 0).unwrap()
    }

    fn detail(akms: &[Akm], pmf: Option<Pmf>) -> SampleDetail {
        SampleDetail {
            akms: akms.to_vec(),
            pairwise_ciphers: vec![Cipher::Ccmp],
            group_ciphers: vec![Cipher::Ccmp],
            group_mgmt_cipher: None,
            pmf,
            hidden: false,
            mld_address: None,
        }
    }

    /// A WPA2-Personal "Corp" sample on 5 GHz, PMF capable.
    fn s(bssid: &str, ssid: &str, dbm: f32) -> Sample {
        Sample {
            bssid: bssid.into(),
            ssid: (!ssid.is_empty()).then(|| ssid.into()),
            ssid_raw: ssid.as_bytes().to_vec(),
            frequency_mhz: 5180,
            channel: Some(36),
            band: Band::Band5GHz,
            channel_width_mhz: Some(80),
            channel_center_mhz: Some(5210),
            signal: Signal::from_dbm(dbm),
            security: SecurityKind::Wpa2Personal,
            phy_type: None,
            wifi_generation: None,
            noise_dbm: None,
            snr_db: None,
            channel_utilization_pct: None,
            station_count: None,
            last_seen_age_ms: Some(100),
            is_connected: false,
            detail: Some(detail(&[Akm::Psk], Some(Pmf::Capable))),
        }
    }

    fn with_sec(mut s: Sample, kind: SecurityKind, akms: &[Akm], pmf: Option<Pmf>) -> Sample {
        s.security = kind;
        s.detail = Some(detail(akms, pmf));
        s
    }

    fn on(mut s: Sample, band: Band, mhz: u32, ch: u16) -> Sample {
        s.band = band;
        s.frequency_mhz = mhz;
        s.channel = Some(ch);
        s
    }

    fn point(id: i64, floor_id: i64, min: u32, samples: Vec<Sample>) -> SurveyPoint {
        SurveyPoint {
            adapter_bands: None,
            id,
            floor_id,
            x: id as f64 * 10.0,
            y: 5.0,
            measured_at: t(min),
            scan_duration_ms: 3000,
            adapter: MeasuringAdapter {
                id: AdapterId::linux("wlan0"),
                provider: "test".into(),
                model: None,
                driver: None,
                hw_id: None,
            },
            samples,
        }
    }

    fn ap(id: i64, name: &str, bssids: &[&str]) -> PlacedAp {
        PlacedAp {
            id,
            floor_id: 1,
            name: name.into(),
            x: 0.0,
            y: 0.0,
            model: None,
            notes: None,
            bssids: bssids.iter().map(|b| b.to_string()).collect(),
            created_at: t(0),
            updated_at: t(0),
        }
    }

    fn survey(points: Vec<SurveyPoint>, aps: Vec<PlacedAp>) -> ProjectSurvey {
        ProjectSurvey {
            project_id: 1,
            floors: vec![
                FloorRef {
                    id: 1,
                    name: "Ground".into(),
                    building_id: 1,
                    building_name: "Main".into(),
                },
                FloorRef {
                    id: 2,
                    name: "First".into(),
                    building_id: 1,
                    building_name: "Main".into(),
                },
            ],
            points,
            aps,
            marks: vec![],
            anomalies: vec![],
        }
    }

    fn project(data: &ProjectSurvey) -> Findings {
        analyse(data, FindingScope::Project { id: 1 })
    }

    fn of_kind(f: &Findings, kind: FindingKind) -> Vec<&Finding> {
        f.findings.iter().filter(|x| x.kind == kind).collect()
    }

    #[test]
    fn ap_key_matches_the_frontend_heuristic() {
        assert_eq!(ap_key("02:11:22:33:44:51"), "11:22:33:44");
        assert_eq!(ap_key(AP1_5), ap_key(AP1_24));
        assert_ne!(ap_key(ROGUE), ap_key(AP1_5));
    }

    #[test]
    fn unknown_transmitter_on_a_project_ssid() {
        let data = survey(
            vec![
                point(
                    1,
                    1,
                    1,
                    vec![s(AP1_5, "Corp", -50.0), s(ROGUE, "Corp", -70.0)],
                ),
                point(
                    2,
                    2,
                    5,
                    vec![
                        s(ROGUE, "Corp", -60.0),
                        s("AA:00:00:00:00:09", "Cafe", -60.0),
                    ],
                ),
            ],
            vec![ap(1, "AP 1", &[AP1_5])],
        );
        let f = project(&data);
        assert_eq!(f.project_ssids, ["Corp"]);
        let unknown = of_kind(&f, FindingKind::UnknownTransmitter);
        // The café's BSSID uses no project SSID: not a finding.
        assert_eq!(unknown.len(), 1);
        let u = unknown[0];
        assert_eq!(u.bssid, ROGUE);
        assert_eq!(u.severity, Severity::Warning);
        assert_eq!(u.ownership, Ownership::Unknown);
        assert!(u.explanation.starts_with(
            "Unknown transmitter using \"Corp\": not linked to a placed AP (could be one of \
             yours not yet placed, a neighbour, or a rogue)."
        ));
        // Strongest first, with floor and point number.
        assert_eq!(u.heard.len(), 2);
        assert_eq!(
            (u.heard[0].floor_name.as_str(), u.heard[0].point_number),
            ("First", 1)
        );
        assert_eq!(u.heard[0].signal.dbm, Some(-60.0));
        assert_eq!(u.channels, [36]);
        assert_eq!((u.first_seen, u.last_seen), (Some(t(1)), Some(t(5))));

        // Floor scope: only evidence from that floor.
        let floor = analyse(&data, FindingScope::Floor { id: 1 });
        let u = of_kind(&floor, FindingKind::UnknownTransmitter)[0];
        assert_eq!(u.heard.len(), 1);
        assert_eq!(u.heard[0].point_id, 1);
        assert_eq!(floor.points, 1);
    }

    #[test]
    fn marks_and_probable_radios_are_not_unknown() {
        let sibling = "06:11:22:33:44:52";
        let mut mld_radio = s("7A:00:00:00:00:01", "Corp", -55.0);
        mld_radio.detail.as_mut().unwrap().mld_address = Some("AA:BB:CC:00:00:01".into());
        let mut linked = s(AP1_5, "Corp", -50.0);
        linked.detail.as_mut().unwrap().mld_address = Some("AA:BB:CC:00:00:01".into());
        let mut data = survey(
            vec![point(
                1,
                1,
                1,
                vec![
                    linked,
                    s(sibling, "Corp", -52.0),
                    mld_radio,
                    s(ROGUE, "Corp", -70.0),
                    s("DE:AD:BE:EF:00:02", "Corp", -75.0),
                ],
            )],
            vec![ap(1, "AP 1", &[AP1_5])],
        );
        data.marks = vec![
            BssidMark {
                bssid: ROGUE.into(),
                status: MarkStatus::Neighbour,
                note: Some("café next door".into()),
                updated_at: t(0),
            },
            BssidMark {
                bssid: "DE:AD:BE:EF:00:02".into(),
                status: MarkStatus::OursUnplaced,
                note: None,
                updated_at: t(0),
            },
        ];
        let f = project(&data);
        assert!(of_kind(&f, FindingKind::UnknownTransmitter).is_empty());
        let radios = of_kind(&f, FindingKind::UnlinkedRadio);
        let summary: Vec<_> = radios
            .iter()
            .map(|r| (r.bssid.as_str(), r.ap.as_ref().unwrap().basis, r.severity))
            .collect();
        assert_eq!(
            summary,
            [
                (sibling, MatchBasis::Heuristic, Severity::Info),
                ("7A:00:00:00:00:01", MatchBasis::SameMld, Severity::Info),
            ]
        );
        assert!(radios[0].explanation.contains("heuristic"));
        assert!(f.findings.iter().all(|x| x.bssid != ROGUE));
    }

    #[test]
    fn open_and_weaker_copies_are_security_mismatches() {
        let open = with_sec(s(ROGUE, "Corp", -60.0), SecurityKind::Open, &[], None);
        let mut tkip = with_sec(
            s("DE:AD:BE:EF:00:02", "Corp", -65.0),
            SecurityKind::Wpa2Personal,
            &[Akm::Psk],
            Some(Pmf::Disabled),
        );
        tkip.detail.as_mut().unwrap().pairwise_ciphers = vec![Cipher::Tkip, Cipher::Ccmp];
        let data = survey(
            vec![point(
                1,
                1,
                1,
                vec![
                    with_sec(
                        s(AP1_5, "Corp", -50.0),
                        SecurityKind::Wpa2Wpa3Personal,
                        &[Akm::Psk, Akm::Sae],
                        Some(Pmf::Capable),
                    ),
                    open,
                    tkip,
                ],
            )],
            vec![ap(1, "AP 1", &[AP1_5])],
        );
        let f = project(&data);
        let m = of_kind(&f, FindingKind::SecurityMismatch);
        assert_eq!(m.len(), 2);
        let open = m.iter().find(|x| x.bssid == ROGUE).unwrap();
        assert_eq!(open.title, "Open copy of \"Corp\" on 5 GHz");
        assert_eq!(open.severity, Severity::Warning);
        assert_eq!(open.compared_with[0].bssid, AP1_5);
        assert!(!open.coarse);
        let weak = m.iter().find(|x| x.bssid == "DE:AD:BE:EF:00:02").unwrap();
        assert!(weak
            .explanation
            .contains("WPA2-Personal where yours use at least WPA2/WPA3-Personal"));
        assert!(weak.explanation.contains("allows TKIP"));
        assert!(weak
            .explanation
            .contains("Protected management frames (802.11w) are off"));
    }

    #[test]
    fn six_ghz_sae_only_of_a_transition_network_is_expected() {
        let transition = |b: &str, band, mhz, ch| {
            on(
                with_sec(
                    s(b, "Corp", -50.0),
                    SecurityKind::Wpa2Wpa3Personal,
                    &[Akm::Psk, Akm::Sae],
                    Some(Pmf::Capable),
                ),
                band,
                mhz,
                ch,
            )
        };
        let six = on(
            with_sec(
                s("02:11:22:33:44:53", "Corp", -55.0),
                SecurityKind::Wpa3Personal,
                &[Akm::Sae],
                Some(Pmf::Required),
            ),
            Band::Band6GHz,
            5955,
            1,
        );
        let data = survey(
            vec![point(
                1,
                1,
                1,
                vec![
                    transition(AP1_5, Band::Band5GHz, 5180, 36),
                    transition(AP1_24, Band::Band2_4GHz, 2412, 1),
                    six,
                ],
            )],
            vec![ap(1, "AP 1", &[AP1_5, AP1_24, "02:11:22:33:44:53"])],
        );
        let f = project(&data);
        assert!(
            of_kind(&f, FindingKind::SecurityMismatch).is_empty(),
            "{:#?}",
            f.findings
        );
        // And the 2.4 GHz transition BSSID isn't held to the 6 GHz rules
        // when it is the only one on its band.
        assert!(f.findings.is_empty(), "{:#?}", f.findings);
    }

    #[test]
    fn owe_transition_pair_is_expected() {
        let open_half = with_sec(
            s(AP1_5, "Guest", -50.0),
            SecurityKind::Owe,
            &[Akm::OweTransition],
            None,
        );
        let mut hidden_owe = with_sec(
            s("02:11:22:33:44:52", "", -50.0),
            SecurityKind::Owe,
            &[Akm::Owe],
            Some(Pmf::Required),
        );
        hidden_owe.detail.as_mut().unwrap().hidden = true;
        // A second AP's open half, and a plain open BSS: as open as the
        // transition network's open half for clients without OWE.
        let other_half = with_sec(
            s("00:AA:BB:CC:DD:01", "Guest", -60.0),
            SecurityKind::Owe,
            &[Akm::OweTransition],
            None,
        );
        let data = survey(
            vec![point(1, 1, 1, vec![open_half, hidden_owe, other_half])],
            vec![
                ap(1, "AP 1", &[AP1_5, "02:11:22:33:44:52"]),
                ap(2, "AP 2", &["00:AA:BB:CC:DD:01"]),
            ],
        );
        let f = project(&data);
        assert!(
            of_kind(&f, FindingKind::SecurityMismatch).is_empty(),
            "{:#?}",
            f.findings
        );

        // An open copy of a pure OWE network is a mismatch.
        let owe = with_sec(
            s(AP1_5, "Guest", -50.0),
            SecurityKind::Owe,
            &[Akm::Owe],
            Some(Pmf::Required),
        );
        let open = with_sec(s(ROGUE, "Guest", -60.0), SecurityKind::Open, &[], None);
        let data = survey(
            vec![point(1, 1, 1, vec![owe, open])],
            vec![ap(1, "AP 1", &[AP1_5])],
        );
        let m = project(&data);
        let m = of_kind(&m, FindingKind::SecurityMismatch);
        assert_eq!(m.len(), 1);
        assert!(m[0].title.starts_with("Open copy"));
    }

    #[test]
    fn old_rows_compare_coarsely() {
        let mut ours = s(AP1_5, "Corp", -50.0);
        ours.detail = None;
        let mut open = with_sec(s(ROGUE, "Corp", -60.0), SecurityKind::Open, &[], None);
        open.detail = None;
        let data = survey(
            vec![point(1, 1, 1, vec![ours, open])],
            vec![ap(1, "AP 1", &[AP1_5])],
        );
        let f = project(&data);
        let m = of_kind(&f, FindingKind::SecurityMismatch);
        assert_eq!(m.len(), 1);
        assert!(m[0].coarse);
        assert!(m[0].explanation.contains("coarse"));
        assert!(f.limits.iter().any(|l| l.contains("2 of 2 readings")));
    }

    #[test]
    fn other_bands_are_the_fallback_reference() {
        let ours = on(s(AP1_24, "Corp", -50.0), Band::Band2_4GHz, 2412, 1);
        let open = with_sec(s(ROGUE, "Corp", -60.0), SecurityKind::Open, &[], None);
        let data = survey(
            vec![point(1, 1, 1, vec![ours, open])],
            vec![ap(1, "AP 1", &[AP1_24])],
        );
        let f = project(&data);
        let m = of_kind(&f, FindingKind::SecurityMismatch);
        assert_eq!(m.len(), 1);
        assert!(m[0].explanation.contains("on other bands"));
    }

    #[test]
    fn personal_copy_of_an_enterprise_network_is_info() {
        let ent = with_sec(
            s(AP1_5, "Corp", -50.0),
            SecurityKind::Wpa2Enterprise,
            &[Akm::Ieee8021x],
            Some(Pmf::Capable),
        );
        let psk = s(ROGUE, "Corp", -60.0);
        let data = survey(
            vec![point(1, 1, 1, vec![ent, psk])],
            vec![ap(1, "AP 1", &[AP1_5])],
        );
        let f = project(&data);
        let m = of_kind(&f, FindingKind::SecurityMismatch);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].severity, Severity::Info);
        assert!(m[0].title.starts_with("Different security type"));
    }

    #[test]
    fn two_frequencies_in_one_scan() {
        let mut data = survey(
            vec![point(1, 1, 1, vec![s(AP1_5, "Corp", -50.0)])],
            vec![ap(1, "AP 1", &[AP1_5])],
        );
        data.anomalies = vec![(
            1,
            PointAnomaly {
                bssid: AP1_5.into(),
                kind: AnomalyKind::MultiFrequency,
                frequency_mhz: 5500,
                channel: Some(100),
                band: Band::Band5GHz,
                ssid_raw: b"Corp".to_vec(),
                signal: Signal::from_dbm(-80.0),
            },
        )];
        let f = project(&data);
        let m = of_kind(&f, FindingKind::MultiFrequency);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].severity, Severity::Warning);
        assert_eq!(m[0].channels, [36, 100]);
        assert_eq!(m[0].heard.len(), 2);
        assert!(m[0].explanation.contains("clone of AP 1"));
        // Out of scope: no finding.
        let floor2 = analyse(&data, FindingScope::Floor { id: 2 });
        assert!(floor2.findings.is_empty());
    }

    #[test]
    fn linked_bssid_with_another_ssid_or_security_than_usual() {
        let data = survey(
            vec![
                point(1, 1, 1, vec![s(AP1_5, "Corp", -50.0)]),
                point(2, 1, 2, vec![s(AP1_5, "Corp", -52.0)]),
                point(
                    3,
                    1,
                    3,
                    vec![with_sec(
                        s(AP1_5, "Corp-Free", -60.0),
                        SecurityKind::Open,
                        &[],
                        None,
                    )],
                ),
                // A hidden beacon is no different SSID.
                point(4, 1, 4, vec![s(AP1_5, "", -55.0)]),
            ],
            vec![ap(1, "AP 1", &[AP1_5])],
        );
        let f = project(&data);
        let m = of_kind(&f, FindingKind::BssidInconsistent);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].heard.len(), 1);
        assert_eq!(m[0].heard[0].point_id, 3);
        let note = m[0].heard[0].note.as_deref().unwrap();
        assert!(
            note.contains("SSID \"Corp-Free\" instead of \"Corp\""),
            "{note}"
        );
        assert!(note.contains("Open instead of WPA2-Personal"), "{note}");
    }

    #[test]
    fn link_options_suggest_related_radios_and_a_position() {
        let data = survey(
            vec![
                point(
                    1,
                    1,
                    1,
                    vec![
                        s(ROGUE, "Corp", -70.0),
                        s("DA:AD:BE:EF:00:02", "Corp-IoT", -72.0),
                    ],
                ),
                point(2, 1, 2, vec![s(ROGUE, "Corp", -55.0)]),
                point(3, 2, 3, vec![s(ROGUE, "Corp", -80.0)]),
            ],
            vec![ap(1, "AP 1", &[AP1_5])],
        );
        let o = link_options(&data, ROGUE);
        assert_eq!(o.ssids, ["Corp"]);
        assert_eq!(o.related_bssids, ["DA:AD:BE:EF:00:02", ROGUE]);
        assert!(o.probable_ap.is_none());
        let best: Vec<_> = o
            .strongest_per_floor
            .iter()
            .map(|h| (h.floor_id, h.point_id))
            .collect();
        assert_eq!(best, [(1, 2), (2, 3)]);
        assert_eq!(o.aps.len(), 1);
    }

    #[test]
    fn nothing_linked_says_so() {
        let data = survey(vec![point(1, 1, 1, vec![s(ROGUE, "Corp", -60.0)])], vec![]);
        let f = project(&data);
        assert!(f.findings.is_empty());
        assert!(f.limits[0].contains("No placed access point has linked BSSIDs"));
        // Serialises for the UI and the report.
        let json = serde_json::to_value(&f).unwrap();
        assert_eq!(json["scope"]["kind"], "project");
    }
}
