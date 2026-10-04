//! Requirements profiles: what "good" means for a survey, and pass/fail per
//! survey point, defined against the stored data only.
//!
//! A result is three-state: pass, fail (which rules), or not evaluated (why).
//! Nothing is guessed: a point with only % readings is never failed, a band
//! the card can't receive is never reported missing, and SNR or utilisation
//! are judged only where the data exists. Area (as opposed to point) results
//! are computed in the UI with the same IDW grid as the coverage heatmap;
//! [`PointEvaluation`] carries the per-point inputs it needs.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::models::{
    MeasuringAdapter, PlacedAp, PointTest, PointTestResults, PointTestRole, PointTestStatus,
    Sample, SurveyPoint,
};
use crate::error::{Result, WifiError};
use crate::wifi::models::{Band, Capability};

// ---------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Preset {
    OfficeData,
    VoiceVideo,
    WarehouseBasic,
    Custom,
}

impl Preset {
    pub const ALL: [Preset; 4] = [
        Preset::OfficeData,
        Preset::VoiceVideo,
        Preset::WarehouseBasic,
        Preset::Custom,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Preset::OfficeData => "office_data",
            Preset::VoiceVideo => "voice_video",
            Preset::WarehouseBasic => "warehouse_basic",
            Preset::Custom => "custom",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Preset::OfficeData => "Office data",
            Preset::VoiceVideo => "Voice / video",
            Preset::WarehouseBasic => "Warehouse / basic coverage",
            Preset::Custom => "Custom",
        }
    }

    /// The preset's values; `None` for Custom.
    ///
    /// These follow commonly published vendor design guidance (the WLAN
    /// design guides of Cisco, HPE Aruba and Ekahau cite −67 dBm for voice
    /// and roaming-sensitive clients, a second AP a few dB weaker for
    /// roaming, and few co-channel APs above about −80/−85 dBm). They are
    /// starting points for the job, not a standard. No preset sets SNR:
    /// many drivers (iwlwifi) and Windows report no noise floor. No preset
    /// sets speed targets either: they depend on the site's internet and
    /// LAN, not on the Wi-Fi design.
    pub fn values(self) -> Option<RequirementValues> {
        let v = |primary, secondary, cochannel: (u32, i32), bands: &[Band]| RequirementValues {
            primary_min_dbm: primary,
            secondary_min_dbm: secondary,
            cochannel_max: Some(cochannel.0),
            cochannel_level_dbm: Some(cochannel.1),
            required_bands: bands.to_vec(),
            min_snr_db: None,
            max_util_pct: None,
            ..RequirementValues::default()
        };
        match self {
            Preset::OfficeData => Some(v(-67, Some(-75), (2, -80), &[Band::Band5GHz])),
            Preset::VoiceVideo => Some(v(-65, Some(-70), (1, -80), &[Band::Band5GHz])),
            Preset::WarehouseBasic => Some(v(-72, None, (3, -85), &[])),
            Preset::Custom => None,
        }
    }
}

/// The thresholds of a profile. `None` switches a rule off.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequirementValues {
    /// Strongest target BSSID must reach this.
    pub primary_min_dbm: i32,
    /// A different physical AP (same SSID and band) must reach this.
    pub secondary_min_dbm: Option<i32>,
    /// At most this many other radios overlapping the primary's channel ...
    pub cochannel_max: Option<u32>,
    /// ... heard at or above this level. Set together with `cochannel_max`.
    pub cochannel_level_dbm: Option<i32>,
    /// A target network must reach `primary_min_dbm` on each of these.
    pub required_bands: Vec<Band>,
    pub min_snr_db: Option<i32>,
    /// Maximum channel utilisation the serving AP advertises (BSS Load).
    pub max_util_pct: Option<i32>,
    // Speed targets, from the point's active tests (the latest of each):
    // iperf3 receiver averages and the gateway ping. A point without that
    // test isn't evaluated on it.
    /// iperf3 download (server → this computer), Mbit/s.
    #[serde(default)]
    pub min_download_mbps: Option<u32>,
    /// iperf3 upload, Mbit/s.
    #[serde(default)]
    pub min_upload_mbps: Option<u32>,
    /// Average round trip to the gateway, ms.
    #[serde(default)]
    pub max_latency_ms: Option<u32>,
    /// Ping loss to the gateway, %.
    #[serde(default)]
    pub max_loss_pct: Option<u32>,
}

pub const REQUIRABLE_BANDS: [Band; 3] = [Band::Band2_4GHz, Band::Band5GHz, Band::Band6GHz];
const NAME_MAX_CHARS: usize = 100;

impl RequirementValues {
    /// Checks ranges; returns the values with bands sorted and de-duplicated.
    pub fn validated(&self) -> Result<Self> {
        let dbm = |v: i32, what: &str| {
            if (-100..=-20).contains(&v) {
                Ok(())
            } else {
                Err(WifiError::InvalidInput(format!(
                    "{what} must be between -100 and -20 dBm"
                )))
            }
        };
        dbm(self.primary_min_dbm, "the primary signal level")?;
        if let Some(v) = self.secondary_min_dbm {
            dbm(v, "the secondary signal level")?;
        }
        match (self.cochannel_max, self.cochannel_level_dbm) {
            (Some(n), Some(level)) => {
                if n > 50 {
                    return Err(WifiError::InvalidInput(
                        "the co-channel limit must be between 0 and 50".into(),
                    ));
                }
                dbm(level, "the co-channel level")?;
            }
            (None, None) => {}
            _ => {
                return Err(WifiError::InvalidInput(
                    "the co-channel rule needs both a count and a level".into(),
                ))
            }
        }
        if self.min_snr_db.is_some_and(|v| !(0..=80).contains(&v)) {
            return Err(WifiError::InvalidInput(
                "the minimum SNR must be between 0 and 80 dB".into(),
            ));
        }
        if self.max_util_pct.is_some_and(|v| !(0..=100).contains(&v)) {
            return Err(WifiError::InvalidInput(
                "the maximum utilisation must be between 0 and 100 %".into(),
            ));
        }
        for (v, what) in [
            (self.min_download_mbps, "the download target"),
            (self.min_upload_mbps, "the upload target"),
        ] {
            if v.is_some_and(|v| !(1..=100_000).contains(&v)) {
                return Err(WifiError::InvalidInput(format!(
                    "{what} must be between 1 and 100000 Mbit/s"
                )));
            }
        }
        if self
            .max_latency_ms
            .is_some_and(|v| !(1..=10_000).contains(&v))
        {
            return Err(WifiError::InvalidInput(
                "the latency target must be between 1 and 10000 ms".into(),
            ));
        }
        if self.max_loss_pct.is_some_and(|v| v > 100) {
            return Err(WifiError::InvalidInput(
                "the loss target must be between 0 and 100 %".into(),
            ));
        }
        let mut bands = self.required_bands.clone();
        if let Some(b) = bands.iter().find(|b| !REQUIRABLE_BANDS.contains(b)) {
            return Err(WifiError::InvalidInput(format!(
                "{b} can't be a required band"
            )));
        }
        bands.sort();
        bands.dedup();
        Ok(Self {
            required_bands: bands,
            ..self.clone()
        })
    }
}

/// A network a profile applies to.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum RequirementTarget {
    /// Every BSSID broadcasting these SSID bytes.
    Ssid { ssid_raw: Vec<u8> },
    /// Every BSSID linked to this placed AP (works for hidden SSIDs).
    Ap { ap_id: i64 },
}

