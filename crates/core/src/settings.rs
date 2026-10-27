//! User settings and their on-disk `key = value` format.

use std::fmt::Write as _;

/// Which way the aircraft must be moving through the altitude for it to count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Either,
    Climbing,
    Descending,
}

impl Direction {
    pub const ALL: [Direction; 3] = [Direction::Descending, Direction::Climbing, Direction::Either];

    pub fn label(self) -> &'static str {
        match self {
            Direction::Either => "either way",
            Direction::Climbing => "climbing",
            Direction::Descending => "descending",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Direction::Either => "either",
            Direction::Climbing => "climbing",
            Direction::Descending => "descending",
        }
    }

    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|d| *d == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|d| d.key() == s)
    }
}

/// Altitude reference used by the altitude trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AltRef {
    /// Pilot-side indicated altitude, i.e. what the altimeter shows.
    Msl,
    /// Height above ground level.
    Agl,
}

impl AltRef {
    pub fn label(self) -> &'static str {
        match self {
            AltRef::Msl => "MSL",
            AltRef::Agl => "AGL",
        }
    }

    pub fn next(self) -> Self {
        match self {
            AltRef::Msl => AltRef::Agl,
            AltRef::Agl => AltRef::Msl,
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "msl" => Some(AltRef::Msl),
            "agl" => Some(AltRef::Agl),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    /// Pause when within `wpt_within_nm` of the waypoint `wpt_id`.
    pub wpt_enabled: bool,
    pub wpt_id: String,
    pub wpt_within_nm: f64,

    /// Pause when passing `alt_ft` in direction `alt_dir`.
    pub alt_enabled: bool,
    pub alt_ft: f64,
    pub alt_dir: Direction,
    pub alt_ref: AltRef,

    /// Pause `tod_lead_nm` before the computed top of descent.
    pub tod_enabled: bool,
    /// Where the descent should end. Empty means the FMS destination.
    pub tod_point: String,
    /// Altitude to be at over `tod_point`. `None` means the point's elevation (0 if unknown) + 1500 ft.
    pub tod_cross_ft: Option<f64>,
    pub tod_lead_nm: f64,

    /// Pause after this many minutes of (unpaused) sim time.
    pub timer_enabled: bool,
    pub timer_min: f64,

    // Advanced: only editable in the preferences file.
    /// Descent planning ratio, the classic "3 nm per 1000 ft" rule.
    pub tod_nm_per_kft: f64,
    /// Optional sim/aircraft dataref that publishes the distance to ToD in nm.
    pub tod_dataref: String,
    /// Vertical speed (negative, fpm) treated as "descending" by the descent-onset fallback.
    pub descent_vs_fpm: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            wpt_enabled: false,
            wpt_id: String::new(),
            wpt_within_nm: 20.0,
            alt_enabled: false,
            alt_ft: 10_000.0,
            alt_dir: Direction::Descending,
            alt_ref: AltRef::Msl,
            tod_enabled: true,
            tod_point: String::new(),
            tod_cross_ft: None,
            tod_lead_nm: 5.0,
            timer_enabled: false,
            timer_min: 60.0,
            tod_nm_per_kft: 3.0,
            tod_dataref: "sim/cockpit2/radios/indicators/fms_distance_to_tod_pilot".into(),
            descent_vs_fpm: -500.0,
        }
    }
}

impl Settings {
    pub fn any_enabled(&self) -> bool {
        self.wpt_enabled || self.alt_enabled || self.tod_enabled || self.timer_enabled
    }

    /// Checks the values that only make sense in a range. Returns a user-facing message.
    pub fn validate(&self) -> Result<(), String> {
        if !self.any_enabled() {
            return Err("Enable at least one condition".into());
        }
        if self.wpt_enabled {
            if self.wpt_id.trim().is_empty() {
                return Err("Enter a waypoint identifier".into());
            }
            if self.wpt_within_nm.is_nan() || self.wpt_within_nm < 0.0 {
                return Err("Waypoint distance must be >= 0 nm".into());
            }
        }
        if self.alt_enabled && !(-2000.0..=60_000.0).contains(&self.alt_ft) {
            return Err("Altitude must be between -2000 and 60000 ft".into());
        }
        if self.tod_enabled && (self.tod_lead_nm.is_nan() || self.tod_lead_nm < 0.0) {
            return Err("ToD lead must be >= 0 nm".into());
        }
        if self.timer_enabled && (self.timer_min.is_nan() || self.timer_min <= 0.0) {
            return Err("Timer must be > 0 minutes".into());
        }
        Ok(())
    }

