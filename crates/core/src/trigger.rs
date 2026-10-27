//! Pause heuristics.
//!
//! A [`Monitor`] is built when the user arms the plugin and is fed one
//! [`AircraftState`] sample per tick while the sim runs. The first enabled
//! condition that becomes true wins; the plugin then pauses and disarms.

use crate::geo::distance_nm;
use crate::settings::{AltRef, Direction, Settings};

/// A resolved navigation point.
#[derive(Debug, Clone, PartialEq)]
pub struct Waypoint {
    pub id: String,
    /// Human readable kind, e.g. "airport", "fix", "VOR", "FMS leg 4".
    pub kind: String,
    pub lat: f64,
    pub lon: f64,
    pub elevation_ft: Option<f64>,
}

/// One sample of the user's aircraft.
#[derive(Debug, Clone, Copy, Default)]
pub struct AircraftState {
    pub lat: f64,
    pub lon: f64,
    pub alt_msl_ft: f64,
    pub alt_agl_ft: f64,
    pub vs_fpm: f64,
    /// Sim time that only advances while unpaused.
    pub flight_time_s: f64,
    /// Distance to top of descent published by the FMS, when available.
    pub fms_tod_nm: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Fired {
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConditionKind {
    Waypoint,
    Altitude,
    TopOfDescent,
    Timer,
}

/// Live progress of one armed condition, for display.
#[derive(Debug, Clone, PartialEq)]
pub struct ConditionStatus {
    pub kind: ConditionKind,
    pub text: String,
    /// 0.0 when armed, 1.0 when about to fire. `None` when it can't be estimated.
    pub progress: Option<f32>,
}

/// Altitude jumps larger than this between two samples are treated as a reposition.
const TELEPORT_FT: f64 = 2000.0;
/// ToD logic is inactive below this height so it never fires on the ground or right after takeoff.
const TOD_MIN_AGL_FT: f64 = 1000.0;
/// Level-flight window and minimum height that count as "cruise" for the descent-onset fallback.
const LEVEL_VS_FPM: f64 = 300.0;
const CRUISE_MIN_S: f64 = 60.0;
const CRUISE_MIN_AGL_FT: f64 = 5000.0;
/// How long a descent must be sustained before the fallback fires.
const DESCENT_CONFIRM_S: f64 = 10.0;
/// Default end-of-descent height above the point when the user gives no crossing altitude.
const DEFAULT_CROSS_ABOVE_FT: f64 = 1500.0;

#[derive(Debug)]
enum Condition {
    Waypoint { wpt: Waypoint, within_nm: f64 },
    Altitude { target_ft: f64, dir: Direction, reference: AltRef, prev: Option<f64> },
    TopOfDescent(Tod),
    Timer { seconds: f64, start: Option<f64> },
}

#[derive(Debug)]
struct Tod {
    point: Option<Waypoint>,
    cross_ft: f64,
    lead_nm: f64,
    nm_per_kft: f64,
    descent_vs_fpm: f64,
    fms_seen_positive: bool,
    level_since: Option<f64>,
    cruise_seen: bool,
    descent_since: Option<f64>,
}

impl Tod {
    /// Distance to go before the geometric (N nm per 1000 ft) top of descent.
    fn geometric_nm(&self, s: &AircraftState) -> Option<f64> {
        let p = self.point.as_ref()?;
        let to_lose = (s.alt_msl_ft - self.cross_ft).max(0.0);
        let descent_nm = to_lose / 1000.0 * self.nm_per_kft;
        Some(distance_nm(s.lat, s.lon, p.lat, p.lon) - descent_nm)
    }

    fn update(&mut self, s: &AircraftState) -> Option<String> {
        if s.alt_agl_ft < TOD_MIN_AGL_FT {
            self.level_since = None;
            self.descent_since = None;
            return None;
        }

        // 1. The FMS knows best, when it publishes a value.
        if let Some(d) = s.fms_tod_nm {
            if d > 0.0 {
                self.fms_seen_positive = true;
            }
            if self.fms_seen_positive && d <= self.lead_nm {
                return Some(format!("Top of descent in {d:.1} nm (FMS)"));
            }
        }

        // 2. Geometric rule of thumb towards the end-of-descent point.
        if let Some(d) = self.geometric_nm(s) {
            if d <= self.lead_nm {
                let id = &self.point.as_ref().map(|p| p.id.as_str()).unwrap_or("");
                return Some(format!("Top of descent for {id} ({:.0} nm/1000 ft)", self.nm_per_kft));
            }
        }

        // 3. Fallback: a sustained descent after a period of level cruise.
        let t = s.flight_time_s;
        if s.vs_fpm.abs() < LEVEL_VS_FPM {
            let since = *self.level_since.get_or_insert(t);
            if t - since >= CRUISE_MIN_S && s.alt_agl_ft >= CRUISE_MIN_AGL_FT {
                self.cruise_seen = true;
            }
        } else {
            self.level_since = None;
        }
        if s.vs_fpm <= self.descent_vs_fpm {
            let since = *self.descent_since.get_or_insert(t);
            if self.cruise_seen && t - since >= DESCENT_CONFIRM_S {
                return Some("Descent from cruise detected".into());
            }
        } else {
            self.descent_since = None;
        }
        None
    }

    fn status(&self, s: &AircraftState) -> String {
        if let Some(d) = s.fms_tod_nm.filter(|d| *d > 0.0) {
            return format!("T/D in {d:.1} nm (FMS), pause at {:.0} nm", self.lead_nm);
        }
        match (self.geometric_nm(s), &self.point) {
            (Some(d), Some(p)) => format!(
                "T/D in {:.1} nm (to {} at {} ft), pause at {:.0} nm",
                d.max(0.0),
                p.id,
                thousands(self.cross_ft),
                self.lead_nm
            ),
            _ if self.cruise_seen => "Cruise seen, watching for the descent".into(),
            _ => "No destination: waiting for cruise, then descent".into(),
        }
    }

    fn remaining_nm(&self, s: &AircraftState) -> Option<f64> {
        let d = s.fms_tod_nm.filter(|d| *d > 0.0).or_else(|| self.geometric_nm(s))?;
        Some((d - self.lead_nm).max(0.0))
    }
}

impl Condition {
    fn kind(&self) -> ConditionKind {
        match self {
            Condition::Waypoint { .. } => ConditionKind::Waypoint,
            Condition::Altitude { .. } => ConditionKind::Altitude,
            Condition::TopOfDescent(_) => ConditionKind::TopOfDescent,
            Condition::Timer { .. } => ConditionKind::Timer,
        }
    }

    /// How far from firing, in the condition's own unit (nm, ft or s).
    fn remaining(&self, s: &AircraftState) -> Option<f64> {
        match self {
            Condition::Waypoint { wpt, within_nm } => {
                Some((distance_nm(s.lat, s.lon, wpt.lat, wpt.lon) - within_nm).max(0.0))
            }
            Condition::Altitude { target_ft, reference, .. } => Some((altitude(s, *reference) - target_ft).abs()),
            Condition::TopOfDescent(t) => t.remaining_nm(s),
            Condition::Timer { seconds, start } => {
                start.map(|st| (seconds - (s.flight_time_s - st)).max(0.0))
            }
        }
    }
}

fn altitude(s: &AircraftState, reference: AltRef) -> f64 {
    match reference {
        AltRef::Msl => s.alt_msl_ft,
        AltRef::Agl => s.alt_agl_ft,
    }
}

#[derive(Debug, Default)]
pub struct Monitor {
    conditions: Vec<Condition>,
    /// `remaining()` at the first sample after arming, the 0% mark of progress.
    initial: Vec<Option<f64>>,
}

impl Monitor {
    /// Builds the armed set of conditions. Waypoints are resolved by the caller
    /// (nav database / FMS), which is why they are passed in separately.
    pub fn new(settings: &Settings, wpt: Option<Waypoint>, tod_point: Option<Waypoint>) -> Self {
        let mut conditions = Vec::new();
        if settings.wpt_enabled {
            if let Some(wpt) = wpt {
                conditions.push(Condition::Waypoint { wpt, within_nm: settings.wpt_within_nm });
            }
        }
        if settings.alt_enabled {
            conditions.push(Condition::Altitude {
                target_ft: settings.alt_ft,
                dir: settings.alt_dir,
                reference: settings.alt_ref,
                prev: None,
            });
        }
        if settings.tod_enabled {
            let cross_ft = settings.tod_cross_ft.unwrap_or_else(|| {
                tod_point.as_ref().and_then(|p| p.elevation_ft).unwrap_or(0.0) + DEFAULT_CROSS_ABOVE_FT
            });
            conditions.push(Condition::TopOfDescent(Tod {
                point: tod_point,
                cross_ft,
                lead_nm: settings.tod_lead_nm,
                nm_per_kft: settings.tod_nm_per_kft.max(0.5),
                descent_vs_fpm: -settings.descent_vs_fpm.abs(),
                fms_seen_positive: false,
                level_since: None,
                cruise_seen: false,
                descent_since: None,
            }));
        }
        if settings.timer_enabled {
            conditions.push(Condition::Timer { seconds: settings.timer_min * 60.0, start: None });
        }
        let initial = vec![None; conditions.len()];
        Self { conditions, initial }
    }

    pub fn is_empty(&self) -> bool {
        self.conditions.is_empty()
    }

    /// Forgets edge-detection history, e.g. after the aircraft was repositioned.
    pub fn reset_tracking(&mut self) {
        for c in &mut self.conditions {
            match c {
                Condition::Altitude { prev, .. } => *prev = None,
                Condition::TopOfDescent(t) => {
                    t.level_since = None;
                    t.descent_since = None;
                    t.fms_seen_positive = false;
                }
                _ => {}
            }
        }
    }

    /// Feeds one sample. Returns the reason to pause, if any condition fired.
    pub fn update(&mut self, s: &AircraftState) -> Option<Fired> {
        let mut fired = None;
        for (c, initial) in self.conditions.iter_mut().zip(&mut self.initial) {
            let reason = match c {
                Condition::Waypoint { wpt, within_nm } => {
                    let d = distance_nm(s.lat, s.lon, wpt.lat, wpt.lon);
                    (d <= *within_nm).then(|| format!("{:.1} nm from {}", d, wpt.id))
                }
                Condition::Altitude { target_ft, dir, reference, prev } => {
                    let alt = altitude(s, *reference);
                    let crossed = match *prev {
                        Some(p) if (alt - p).abs() < TELEPORT_FT => {
                            let down = p > *target_ft && alt <= *target_ft;
                            let up = p < *target_ft && alt >= *target_ft;
                            match dir {
                                Direction::Descending => down,
                                Direction::Climbing => up,
                                Direction::Either => down || up,
                            }
                        }
                        _ => false,
                    };
                    *prev = Some(alt);
                    crossed.then(|| {
                        let verb = if s.vs_fpm < 0.0 { "Descending" } else { "Climbing" };
                        format!("{verb} through {} ft {}", thousands(*target_ft), reference.label())
                    })
                }
                Condition::TopOfDescent(t) => t.update(s),
                Condition::Timer { seconds, start } => {
                    // A new flight resets the sim clock; restart the countdown with it.
                    let st = match *start {
                        Some(st) if st <= s.flight_time_s => st,
                        _ => *start.insert(s.flight_time_s),
                    };
                    (s.flight_time_s - st >= *seconds)
                        .then(|| format!("{:.0} min timer elapsed", *seconds / 60.0))
                }
            };
            if initial.is_none() {
                *initial = c.remaining(s);
            }
            // Keep updating the remaining conditions so their history stays consistent.
            if fired.is_none() {
                fired = reason.map(|reason| Fired { reason });
            }
        }
        fired
    }

    /// Live progress per armed condition, in arming order.
    pub fn status(&self, s: &AircraftState) -> Vec<ConditionStatus> {
        self.conditions
            .iter()
            .zip(&self.initial)
            .map(|(c, initial)| {
                let text = match c {
                    Condition::Waypoint { wpt, within_nm } => format!(
                        "{} ({}): {:.1} nm to go, pause at {:.0} nm",
                        wpt.id,
                        wpt.kind,
                        distance_nm(s.lat, s.lon, wpt.lat, wpt.lon),
                        within_nm
                    ),
                    Condition::Altitude { target_ft, dir, reference, .. } => format!(
                        "{} ft {} now, pause {} through {} ft",
                        thousands(altitude(s, *reference)),
                        reference.label(),
                        dir.label(),
                        thousands(*target_ft)
                    ),
                    Condition::TopOfDescent(t) => t.status(s),
                    Condition::Timer { seconds, start } => {
                        let left = (seconds - start.map_or(0.0, |st| s.flight_time_s - st)).max(0.0) as u64;
                        format!("{}:{:02}:{:02} of sim time left", left / 3600, left / 60 % 60, left % 60)
                    }
                };
                let progress = match (*initial, c.remaining(s)) {
                    (Some(init), Some(rem)) if init > 0.0 => Some((1.0 - rem / init).clamp(0.0, 1.0) as f32),
                    _ => None,
                };
                ConditionStatus { kind: c.kind(), text, progress }
            })
            .collect()
    }
}

/// Formats feet with thousands separators: 10000 -> "10,000".
pub fn thousands(v: f64) -> String {
    let n = v.round() as i64;
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    if n < 0 {
        out.insert(0, '-');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn only(f: impl FnOnce(&mut Settings)) -> Settings {
        let mut s = Settings { tod_enabled: false, ..Settings::default() };
        f(&mut s);
        s
    }

    fn wpt(id: &str, lat: f64, lon: f64) -> Waypoint {
        Waypoint { id: id.into(), kind: "fix".into(), lat, lon, elevation_ft: Some(0.0) }
    }

    fn at(lat: f64, alt: f64, vs: f64, t: f64) -> AircraftState {
        AircraftState {
            lat,
            lon: 0.0,
            alt_msl_ft: alt,
            alt_agl_ft: alt,
            vs_fpm: vs,
            flight_time_s: t,
            fms_tod_nm: None,
        }
    }

    #[test]
    fn waypoint_distance() {
        let s = only(|s| {
            s.wpt_enabled = true;
            s.wpt_within_nm = 10.0;
        });
        let mut m = Monitor::new(&s, Some(wpt("ABC", 1.0, 0.0)), None);
        assert!(m.update(&at(0.0, 30000.0, 0.0, 0.0)).is_none()); // 60 nm out
        assert!(m.update(&at(0.8, 30000.0, 0.0, 1.0)).is_none()); // 12 nm out
        let f = m.update(&at(0.85, 30000.0, 0.0, 2.0)).unwrap(); // 9 nm out
        assert!(f.reason.contains("ABC"), "{}", f.reason);
    }

    #[test]
    fn altitude_descending_only() {
        let s = only(|s| {
            s.alt_enabled = true;
            s.alt_ft = 10000.0;
            s.alt_dir = Direction::Descending;
        });
        let mut m = Monitor::new(&s, None, None);
        // Climbing through does not count.
        assert!(m.update(&at(0.0, 9900.0, 1500.0, 0.0)).is_none());
        assert!(m.update(&at(0.0, 10100.0, 1500.0, 1.0)).is_none());
        // Descending through does.
        assert!(m.update(&at(0.0, 10050.0, -1500.0, 2.0)).is_none());
        assert!(m.update(&at(0.0, 9990.0, -1500.0, 3.0)).is_some());
    }

    #[test]
    fn altitude_ignores_teleport() {
        let s = only(|s| {
            s.alt_enabled = true;
            s.alt_dir = Direction::Either;
        });
        let mut m = Monitor::new(&s, None, None);
        assert!(m.update(&at(0.0, 35000.0, 0.0, 0.0)).is_none());
        assert!(m.update(&at(0.0, 500.0, 0.0, 1.0)).is_none());
    }

    #[test]
    fn armed_already_past_altitude_waits_for_a_crossing() {
        let s = only(|s| {
            s.alt_enabled = true;
            s.alt_dir = Direction::Descending;
        });
        let mut m = Monitor::new(&s, None, None);
        for t in 0..10 {
            assert!(m.update(&at(0.0, 5000.0, -1000.0, t as f64)).is_none());
        }
    }

    #[test]
    fn tod_geometric() {
        let s = only(|s| {
            s.tod_enabled = true;
            s.tod_lead_nm = 5.0;
            s.tod_cross_ft = Some(0.0);
        });
        // FL300 to sea level at 3 nm/1000 ft = 90 nm descent; pause at 95 nm to go.
        let dest = wpt("DEST", 2.0, 0.0);
        let mut m = Monitor::new(&s, None, Some(dest));
        let lat_for = |nm_to_go: f64| 2.0 - nm_to_go / 60.04;
        assert!(m.update(&at(lat_for(100.0), 30000.0, 0.0, 0.0)).is_none());
        assert!(m.update(&at(lat_for(96.0), 30000.0, 0.0, 1.0)).is_none());
        assert!(m.update(&at(lat_for(94.0), 30000.0, 0.0, 2.0)).is_some());
    }

    #[test]
    fn tod_geometric_uses_field_elevation() {
        let s = only(|s| {
            s.tod_enabled = true;
            s.tod_lead_nm = 0.0;
        });
        // Denver-like field at 5,400 ft: cross at 6,900 ft, so FL300 needs
        // (30,000 - 6,900) / 1000 * 3 = 69.3 nm instead of 85.5 nm at sea level.
        let dest = Waypoint { elevation_ft: Some(5400.0), ..wpt("KDEN", 2.0, 0.0) };
        let mut m = Monitor::new(&s, None, Some(dest));
        let lat_for = |nm_to_go: f64| 2.0 - nm_to_go / 60.04;
        assert!(m.update(&at(lat_for(80.0), 30000.0, 0.0, 0.0)).is_none());
        assert!(m.update(&at(lat_for(70.0), 30000.0, 0.0, 1.0)).is_none());
        assert!(m.update(&at(lat_for(69.0), 30000.0, 0.0, 2.0)).is_some());
    }

    #[test]
    fn tod_fms_value() {
        let s = only(|s| {
            s.tod_enabled = true;
            s.tod_lead_nm = 2.0;
        });
        let mut m = Monitor::new(&s, None, None);
        let mut st = at(0.0, 30000.0, 0.0, 0.0);
        st.fms_tod_nm = Some(0.0); // FMS not computing yet: ignored
        assert!(m.update(&st).is_none());
        st.fms_tod_nm = Some(10.0);
        assert!(m.update(&st).is_none());
        st.fms_tod_nm = Some(1.5);
        assert!(m.update(&st).unwrap().reason.contains("FMS"));
    }

    #[test]
    fn tod_descent_onset_requires_cruise() {
        let s = only(|s| s.tod_enabled = true);
        let mut m = Monitor::new(&s, None, None);
        // Climb, then a short dip: no cruise yet, no pause.
        for t in 0..30 {
            assert!(m.update(&at(0.0, 20000.0, 2000.0, t as f64)).is_none());
        }
        for t in 30..45 {
            assert!(m.update(&at(0.0, 20000.0, -800.0, t as f64)).is_none());
        }
        // Level for > 60 s, then a sustained descent.
        for t in 45..120 {
            assert!(m.update(&at(0.0, 35000.0, 0.0, t as f64)).is_none());
        }
        let mut fired = None;
        for t in 120..140 {
            if let Some(f) = m.update(&at(0.0, 35000.0, -1500.0, t as f64)) {
                fired = Some(t);
                assert!(f.reason.contains("Descent"));
                break;
            }
        }
        assert_eq!(fired, Some(130));
    }

    #[test]
    fn timer_uses_flight_time() {
        let s = only(|s| {
            s.timer_enabled = true;
            s.timer_min = 1.0;
        });
        let mut m = Monitor::new(&s, None, None);
        assert!(m.update(&at(0.0, 0.0, 0.0, 100.0)).is_none());
        assert!(m.update(&at(0.0, 0.0, 0.0, 159.0)).is_none());
        assert!(m.update(&at(0.0, 0.0, 0.0, 160.0)).is_some());
    }

    #[test]
    fn progress_tracks_from_arming() {
        let s = only(|s| {
            s.wpt_enabled = true;
            s.wpt_within_nm = 0.0;
            s.timer_enabled = true;
            s.timer_min = 10.0;
        });
        let mut m = Monitor::new(&s, Some(wpt("ABC", 1.0, 0.0)), None);
        let start = at(0.0, 30000.0, 0.0, 0.0);
        assert!(m.status(&start).iter().all(|c| c.progress.is_none()), "nothing sampled yet");
        m.update(&start);
        let half = at(0.5, 30000.0, 0.0, 300.0);
        let st = m.status(&half);
        assert_eq!(st[0].kind, ConditionKind::Waypoint);
        assert!((st[0].progress.unwrap() - 0.5).abs() < 0.01);
        assert_eq!(st[1].kind, ConditionKind::Timer);
        assert!((st[1].progress.unwrap() - 0.5).abs() < 0.01);
    }

    #[test]
    fn formatting() {
        assert_eq!(thousands(10000.0), "10,000");
        assert_eq!(thousands(999.4), "999");
        assert_eq!(thousands(-1234.0), "-1,234");
    }
}