/// A target as stored, with what the UI shows for it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetInfo {
    #[serde(flatten)]
    pub target: RequirementTarget,
    /// The SSID (lossy UTF-8) or the AP's name.
    pub label: String,
    /// AP targets: the AP's BSSIDs now. Empty for SSID targets.
    pub bssids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequirementProfile {
    pub id: i64,
    pub project_id: i64,
    pub name: String,
    /// The preset the values are from; `Custom` once they differ from it.
    pub preset: Preset,
    #[serde(flatten)]
    pub values: RequirementValues,
    /// Applies to every floor of the project without an override.
    pub is_default: bool,
    pub targets: Vec<TargetInfo>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Fields set when creating or editing a profile (targets are set separately).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequirementProfileInput {
    pub name: String,
    pub preset: Preset,
    #[serde(flatten)]
    pub values: RequirementValues,
    #[serde(default)]
    pub is_default: bool,
}

impl RequirementProfileInput {
    /// Validated (name, preset, values). A preset whose values were changed
    /// is stored as Custom, so a report never shows a preset's name next to
    /// other numbers.
    pub fn validated(&self) -> Result<(String, Preset, RequirementValues)> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err(WifiError::InvalidInput("the profile needs a name".into()));
        }
        if name.chars().count() > NAME_MAX_CHARS {
            return Err(WifiError::InvalidInput(format!(
                "the profile name is longer than {NAME_MAX_CHARS} characters"
            )));
        }
        let values = self.values.validated()?;
        let preset = match self.preset.values() {
            Some(p) if p == values => self.preset,
            _ => Preset::Custom,
        };
        Ok((name.to_string(), preset, values))
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetInfo {
    pub preset: Preset,
    pub label: &'static str,
    pub values: Option<RequirementValues>,
}

pub fn presets() -> Vec<PresetInfo> {
    Preset::ALL
        .into_iter()
        .map(|preset| PresetInfo {
            preset,
            label: preset.label(),
            values: preset.values(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Pass,
    Fail,
    NotEvaluated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    PrimarySignal,
    SecondarySignal,
    CoChannel,
    RequiredBand,
    Snr,
    Utilisation,
    Download,
    Upload,
    Latency,
    Loss,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleResult {
    pub rule: Rule,
    /// The band, for `RequiredBand` (one result per required band).
    pub band: Option<Band>,
    pub outcome: Outcome,
    /// dBm, a count, dB or %, depending on the rule.
    pub measured: Option<f32>,
    /// What it was held against (same unit).
    pub limit: Option<f32>,
    pub detail: String,
    /// Relied on the BSSID heuristic to tell physical APs apart (some
    /// BSSIDs involved aren't linked to a placed AP).
    pub heuristic: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PointEvaluation {
    pub point_id: i64,
    pub outcome: Outcome,
    /// Why the point wasn't evaluated.
    pub reason: Option<String>,
    pub rules: Vec<RuleResult>,
    /// The strongest target BSSID.
    pub primary_bssid: Option<String>,
    // Inputs for the area estimate (interpolated in the UI); `None` when
    // nothing qualifying was heard or the rule wasn't evaluated.
    pub primary_dbm: Option<f32>,
    pub secondary_dbm: Option<f32>,
    pub cochannel_count: Option<u32>,
    /// The primary's channel width was unknown, so co-channel was judged
    /// on its 20 MHz primary channel only.
    pub width_unknown: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleCount {
    pub rule: Rule,
    pub band: Option<Band>,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasonCount {
    pub reason: String,
    pub count: usize,
}

/// Per-floor summary over survey POINTS (not area: the UI estimates the
/// share of mapped area separately and labels it as such).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FloorSummary {
    pub points: usize,
    pub passed: usize,
    pub failed: usize,
    pub not_evaluated: usize,
    /// `passed / (passed + failed)`: share of the evaluated points.
    pub pass_fraction_of_points: Option<f64>,
    /// Failing points per rule (a point can fail several).
    pub failures_by_rule: Vec<RuleCount>,
    /// Points where a rule couldn't be judged (e.g. the card has no 6 GHz).
    pub rules_not_evaluated: Vec<RuleCount>,
    pub not_evaluated_reasons: Vec<ReasonCount>,
    /// Distinct adapters that measured the points; more than one means the
    /// readings aren't directly comparable.
    pub adapters: Vec<MeasuringAdapter>,
    /// Some result depended on the BSSID heuristic, not placed APs.
    pub uses_heuristic: bool,
    /// Points whose primary had an unknown channel width.
    pub width_unknown_points: usize,
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

/// Same physical AP across bands/SSIDs: BSSIDs derived from one base MAC
/// usually differ only in the first and/or last octet. A heuristic; the
/// same one as `apKey` in the UI (`src/lib/heatmap.ts`).
pub fn ap_key(bssid: &str) -> String {
    bssid
        .to_ascii_lowercase()
        .split(':')
        .skip(1)
        .take(4)
        .collect::<Vec<_>>()
        .join(":")
}

/// What a floor's points are judged with.
pub struct Evaluator<'a> {
    values: &'a RequirementValues,
    target_ssids: HashSet<&'a [u8]>,
    target_bssids: HashSet<&'a str>,
    /// BSSID → placed AP id (the building's APs).
    placed: HashMap<&'a str, i64>,
}

impl<'a> Evaluator<'a> {
    pub fn new(profile: &'a RequirementProfile, building_aps: &'a [PlacedAp]) -> Self {
        let mut target_ssids = HashSet::new();
        let mut target_bssids = HashSet::new();
        for t in &profile.targets {
            match &t.target {
                RequirementTarget::Ssid { ssid_raw } => {
                    target_ssids.insert(ssid_raw.as_slice());
                }
                RequirementTarget::Ap { .. } => {
                    target_bssids.extend(t.bssids.iter().map(String::as_str));
                }
            }
        }
        let placed = building_aps
            .iter()
            .flat_map(|ap| ap.bssids.iter().map(move |b| (b.as_str(), ap.id)))
            .collect();
        Self {
            values: &profile.values,
            target_ssids,
            target_bssids,
            placed,
        }
    }

    fn has_targets(&self) -> bool {
        !self.target_ssids.is_empty() || !self.target_bssids.is_empty()
    }

    fn is_target(&self, s: &Sample) -> bool {
        self.target_bssids.contains(s.bssid.as_str())
            || (!s.ssid_raw.is_empty() && self.target_ssids.contains(s.ssid_raw.as_slice()))
    }

    /// Whether two BSSIDs are one physical AP, and whether that answer came
    /// from the heuristic: placed APs decide when both are linked, else the
    /// shared base MAC.
    fn same_ap(&self, a: &str, b: &str) -> (bool, bool) {
        match (self.placed.get(a), self.placed.get(b)) {
            (Some(x), Some(y)) => (x == y, false),
            _ => (ap_key(a) == ap_key(b), true),
        }
    }

    /// One radio = one physical AP on one frequency (its SSIDs share it).
    fn radio_key(&self, s: &Sample) -> String {
        match self.placed.get(s.bssid.as_str()) {
            Some(id) => format!("ap:{id}@{}", s.frequency_mhz),
            None => format!("mac:{}@{}", ap_key(&s.bssid), s.frequency_mhz),
        }
    }

    pub fn evaluate(&self, point: &SurveyPoint) -> PointEvaluation {
        let mut eval = PointEvaluation {
            point_id: point.id,
            outcome: Outcome::NotEvaluated,
            reason: None,
            rules: Vec::new(),
            primary_bssid: None,
            primary_dbm: None,
            secondary_dbm: None,
            cochannel_count: None,
            width_unknown: false,
        };
        if !self.has_targets() {
            eval.reason = Some("the profile has no target networks".into());
            return eval;
        }
        let samples = &point.samples;
        if !samples.is_empty() && samples.iter().all(|s| s.signal.dbm.is_none()) {
            eval.reason =
                Some("the adapter reported signal as % only; requirements need dBm".into());
            return eval;
        }
        let dbm = |s: &Sample| s.signal.dbm.unwrap_or(f32::NEG_INFINITY);
        let targets: Vec<&Sample> = samples.iter().filter(|s| self.is_target(s)).collect();
        let primary = targets
            .iter()
            .copied()
            .filter(|s| s.signal.dbm.is_some())
            .max_by(|a, b| dbm(a).total_cmp(&dbm(b)));
        if primary.is_none() && !targets.is_empty() {
            eval.reason = Some("the target network was heard here only as %, not dBm".into());
            return eval;
        }
        let v = self.values;

        // Primary signal
        let primary_limit = v.primary_min_dbm as f32;
        eval.rules.push(match primary {
            Some(p) => {
                let d = dbm(p);
                eval.primary_bssid = Some(p.bssid.clone());
                eval.primary_dbm = Some(d);
                rule(
                    Rule::PrimarySignal,
                    d >= primary_limit,
                    Some(d),
                    Some(primary_limit),
                    format!("strongest target {} at {d:.0} dBm", describe(p)),
                )
            }
            None => rule(
                Rule::PrimarySignal,
                false,
                None,
                Some(primary_limit),
                "no target network heard".into(),
            ),
        });

        // Secondary signal: a different physical AP, same SSID, same band.
        if let Some(limit) = v.secondary_min_dbm {
            let limit = limit as f32;
            eval.rules.push(match primary {
                None => rule(
                    Rule::SecondarySignal,
                    false,
                    None,
                    Some(limit),
                    "no target network heard".into(),
                ),
                Some(p) => {
                    let mut heuristic = false;
                    let best = targets
                        .iter()
                        .copied()
                        .filter(|s| {
                            s.signal.dbm.is_some()
                                && s.ssid_raw == p.ssid_raw
                                && s.band == p.band
                                && s.bssid != p.bssid
                        })
                        .filter(|s| {
                            let (same, guessed) = self.same_ap(&p.bssid, &s.bssid);
                            heuristic |= guessed;
                            !same
                        })
                        .max_by(|a, b| dbm(a).total_cmp(&dbm(b)));
                    let mut r = match best {
                        Some(s) => {
                            let d = dbm(s);
                            eval.secondary_dbm = Some(d);
                            rule(
                                Rule::SecondarySignal,
                                d >= limit,
                                Some(d),
                                Some(limit),
                                format!("second AP {} at {d:.0} dBm", describe(s)),
                            )
                        }
                        None => rule(
                            Rule::SecondarySignal,
                            false,
                            None,
                            Some(limit),
                            format!("no other AP heard on this SSID in {}", p.band),
                        ),
                    };
                    r.heuristic = heuristic;
                    r
                }
            });
        }

        // Co-channel: distinct other radios overlapping the primary's
        // occupied span, at or above the level.
        if let (Some(max), Some(level)) = (v.cochannel_max, v.cochannel_level_dbm) {
            eval.rules.push(match primary {
                None => skipped(Rule::CoChannel, None, "no target network heard"),
                Some(p) => {
                    let (lo, hi, known) = span(p);
                    eval.width_unknown = !known;
                    let mut heuristic = false;
                    let mut radios = HashSet::new();
                    for s in samples {
                        if s.signal.dbm.is_none_or(|d| d < level as f32) {
                            continue;
                        }
                        let (slo, shi, _) = span(s);
                        if !(slo < hi && lo < shi) {
                            continue;
                        }
                        let (same, guessed) = self.same_ap(&p.bssid, &s.bssid);
                        heuristic |= guessed;
                        if same && s.frequency_mhz == p.frequency_mhz {
                            continue; // the primary's own radio
                        }
                        radios.insert(self.radio_key(s));
                    }
                    let n = radios.len() as u32;
                    eval.cochannel_count = Some(n);
                    let mut detail = format!(
                        "{n} other radio{} at ≥ {level} dBm overlapping {}",
                        if n == 1 { "" } else { "s" },
                        channel_label(p)
                    );
                    if !known {
                        detail.push_str(" (channel width unknown: primary 20 MHz channel only)");
                    }
                    let mut r = rule(
                        Rule::CoChannel,
                        n <= max,
                        Some(n as f32),
                        Some(max as f32),
                        detail,
                    );
                    r.heuristic = heuristic;
                    r
                }
            });
        }

        // Required bands: a target network at the primary level on each.
        for &band in &v.required_bands {
            let best = targets
                .iter()
                .filter(|s| s.band == band)
                .filter_map(|s| s.signal.dbm)
                .max_by(f32::total_cmp);
            let detail = match best {
                Some(d) => format!("best target on {band}: {d:.0} dBm"),
                None => format!("no target network heard on {band}"),
            };
            let mut r = if best.is_some_and(|d| d >= primary_limit) {
                rule(Rule::RequiredBand, true, best, Some(primary_limit), detail)
            } else {
                // Missing only counts if the card could have heard it: it
                // says so, or it heard something on that band here.
                let heard_band = samples.iter().any(|s| s.band == band);
                let capability = point.adapter_bands.map(|b| b.get(band));
                match (heard_band, capability) {
                    (true, _) | (_, Some(Capability::Supported)) => {
                        rule(Rule::RequiredBand, false, best, Some(primary_limit), detail)
                    }
                    (false, None) => skipped(
                        Rule::RequiredBand,
                        best,
                        "which bands the adapter supports wasn't recorded for this point",
                    ),
                    (false, Some(Capability::Unsupported)) => skipped(
                        Rule::RequiredBand,
                        best,
                        &format!("the adapter can't receive {band}"),
                    ),
                    (false, Some(Capability::Unknown)) => skipped(
                        Rule::RequiredBand,
                        best,
                        &format!("unknown whether the adapter can receive {band}"),
                    ),
                }
            };
            r.band = Some(band);
            eval.rules.push(r);
        }

        // SNR, only where the driver reports noise.
        if let Some(min) = v.min_snr_db {
            let min = min as f32;
            eval.rules.push(match primary {
                None => skipped(Rule::Snr, None, "no target network heard"),
                Some(p) => match p.snr_db.or_else(|| Some(p.signal.dbm? - p.noise_dbm?)) {
                    Some(snr) => rule(
                        Rule::Snr,
                        snr >= min,
                        Some(snr),
                        Some(min),
                        format!("SNR {snr:.0} dB"),
                    ),
                    None => skipped(
                        Rule::Snr,
                        None,
                        "no noise floor reported (the driver or OS doesn't provide it)",
                    ),
                },
            });
        }

        // Utilisation, as the primary AP advertises it (BSS Load element).
        if let Some(max) = v.max_util_pct {
            let max = max as f32;
            eval.rules.push(match primary {
                None => skipped(Rule::Utilisation, None, "no target network heard"),
                Some(p) => match p.channel_utilization_pct {
                    Some(u) => rule(
                        Rule::Utilisation,
                        u <= max,
                        Some(u),
                        Some(max),
                        format!("AP-advertised channel load (BSS Load) {u:.0} %"),
                    ),
                    None => skipped(
                        Rule::Utilisation,
                        None,
                        "the AP doesn't advertise its channel load (BSS Load)",
                    ),
                },
            });
        }

        eval.outcome = if eval.rules.iter().any(|r| r.outcome == Outcome::Fail) {
            Outcome::Fail
        } else {
            Outcome::Pass
        };
        eval
    }
}

fn rule(
    rule: Rule,
    pass: bool,
    measured: Option<f32>,
    limit: Option<f32>,
    detail: String,
) -> RuleResult {
    RuleResult {
        rule,
        band: None,
        outcome: if pass { Outcome::Pass } else { Outcome::Fail },
        measured,
        limit,
        detail,
        heuristic: false,
    }
}

fn skipped(rule: Rule, measured: Option<f32>, why: &str) -> RuleResult {
    RuleResult {
        rule,
        band: None,
        outcome: Outcome::NotEvaluated,
        measured,
        limit: None,
        detail: why.to_string(),
        heuristic: false,
    }
}

fn describe(s: &Sample) -> String {
    format!("{} ({})", s.bssid, channel_label(s))
}

fn channel_label(s: &Sample) -> String {
    let ch = s
        .channel
        .map_or_else(|| format!("{} MHz", s.frequency_mhz), |c| format!("ch {c}"));
    match s.channel_width_mhz {
        Some(w) => format!("{ch}/{w}"),
        None => ch,
    }
}

/// Occupied span in MHz from centre and width; with either unknown, the
/// 20 MHz primary channel (third value `false`).
fn span(s: &Sample) -> (f64, f64, bool) {
    match (s.channel_center_mhz, s.channel_width_mhz) {
        (Some(c), Some(w)) if w > 0 => {
            let half = f64::from(w) / 2.0;
            (f64::from(c) - half, f64::from(c) + half, true)
        }
        _ => {
            let f = f64::from(s.frequency_mhz);
            (f - 10.0, f + 10.0, false)
        }
    }
}

/// Evaluate every point and summarise. `tests`: the floor's point tests
/// (any order), for the speed targets.
pub fn evaluate_floor(
    profile: &RequirementProfile,
    building_aps: &[PlacedAp],
    points: &[SurveyPoint],
    tests: &[PointTest],
) -> (Vec<PointEvaluation>, FloorSummary) {
    let evaluator = Evaluator::new(profile, building_aps);
    let evals: Vec<PointEvaluation> = points
        .iter()
        .map(|p| {
            let mut eval = evaluator.evaluate(p);
            let mine: Vec<&PointTest> = tests.iter().filter(|t| t.point_id == p.id).collect();
            let speed = evaluate_speed(&profile.values, &mine);
            if speed.iter().any(|r| r.outcome == Outcome::Fail) {
                eval.outcome = Outcome::Fail;
            }
            eval.rules.extend(speed);
            eval
        })
        .collect();
    let summary = summarise(&evals, points);
    (evals, summary)
}

/// The speed rules the profile sets, judged on the latest finished test of
/// each kind at the point (cancelled ones are ignored). No test, or one that
/// failed to run (server unreachable), is "not evaluated", never a fail: it
/// says nothing about the spot.
pub fn evaluate_speed(values: &RequirementValues, tests: &[&PointTest]) -> Vec<RuleResult> {
    let latest = |role: PointTestRole| {
        tests
            .iter()
            .filter(|t| t.role == role && t.status != PointTestStatus::Cancelled)
            .max_by_key(|t| (t.started_at, t.id))
            .copied()
    };
    let mut out = Vec::new();
    for (limit, rule_kind, role, what) in [
        (
            values.min_download_mbps,
            Rule::Download,
            PointTestRole::Iperf3Download,
            "download",
        ),
        (
            values.min_upload_mbps,
            Rule::Upload,
            PointTestRole::Iperf3Upload,
            "upload",
        ),
    ] {
        let Some(limit) = limit else { continue };
        out.push(match latest(role) {
            None => skipped(rule_kind, None, &format!("no {what} test at this point")),
            Some(t) => match (&t.results, t.status) {
                (Some(PointTestResults::Iperf3(r)), PointTestStatus::Ok) => {
                    let mbps = (r.bits_per_second / 1e6) as f32;
                    rule(
                        rule_kind,
                        mbps >= limit as f32,
                        Some(mbps),
                        Some(limit as f32),
                        format!("{what} {mbps:.0} Mbit/s, target ≥ {limit} Mbit/s"),
                    )
                }
                _ => skipped(
                    rule_kind,
                    None,
                    &format!(
                        "the {what} test didn't run: {}",
                        t.error.as_deref().unwrap_or("no result")
                    ),
                ),
            },
        });
    }
    if values.max_latency_ms.is_some() || values.max_loss_pct.is_some() {
        let ping = latest(PointTestRole::Gateway);
        let result = ping.and_then(|t| match &t.results {
            Some(PointTestResults::Ping(r)) => Some(r),
            _ => None,
        });
        if let Some(limit) = values.max_latency_ms {
            out.push(match (ping, result.and_then(|r| r.avg_ms)) {
                (None, _) => skipped(Rule::Latency, None, "no gateway ping at this point"),
                (Some(_), None) => skipped(Rule::Latency, None, "the gateway ping got no replies"),
                (Some(_), Some(avg)) => rule(
                    Rule::Latency,
                    avg <= f64::from(limit),
                    Some(avg as f32),
                    Some(limit as f32),
                    format!("gateway ping {avg:.1} ms avg, target ≤ {limit} ms"),
                ),
            });
        }
        if let Some(limit) = values.max_loss_pct {
            out.push(match (ping, result) {
                (None, _) => skipped(Rule::Loss, None, "no gateway ping at this point"),
                (Some(t), None) => skipped(
                    Rule::Loss,
                    None,
                    &format!(
                        "the gateway ping didn't run: {}",
                        t.error.as_deref().unwrap_or("no result")
                    ),
                ),
                (Some(_), Some(r)) => rule(
                    Rule::Loss,
                    r.loss_percent <= f64::from(limit),
                    Some(r.loss_percent as f32),
                    Some(limit as f32),
                    format!(
                        "gateway ping loss {:.0} % ({}/{} answered), target ≤ {limit} %",
                        r.loss_percent, r.received, r.sent
                    ),
                ),
            });
        }
    }
    out
}

pub fn summarise(evals: &[PointEvaluation], points: &[SurveyPoint]) -> FloorSummary {
    let count = |o: Outcome| evals.iter().filter(|e| e.outcome == o).count();
    let (passed, failed) = (count(Outcome::Pass), count(Outcome::Fail));
    let mut failures: BTreeMap<(Rule, Option<Band>), usize> = BTreeMap::new();
    let mut skipped: BTreeMap<(Rule, Option<Band>), usize> = BTreeMap::new();
    let mut reasons: BTreeMap<&str, usize> = BTreeMap::new();
    for e in evals {
        if let Some(r) = &e.reason {
            *reasons.entry(r).or_default() += 1;
        }
        for r in &e.rules {
            match r.outcome {
                Outcome::Fail => *failures.entry((r.rule, r.band)).or_default() += 1,
                Outcome::NotEvaluated => *skipped.entry((r.rule, r.band)).or_default() += 1,
                Outcome::Pass => {}
            }
        }
    }
    let counts = |m: BTreeMap<(Rule, Option<Band>), usize>| {
        m.into_iter()
            .map(|((rule, band), count)| RuleCount { rule, band, count })
            .collect()
    };
    let mut adapters: Vec<MeasuringAdapter> = Vec::new();
    for p in points {
        let same = |a: &MeasuringAdapter| a.id == p.adapter.id && a.hw_id == p.adapter.hw_id;
        if !adapters.iter().any(same) {
            adapters.push(p.adapter.clone());
        }
    }
    FloorSummary {
        points: evals.len(),
        passed,
        failed,
        not_evaluated: count(Outcome::NotEvaluated),
        pass_fraction_of_points: (passed + failed > 0)
            .then(|| passed as f64 / (passed + failed) as f64),
        failures_by_rule: counts(failures),
        rules_not_evaluated: counts(skipped),
        not_evaluated_reasons: reasons
            .into_iter()
            .map(|(reason, count)| ReasonCount {
                reason: reason.to_string(),
                count,
            })
            .collect(),
        adapters,
        uses_heuristic: evals.iter().flat_map(|e| &e.rules).any(|r| r.heuristic),
        width_unknown_points: evals.iter().filter(|e| e.width_unknown).count(),
    }
}

// ---------------------------------------------------------------------------
// Floor results and report snapshot
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileSource {
    FloorOverride,
    ProjectDefault,
}

/// A floor's requirements: which profile applies and how each point fared.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FloorRequirements {
    pub floor_id: i64,
    pub project_id: i64,
    /// The floor's own choice; `None` = the project default.
    pub override_profile_id: Option<i64>,
    /// The profile in effect, if any.
    pub profile: Option<RequirementProfile>,
    pub source: Option<ProfileSource>,
    pub points: Vec<PointEvaluation>,
    pub summary: Option<FloorSummary>,
}

/// What the report embeds for a floor: the profile values as they were
/// used, and the per-point summary. The share of mapped AREA passing is
/// estimated in the UI (IDW grid) and is not part of this.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequirementsSnapshot {
    pub floor_id: i64,
    pub profile_name: String,
    pub preset: Preset,
    pub preset_label: &'static str,
    pub source: ProfileSource,
    pub values: RequirementValues,
    /// Target labels (SSIDs or AP names).
    pub targets: Vec<String>,
    pub summary: FloorSummary,
    pub taken_at: DateTime<Utc>,
}

impl FloorRequirements {
    pub fn snapshot(&self) -> Option<RequirementsSnapshot> {
        let (profile, source, summary) =
            (self.profile.as_ref()?, self.source?, self.summary.clone()?);
        Some(RequirementsSnapshot {
            floor_id: self.floor_id,
            profile_name: profile.name.clone(),
            preset: profile.preset,
            preset_label: profile.preset.label(),
            source,
            values: profile.values.clone(),
            targets: profile.targets.iter().map(|t| t.label.clone()).collect(),
            summary,
            taken_at: Utc::now(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::survey::models::AdapterBands;
    use crate::wifi::models::{AdapterId, SecurityKind, Signal};

    const NET: &[u8] = b"Corp";

    fn s(bssid: &str, mhz: u32, width: Option<u32>, dbm: Option<f32>) -> Sample {
        let band = crate::wifi::channel::band_for_frequency(mhz);
        Sample {
            detail: None,
            bssid: bssid.into(),
            ssid: Some("Corp".into()),
            ssid_raw: NET.to_vec(),
            frequency_mhz: mhz,
            channel: crate::wifi::channel::channel_for_frequency(mhz),
            band,
            channel_width_mhz: width,
            channel_center_mhz: crate::wifi::channel::channel_center_mhz(mhz, width, None),
            signal: Signal {
                dbm,
                quality_percent: None,
            },
            security: SecurityKind::Wpa2Personal,
            phy_type: None,
            wifi_generation: None,
            noise_dbm: None,
            snr_db: None,
            channel_utilization_pct: None,
            station_count: None,
            last_seen_age_ms: Some(100),
            is_connected: false,
        }
    }

    fn other(bssid: &str, mhz: u32, width: Option<u32>, dbm: f32) -> Sample {
        Sample {
            ssid: Some("Neighbour".into()),
            ssid_raw: b"Neighbour".to_vec(),
            ..s(bssid, mhz, width, Some(dbm))
        }
    }

    fn bands(six: Capability) -> Option<AdapterBands> {
        Some(AdapterBands {
            band_2ghz: Capability::Supported,
            band_5ghz: Capability::Supported,
            band_6ghz: six,
        })
    }

    fn point(samples: Vec<Sample>) -> SurveyPoint {
        SurveyPoint {
            id: 1,
            floor_id: 1,
            x: 0.0,
            y: 0.0,
            measured_at: Utc::now(),
            scan_duration_ms: 3000,
            adapter: MeasuringAdapter {
                id: AdapterId::linux("wlan0"),
                provider: "networkmanager".into(),
                model: None,
                driver: None,
                hw_id: None,
            },
            adapter_bands: bands(Capability::Supported),
            samples,
        }
    }

    fn profile(preset: Preset, targets: Vec<TargetInfo>) -> RequirementProfile {
        RequirementProfile {
            id: 1,
            project_id: 1,
            name: "P".into(),
            preset,
            values: preset.values().unwrap(),
            is_default: true,
            targets,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn ssid_target() -> Vec<TargetInfo> {
        vec![TargetInfo {
            target: RequirementTarget::Ssid {
                ssid_raw: NET.to_vec(),
            },
            label: "Corp".into(),
            bssids: vec![],
        }]
    }

    fn placed(id: i64, bssids: &[&str]) -> PlacedAp {
        PlacedAp {
            id,
            floor_id: 1,
            name: format!("AP {id}"),
            x: 0.0,
            y: 0.0,
            model: None,
            notes: None,
            bssids: bssids.iter().map(|b| b.to_string()).collect(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn result(e: &PointEvaluation, rule: Rule) -> &RuleResult {
        e.rules.iter().find(|r| r.rule == rule).unwrap()
    }

    fn eval(p: &RequirementProfile, aps: &[PlacedAp], pt: &SurveyPoint) -> PointEvaluation {
        Evaluator::new(p, aps).evaluate(pt)
    }

    #[test]
    fn presets_validate_and_changed_values_become_custom() {
        for info in presets() {
            if let Some(v) = &info.values {
                assert_eq!(&v.validated().unwrap(), v, "{:?}", info.preset);
                assert_eq!(v.min_snr_db, None, "presets never set SNR");
            }
        }
        let office = Preset::OfficeData.values().unwrap();
        let input = |values| RequirementProfileInput {
            name: "  Office ".into(),
            preset: Preset::OfficeData,
            values,
            is_default: false,
        };
        let (name, preset, _) = input(office.clone()).validated().unwrap();
        assert_eq!((name.as_str(), preset), ("Office", Preset::OfficeData));
        let tweaked = RequirementValues {
            primary_min_dbm: -70,
            ..office.clone()
        };
        assert_eq!(input(tweaked).validated().unwrap().1, Preset::Custom);

        let bad = [
            RequirementValues {
                primary_min_dbm: -10,
                ..office.clone()
            },
            RequirementValues {
                cochannel_level_dbm: None,
                ..office.clone()
            },
            RequirementValues {
                required_bands: vec![Band::Band60GHz],
                ..office.clone()
            },
            RequirementValues {
                max_util_pct: Some(101),
                ..office.clone()
            },
        ];
        for v in bad {
            assert!(v.validated().is_err(), "{v:?}");
        }
        let dup = RequirementValues {
            required_bands: vec![Band::Band6GHz, Band::Band5GHz, Band::Band6GHz],
            ..office
        };
        assert_eq!(
            dup.validated().unwrap().required_bands,
            [Band::Band5GHz, Band::Band6GHz]
        );
    }

    #[test]
    fn ap_key_matches_the_ui_heuristic() {
        assert_eq!(ap_key("AA:BB:CC:DD:EE:F1"), "bb:cc:dd:ee");
        assert_eq!(ap_key("AE:BB:CC:DD:EE:F2"), "bb:cc:dd:ee");
    }

    #[test]
    fn passes_with_strong_primary_and_a_second_ap() {
        let p = profile(Preset::OfficeData, ssid_target());
        let pt = point(vec![
            s("00:11:11:11:11:01", 5180, Some(80), Some(-55.0)),
            s("00:22:22:22:22:01", 5500, Some(80), Some(-72.0)),
            s("00:11:11:11:11:02", 2412, Some(20), Some(-70.0)),
        ]);
        let e = eval(&p, &[], &pt);
        assert_eq!(e.outcome, Outcome::Pass, "{e:#?}");
        assert_eq!(e.primary_bssid.as_deref(), Some("00:11:11:11:11:01"));
        assert_eq!((e.primary_dbm, e.secondary_dbm), (Some(-55.0), Some(-72.0)));
        assert_eq!(e.cochannel_count, Some(0));
        // Primary, secondary, co-channel, 5 GHz.
        assert_eq!(e.rules.len(), 4);

        // The strongest target decides the primary, whatever its band.
        let pt = point(vec![
            s("00:11:11:11:11:01", 5180, Some(80), Some(-55.0)),
            s("00:22:22:22:22:01", 5500, Some(80), Some(-72.0)),
            s("00:11:11:11:11:02", 2412, Some(20), Some(-50.0)),
        ]);
        let e = eval(&p, &[], &pt);
        assert_eq!(e.primary_bssid.as_deref(), Some("00:11:11:11:11:02"));
        // No other AP on 2.4 GHz.
        let sec = result(&e, Rule::SecondarySignal);
        assert_eq!(sec.outcome, Outcome::Fail);
        assert!(sec.detail.contains("2.4 GHz"), "{}", sec.detail);
    }

    #[test]
    fn secondary_must_be_another_physical_ap_on_the_same_band_and_ssid() {
        let p = profile(Preset::OfficeData, ssid_target());
        // Same base MAC (differs in the last octet) = same AP by heuristic.
        let pt = point(vec![
            s("00:11:11:11:11:01", 5180, Some(80), Some(-55.0)),
            s("00:11:11:11:11:05", 5500, Some(80), Some(-60.0)),
            other("00:33:33:33:33:01", 5500, Some(80), -60.0),
            s("00:22:22:22:22:01", 2412, Some(20), Some(-60.0)),
        ]);
        let e = eval(&p, &[], &pt);
        let sec = result(&e, Rule::SecondarySignal);
        assert_eq!(sec.outcome, Outcome::Fail, "{sec:?}");
        assert!(sec.heuristic);
        assert_eq!(e.secondary_dbm, None);
        assert_eq!(e.outcome, Outcome::Fail);

        // Placed APs override the heuristic: linked to different APs.
        let aps = [
            placed(1, &["00:11:11:11:11:01"]),
            placed(2, &["00:11:11:11:11:05"]),
        ];
        let e = eval(&p, &aps, &pt);
        let sec = result(&e, Rule::SecondarySignal);
        assert_eq!(sec.outcome, Outcome::Pass, "{sec:?}");
        assert!(!sec.heuristic);
        assert_eq!(e.secondary_dbm, Some(-60.0));

        // ... and linked to the same AP, even with different base MACs.
        let pt2 = point(vec![
            s("00:11:11:11:11:01", 5180, Some(80), Some(-55.0)),
            s("02:99:99:99:99:01", 5500, Some(80), Some(-60.0)),
        ]);
        let aps = [placed(1, &["00:11:11:11:11:01", "02:99:99:99:99:01"])];
        assert_eq!(
            result(&eval(&p, &aps, &pt2), Rule::SecondarySignal).outcome,
            Outcome::Fail
        );
    }

    #[test]
    fn weak_primary_fails_and_names_the_rule() {
        let p = profile(Preset::OfficeData, ssid_target());
        let e = eval(
            &p,
            &[],
            &point(vec![s("00:11:11:11:11:01", 5180, Some(20), Some(-70.0))]),
        );
        assert_eq!(e.outcome, Outcome::Fail);
        let r = result(&e, Rule::PrimarySignal);
        assert_eq!(
            (r.outcome, r.measured, r.limit),
            (Outcome::Fail, Some(-70.0), Some(-67.0))
        );
    }

    #[test]
    fn dead_zone_fails_but_percent_only_is_never_failed() {
        let p = profile(Preset::OfficeData, ssid_target());
        let e = eval(&p, &[], &point(vec![]));
        assert_eq!(e.outcome, Outcome::Fail);
        assert_eq!(
            result(&e, Rule::PrimarySignal).detail,
            "no target network heard"
        );
        assert_eq!(result(&e, Rule::CoChannel).outcome, Outcome::NotEvaluated);

        let pct = point(vec![s("00:11:11:11:11:01", 5180, None, None)]);
        let e = eval(&p, &[], &pct);
        assert_eq!(e.outcome, Outcome::NotEvaluated);
        assert!(e.reason.unwrap().contains("% only"));
        assert!(e.rules.is_empty());

        // Target heard only as %, other networks in dBm: still not failed.
        let mixed = point(vec![
            s("00:11:11:11:11:01", 5180, None, None),
            other("00:33:33:33:33:01", 5180, Some(20), -50.0),
        ]);
        assert_eq!(eval(&p, &[], &mixed).outcome, Outcome::NotEvaluated);
    }

    #[test]
    fn no_targets_means_not_evaluated() {
        let p = profile(Preset::OfficeData, vec![]);
        let e = eval(
            &p,
            &[],
            &point(vec![s("00:11:11:11:11:01", 5180, None, Some(-40.0))]),
        );
        assert_eq!(e.outcome, Outcome::NotEvaluated);
        assert!(e.reason.unwrap().contains("no target"));
    }

    #[test]
    fn co_channel_counts_radios_on_overlapping_spans() {
        let p = profile(Preset::VoiceVideo, ssid_target()); // max 1 at -80
        let pt = point(vec![
            // Primary: ch 36 @80 (5170–5250).
            s("00:11:11:11:11:01", 5180, Some(80), Some(-50.0)),
            // Same radio, other SSID: not counted.
            other("02:11:11:11:11:01", 5180, Some(80), -50.0),
            // Neighbour on ch 44/20 (inside the 80 MHz block): counted.
            other("00:33:33:33:33:01", 5220, Some(20), -70.0),
            // Two SSIDs on one neighbour radio: counted once.
            other("00:44:44:44:44:01", 5200, Some(40), -75.0),
            other("02:44:44:44:44:01", 5200, Some(40), -75.0),
            // Below the level: ignored.
            other("00:55:55:55:55:01", 5180, Some(20), -85.0),
            // Ch 52: outside the span.
            other("00:66:66:66:66:01", 5260, Some(20), -50.0),
        ]);
        let e = eval(&p, &[], &pt);
        let r = result(&e, Rule::CoChannel);
        assert_eq!(e.cochannel_count, Some(2), "{r:?}");
        assert_eq!((r.outcome, r.limit), (Outcome::Fail, Some(1.0)));
        assert!(!e.width_unknown);
    }

    #[test]
    fn co_channel_with_unknown_width_uses_the_primary_channel_and_says_so() {
        let p = profile(Preset::OfficeData, ssid_target());
        let pt = point(vec![
            s("00:11:11:11:11:01", 5180, None, Some(-50.0)),
            // Would overlap an 80 MHz block, but not 20 MHz ch 36.
            other("00:33:33:33:33:01", 5220, Some(20), -60.0),
            // A neighbour's 80 MHz block covering ch 36: counted.
            other("00:44:44:44:44:01", 5240, Some(80), -60.0),
        ]);
        let e = eval(&p, &[], &pt);
        assert!(e.width_unknown);
        assert_eq!(e.cochannel_count, Some(1));
        assert!(result(&e, Rule::CoChannel).detail.contains("width unknown"));
    }

    #[test]
    fn two_four_ghz_overlap_follows_the_spans() {
        let p = profile(Preset::OfficeData, ssid_target());
        let pt = point(vec![
            s("00:11:11:11:11:01", 2437, Some(20), Some(-50.0)), // ch 6
            other("00:33:33:33:33:01", 2412, Some(20), -60.0),   // ch 1: clear
            other("00:44:44:44:44:01", 2447, Some(20), -60.0),   // ch 8: overlaps
            other("00:55:55:55:55:01", 2462, Some(20), -60.0),   // ch 11: clear
        ]);
        assert_eq!(eval(&p, &[], &pt).cochannel_count, Some(1));
    }

    #[test]
    fn required_band_depends_on_what_the_card_can_receive() {
        let mut p = profile(Preset::OfficeData, ssid_target());
        p.values.required_bands = vec![Band::Band5GHz, Band::Band6GHz];
        let only_5 = vec![
            s("00:11:11:11:11:01", 5180, Some(20), Some(-50.0)),
            s("00:22:22:22:22:01", 5180, Some(20), Some(-60.0)),
        ];
        let band_result = |e: &PointEvaluation, b: Band| {
            e.rules
                .iter()
                .find(|r| r.rule == Rule::RequiredBand && r.band == Some(b))
                .unwrap()
                .clone()
        };

        let mut pt = point(only_5.clone());
        pt.adapter_bands = bands(Capability::Supported);
        let e = eval(&p, &[], &pt);
        assert_eq!(band_result(&e, Band::Band5GHz).outcome, Outcome::Pass);
        assert_eq!(band_result(&e, Band::Band6GHz).outcome, Outcome::Fail);
        assert_eq!(e.outcome, Outcome::Fail);

        pt.adapter_bands = bands(Capability::Unsupported);
        let e = eval(&p, &[], &pt);
        let six = band_result(&e, Band::Band6GHz);
        assert_eq!(six.outcome, Outcome::NotEvaluated);
        assert!(six.detail.contains("can't receive 6 GHz"), "{}", six.detail);
        assert_eq!(e.outcome, Outcome::Pass);

        pt.adapter_bands = bands(Capability::Unknown);
        assert_eq!(
            band_result(&eval(&p, &[], &pt), Band::Band6GHz).outcome,
            Outcome::NotEvaluated
        );
        pt.adapter_bands = None;
        assert!(band_result(&eval(&p, &[], &pt), Band::Band6GHz)
            .detail
            .contains("wasn't recorded"));

        // Hearing anything on 6 GHz proves the card receives it.
        let mut with_6 = only_5;
        with_6.push(other("00:77:77:77:77:01", 5955, Some(20), -80.0));
        let pt = SurveyPoint {
            adapter_bands: None,
            ..point(with_6)
        };
        assert_eq!(
            band_result(&eval(&p, &[], &pt), Band::Band6GHz).outcome,
            Outcome::Fail
        );

        // A target on the band, but below the primary level.
        let weak = point(vec![
            s("00:11:11:11:11:01", 2412, Some(20), Some(-50.0)),
            s("00:11:11:11:11:02", 5180, Some(20), Some(-75.0)),
        ]);
        let five = band_result(&eval(&p, &[], &weak), Band::Band5GHz);
        assert_eq!((five.outcome, five.measured), (Outcome::Fail, Some(-75.0)));
    }

    #[test]
    fn snr_and_utilisation_only_where_reported() {
        let mut p = profile(Preset::WarehouseBasic, ssid_target());
        p.values.min_snr_db = Some(25);
        p.values.max_util_pct = Some(50);
        let mut primary = s("00:11:11:11:11:01", 5180, Some(20), Some(-60.0));
        let e = eval(&p, &[], &point(vec![primary.clone()]));
        assert_eq!(result(&e, Rule::Snr).outcome, Outcome::NotEvaluated);
        assert_eq!(result(&e, Rule::Utilisation).outcome, Outcome::NotEvaluated);
        assert_eq!(e.outcome, Outcome::Pass);

        primary.noise_dbm = Some(-80.0);
        primary.channel_utilization_pct = Some(70.0);
        let e = eval(&p, &[], &point(vec![primary]));
        let snr = result(&e, Rule::Snr);
        assert_eq!((snr.outcome, snr.measured), (Outcome::Fail, Some(20.0)));
        let util = result(&e, Rule::Utilisation);
        assert_eq!(util.outcome, Outcome::Fail);
        assert!(util.detail.contains("BSS Load"));
    }

    #[test]
    fn ap_targets_match_hidden_ssids_by_bssid() {
        let target = TargetInfo {
            target: RequirementTarget::Ap { ap_id: 7 },
            label: "Lobby".into(),
            bssids: vec!["00:11:11:11:11:01".into()],
        };
        let p = profile(Preset::WarehouseBasic, vec![target]);
        let hidden = Sample {
            ssid: None,
            ssid_raw: vec![],
            ..s("00:11:11:11:11:01", 5180, Some(20), Some(-60.0))
        };
        let louder_other = other("00:99:99:99:99:01", 5180, Some(20), -40.0);
        let e = eval(&p, &[], &point(vec![louder_other, hidden]));
        assert_eq!(e.primary_bssid.as_deref(), Some("00:11:11:11:11:01"));
        assert_eq!(e.outcome, Outcome::Pass);
        // The louder neighbour isn't a target, but it is co-channel.
        assert_eq!(e.cochannel_count, Some(1));
    }

    #[test]
    fn summary_counts_points_rules_and_adapters() {
        let p = profile(Preset::WarehouseBasic, ssid_target());
        let mut pts = vec![
            point(vec![s("00:11:11:11:11:01", 5180, Some(20), Some(-60.0))]),
            point(vec![s("00:11:11:11:11:01", 5180, None, Some(-80.0))]),
            point(vec![s("00:11:11:11:11:01", 5180, Some(20), None)]),
        ];
        pts[2].adapter.id = AdapterId::linux("wlan1");
        let (evals, sum) = evaluate_floor(&p, &[], &pts, &[]);
        assert_eq!(evals.len(), 3);
        assert_eq!(
            (sum.points, sum.passed, sum.failed, sum.not_evaluated),
            (3, 1, 1, 1)
        );
        assert_eq!(sum.pass_fraction_of_points, Some(0.5));
        assert_eq!(
            sum.failures_by_rule,
            [RuleCount {
                rule: Rule::PrimarySignal,
                band: None,
                count: 1
            }]
        );
        assert_eq!(sum.not_evaluated_reasons.len(), 1);
        assert_eq!(sum.adapters.len(), 2);
        assert_eq!(sum.width_unknown_points, 1);

        let floor = FloorRequirements {
            floor_id: 1,
            project_id: 1,
            override_profile_id: None,
            profile: Some(p),
            source: Some(ProfileSource::ProjectDefault),
            points: evals,
            summary: Some(sum),
        };
        let snap = floor.snapshot().unwrap();
        assert_eq!(snap.preset_label, "Warehouse / basic coverage");
        assert_eq!(snap.values.primary_min_dbm, -72);
        assert_eq!(snap.targets, ["Corp"]);
        let json = serde_json::to_value(&snap).unwrap();
        assert_eq!(json["values"]["cochannelMax"], 3);
        assert_eq!(json["summary"]["passFractionOfPoints"], 0.5);
    }

    #[test]
    fn targets_serialise_with_a_kind_tag() {
        let t: RequirementTarget =
            serde_json::from_str(r#"{"kind":"ssid","ssidRaw":[67,111]}"#).unwrap();
        assert_eq!(
            t,
            RequirementTarget::Ssid {
                ssid_raw: b"Co".to_vec()
            }
        );
        let t: RequirementTarget = serde_json::from_str(r#"{"kind":"ap","apId":3}"#).unwrap();
        assert_eq!(t, RequirementTarget::Ap { ap_id: 3 });
        let info = TargetInfo {
            target: t,
            label: "x".into(),
            bssids: vec![],
        };
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(
            (json["kind"].as_str(), json["apId"].as_i64()),
            (Some("ap"), Some(3))
        );
    }

    mod speed {
        use super::super::*;
        use crate::nettools::iperf3::{Iperf3Direction, Iperf3Result, MeasuredBy};
        use crate::nettools::{PingResult, ProbeMethod, ProbeOutcome};
        use crate::survey::models::{LinkSnapshot, PointTestKind, PointTestMethod};
        use crate::wifi::models::AdapterId;

        fn test(
            id: i64,
            role: PointTestRole,
            status: PointTestStatus,
            results: Option<PointTestResults>,
        ) -> PointTest {
            PointTest {
                id,
                point_id: 1,
                kind: if matches!(role, PointTestRole::Gateway) {
                    PointTestKind::Ping
                } else {
                    PointTestKind::Iperf3
                },
                target: "192.168.1.10".into(),
                role,
                method: PointTestMethod::Iperf3Tcp,
                status,
                started_at: Utc::now() + chrono::Duration::seconds(id),
                duration_ms: 1000,
                adapter_id: AdapterId::linux("wlan0"),
                link: LinkSnapshot::default(),
                link_after: None,
                roamed: None,
                results,
                error: (status == PointTestStatus::Failed).then(|| "server busy".into()),
                error_hint: None,
            }
        }

        fn iperf(direction: Iperf3Direction, mbps: f64) -> Option<PointTestResults> {
            Some(PointTestResults::Iperf3(Iperf3Result {
                version: 1,
                direction,
                server: "192.168.1.10:5201".into(),
                streams: 4,
                duration_s: 5,
                omit_s: 1,
                bits_per_second: mbps * 1e6,
                receiver_bytes: 0,
                receiver_seconds: 5.0,
                measured_by: MeasuredBy::Server,
                sender_bytes: None,
                retransmits: None,
                retransmits_source: None,
                intervals: Vec::new(),
            }))
        }

        fn ping(rtts: &[Option<f64>]) -> Option<PointTestResults> {
            let probes = rtts
                .iter()
                .map(|r| match r {
                    Some(rtt_ms) => ProbeOutcome::Reply { rtt_ms: *rtt_ms },
                    None => ProbeOutcome::Timeout,
                })
                .collect();
            Some(PointTestResults::Ping(PingResult::from_probes(
                ProbeMethod::Icmp,
                None,
                None,
                probes,
            )))
        }

        fn targets() -> RequirementValues {
            RequirementValues {
                min_download_mbps: Some(300),
                min_upload_mbps: Some(100),
                max_latency_ms: Some(10),
                max_loss_pct: Some(1),
                ..RequirementValues::default()
            }
        }

        fn outcome(rules: &[RuleResult], r: Rule) -> Outcome {
            rules.iter().find(|x| x.rule == r).unwrap().outcome
        }

        #[test]
        fn speed_targets_judge_the_latest_tests() {
            let tests = [
                test(
                    1,
                    PointTestRole::Iperf3Download,
                    PointTestStatus::Ok,
                    iperf(Iperf3Direction::Download, 120.0),
                ),
                // The newer download wins; the cancelled one after it is ignored.
                test(
                    2,
                    PointTestRole::Iperf3Download,
                    PointTestStatus::Ok,
                    iperf(Iperf3Direction::Download, 450.0),
                ),
                test(
                    3,
                    PointTestRole::Iperf3Download,
                    PointTestStatus::Cancelled,
                    None,
                ),
                test(
                    4,
                    PointTestRole::Iperf3Upload,
                    PointTestStatus::Ok,
                    iperf(Iperf3Direction::Upload, 80.0),
                ),
                test(
                    5,
                    PointTestRole::Gateway,
                    PointTestStatus::Ok,
                    ping(&[Some(3.0), Some(5.0), None, Some(4.0)]),
                ),
            ];
            let refs: Vec<&PointTest> = tests.iter().collect();
            let rules = evaluate_speed(&targets(), &refs);
            assert_eq!(rules.len(), 4);
            assert_eq!(outcome(&rules, Rule::Download), Outcome::Pass);
            assert_eq!(rules[0].measured, Some(450.0));
            assert_eq!(outcome(&rules, Rule::Upload), Outcome::Fail);
            assert_eq!(outcome(&rules, Rule::Latency), Outcome::Pass);
            // 1 of 4 lost: 25 % > 1 %.
            assert_eq!(outcome(&rules, Rule::Loss), Outcome::Fail);
        }

        #[test]
        fn missing_or_failed_tests_are_not_evaluated() {
            let failed = [test(
                1,
                PointTestRole::Iperf3Download,
                PointTestStatus::Failed,
                None,
            )];
            let refs: Vec<&PointTest> = failed.iter().collect();
            let rules = evaluate_speed(&targets(), &refs);
            assert_eq!(outcome(&rules, Rule::Download), Outcome::NotEvaluated);
            assert!(
                rules[0].detail.contains("server busy"),
                "{}",
                rules[0].detail
            );
            assert_eq!(outcome(&rules, Rule::Upload), Outcome::NotEvaluated);
            assert_eq!(outcome(&rules, Rule::Latency), Outcome::NotEvaluated);
            // No targets set: no speed rules at all.
            assert!(evaluate_speed(&RequirementValues::default(), &refs).is_empty());
            // A ping with no replies: loss is judged, latency isn't.
            let silent = [test(
                1,
                PointTestRole::Gateway,
                PointTestStatus::Failed,
                ping(&[None, None]),
            )];
            let refs: Vec<&PointTest> = silent.iter().collect();
            let rules = evaluate_speed(&targets(), &refs);
            assert_eq!(outcome(&rules, Rule::Latency), Outcome::NotEvaluated);
            assert_eq!(outcome(&rules, Rule::Loss), Outcome::Fail);
        }

        #[test]
        fn speed_targets_are_validated() {
            let ok = RequirementValues {
                primary_min_dbm: -67,
                ..targets()
            };
            assert!(ok.validated().is_ok());
            for bad in [
                RequirementValues {
                    min_download_mbps: Some(0),
                    ..ok.clone()
                },
                RequirementValues {
                    max_latency_ms: Some(0),
                    ..ok.clone()
                },
                RequirementValues {
                    max_loss_pct: Some(101),
                    ..ok.clone()
                },
            ] {
                assert!(bad.validated().is_err(), "{bad:?}");
            }
        }
    }
}