    pub fn serialize(&self) -> String {
        let mut s = String::from("# PauseAtToD preferences\n");
        let mut kv = |k: &str, v: &dyn std::fmt::Display| {
            let _ = writeln!(s, "{k} = {v}");
        };
        kv("wpt_enabled", &self.wpt_enabled);
        kv("wpt_id", &self.wpt_id);
        kv("wpt_within_nm", &self.wpt_within_nm);
        kv("alt_enabled", &self.alt_enabled);
        kv("alt_ft", &self.alt_ft);
        kv("alt_dir", &self.alt_dir.key());
        kv("alt_ref", &self.alt_ref.label().to_ascii_lowercase());
        kv("tod_enabled", &self.tod_enabled);
        kv("tod_point", &self.tod_point);
        kv("tod_cross_ft", &self.tod_cross_ft.map(|v| v.to_string()).unwrap_or_default());
        kv("tod_lead_nm", &self.tod_lead_nm);
        kv("timer_enabled", &self.timer_enabled);
        kv("timer_min", &self.timer_min);
        kv("tod_nm_per_kft", &self.tod_nm_per_kft);
        kv("tod_dataref", &self.tod_dataref);
        kv("descent_vs_fpm", &self.descent_vs_fpm);
        s
    }

    /// Lenient parser: unknown keys and malformed values are ignored and keep their defaults.
    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            let b = |out: &mut bool| {
                if let Ok(x) = v.parse() {
                    *out = x
                }
            };
            let f = |out: &mut f64| {
                if let Some(x) = parse_number(v) {
                    *out = x
                }
            };
            match k.trim() {
                "wpt_enabled" => b(&mut s.wpt_enabled),
                "wpt_id" => s.wpt_id = v.to_ascii_uppercase(),
                "wpt_within_nm" => f(&mut s.wpt_within_nm),
                "alt_enabled" => b(&mut s.alt_enabled),
                "alt_ft" => f(&mut s.alt_ft),
                "alt_dir" => s.alt_dir = Direction::parse(v).unwrap_or(s.alt_dir),
                "alt_ref" => s.alt_ref = AltRef::parse(v).unwrap_or(s.alt_ref),
                "tod_enabled" => b(&mut s.tod_enabled),
                "tod_point" => s.tod_point = v.to_ascii_uppercase(),
                "tod_cross_ft" => s.tod_cross_ft = parse_number(v),
                "tod_lead_nm" => f(&mut s.tod_lead_nm),
                "timer_enabled" => b(&mut s.timer_enabled),
                "timer_min" => f(&mut s.timer_min),
                "tod_nm_per_kft" => f(&mut s.tod_nm_per_kft),
                "tod_dataref" => s.tod_dataref = v.to_string(),
                "descent_vs_fpm" => f(&mut s.descent_vs_fpm),
                _ => {}
            }
        }
        s
    }
}

/// Parses user-typed numbers, tolerating thousands separators and flight levels ("FL350").
pub fn parse_number(text: &str) -> Option<f64> {
    let t: String = text.trim().chars().filter(|c| *c != ',' && *c != '_' && *c != ' ').collect();
    if t.is_empty() {
        return None;
    }
    if let Some(fl) = t.strip_prefix("FL").or_else(|| t.strip_prefix("fl")) {
        return fl.parse::<f64>().ok().filter(|v| v.is_finite()).map(|v| v * 100.0);
    }
    t.parse::<f64>().ok().filter(|v| v.is_finite())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut s = Settings::default();
        s.wpt_enabled = true;
        s.wpt_id = "KSEA".into();
        s.alt_dir = Direction::Climbing;
        s.alt_ref = AltRef::Agl;
        s.tod_cross_ft = Some(3000.0);
        s.tod_point = "ELN".into();
        assert_eq!(Settings::parse(&s.serialize()), s);
    }

    #[test]
    fn garbage_keeps_defaults() {
        let s = Settings::parse("alt_ft = banana\nnope\nwpt_within_nm=12.5\n");
        assert_eq!(s.alt_ft, Settings::default().alt_ft);
        assert_eq!(s.wpt_within_nm, 12.5);
    }

    #[test]
    fn numbers() {
        assert_eq!(parse_number("10,000"), Some(10000.0));
        assert_eq!(parse_number("FL350"), Some(35000.0));
        assert_eq!(parse_number(""), None);
        assert_eq!(parse_number("nan"), None);
    }

    #[test]
    fn validation() {
        let mut s = Settings { tod_enabled: false, ..Settings::default() };
        assert!(s.validate().is_err());
        s.wpt_enabled = true;
        assert!(s.validate().is_err(), "empty waypoint");
        s.wpt_id = "BOS".into();
        assert!(s.validate().is_ok());
    }
}
