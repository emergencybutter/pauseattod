//! Plugin state machine: owns settings, the armed monitor, the menu and the window.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use pauseattod_core::settings::parse_number;
use pauseattod_core::trigger::thousands;
use pauseattod_core::{AircraftState, Monitor, Settings, Waypoint};

use crate::gui::EguiWindow;
use crate::view::{self, Action, Preview, Tone, ViewModel};
use crate::xp::{self, Command, DataRef, Menu};

/// Things that happen to the app outside its own window. Callbacks only ever post these.
#[derive(Debug, Clone, Copy)]
pub enum Event {
    ToggleWindow,
    ToggleArm,
    /// A new flight / aircraft was loaded: history is meaningless now.
    NewFlight,
}

pub const MENU_SETTINGS: usize = 1;
pub const MENU_ARM: usize = 2;
const MENU_ARM_INDEX: i32 = 1;

const FT_PER_M: f64 = 3.28084;
/// Wait this long after the last keystroke before searching the nav database.
const LOOKUP_DEBOUNCE: Duration = Duration::from_millis(500);
/// The FMS destination can change at any time; re-read it this often.
const FMS_REFRESH: Duration = Duration::from_secs(2);
/// Default end-of-descent height above the field (mirrors the core's default).
const CROSS_ABOVE_FIELD_FT: f64 = 1500.0;

struct Refs {
    lat: Option<DataRef>,
    lon: Option<DataRef>,
    alt_ind_ft: Option<DataRef>,
    agl_m: Option<DataRef>,
    vs_fpm: Option<DataRef>,
    flight_time: Option<DataRef>,
    paused: Option<DataRef>,
}

impl Refs {
    fn find() -> Self {
        Self {
            lat: DataRef::find("sim/flightmodel/position/latitude"),
            lon: DataRef::find("sim/flightmodel/position/longitude"),
            alt_ind_ft: DataRef::find("sim/cockpit2/gauges/indicators/altitude_ft_pilot"),
            agl_m: DataRef::find("sim/flightmodel/position/y_agl"),
            vs_fpm: DataRef::find("sim/flightmodel/position/vh_ind_fpm"),
            flight_time: DataRef::find("sim/time/total_flight_time_sec"),
            paused: DataRef::find("sim/time/paused"),
        }
    }
}

struct Armed {
    monitor: Monitor,
    /// Looked up at arm time: aircraft plugins may publish it late.
    tod_ref: Option<DataRef>,
}

/// A debounced navaid lookup for the live preview under an ident field.
#[derive(Default)]
struct Lookup {
    query: String,
    changed: Option<Instant>,
    result: Option<Option<Waypoint>>,
}

impl Lookup {
    /// Re-resolves `query` once it has been stable for a moment.
    fn poll(&mut self, query: &str, resolve: impl FnOnce() -> Option<Waypoint>) {
        if self.query != query {
            self.query = query.to_string();
            self.changed = Some(Instant::now());
            self.result = None;
        }
        if self.result.is_none() && self.changed.is_none_or(|t| t.elapsed() >= LOOKUP_DEBOUNCE) {
            self.result = Some(resolve());
        }
    }
}

pub struct App {
    settings: Settings,
    cross_text: String,
    prefs_path: PathBuf,
    refs: Refs,
    armed: Option<Armed>,
    message: String,
    window: EguiWindow,
    menu: Option<Menu>,
    wpt_lookup: Lookup,
    tod_lookup: Lookup,
    fms_arrival: Option<Waypoint>,
    fms_checked: Option<Instant>,
    /// What we last logged about the flight plan, to log only changes.
    fms_logged: Option<String>,
}

impl App {
    pub fn new(window: EguiWindow, menu: Option<Menu>) -> Self {
        let prefs_path = xp::system_path().join("Output").join("preferences").join("pauseattod.prf");
        let settings = match std::fs::read_to_string(&prefs_path) {
            Ok(text) => Settings::parse(&text),
            Err(_) => Settings::default(),
        };
        view::install_style(window.context());
        let app = Self {
            cross_text: settings.tod_cross_ft.map(|v| format!("{v:.0}")).unwrap_or_default(),
            settings,
            prefs_path,
            refs: Refs::find(),
            armed: None,
            message: "Pick the conditions, then press Arm.".into(),
            window,
            menu,
            wpt_lookup: Lookup::default(),
            tod_lookup: Lookup::default(),
            fms_arrival: None,
            fms_checked: None,
            fms_logged: None,
        };
        app.sync_armed_indicators();
        app
    }

    pub fn window(&mut self) -> &mut EguiWindow {
        &mut self.window
    }

    pub fn handle(&mut self, ev: Event) {
        match ev {
            Event::ToggleWindow => {
                let show = !self.window.is_visible();
                self.window.set_visible(show);
                if !show {
                    self.save();
                }
            }
            Event::ToggleArm => {
                if self.armed.is_some() {
                    self.disarm("Disarmed.");
                } else {
                    self.arm();
                }
            }
            Event::NewFlight => {
                if self.armed.is_some() {
                    self.disarm("Disarmed: a new flight was loaded.");
                }
            }
        }
    }

    fn arm(&mut self) {
        if let Err(e) = self.apply_cross_text().and_then(|_| self.settings.validate()) {
            self.message = format!("Cannot arm: {e}");
            return;
        }
        let s = self.sample(None);

        let wpt = if self.settings.wpt_enabled {
            match xp::find_waypoint(&self.settings.wpt_id, s.lat, s.lon) {
                Some(w) => Some(w),
                None => {
                    self.message = format!("Cannot arm: waypoint {} not found", self.settings.wpt_id);
                    return;
                }
            }
        } else {
            None
        };

        let tod_point = if !self.settings.tod_enabled {
            None
        } else if self.settings.tod_point.is_empty() {
            self.read_fms_arrival()
        } else {
            match xp::find_waypoint(&self.settings.tod_point, s.lat, s.lon) {
                Some(w) => Some(w),
                None => {
                    self.message = format!("Cannot arm: ToD point {} not found", self.settings.tod_point);
                    return;
                }
            }
        };

        let tod_ref = if self.settings.tod_enabled && !self.settings.tod_dataref.is_empty() {
            DataRef::find(&self.settings.tod_dataref)
        } else {
            None
        };

        let monitor = Monitor::new(&self.settings, wpt, tod_point);
        self.armed = Some(Armed { monitor, tod_ref });
        xp::log("armed");
        self.save();
        self.sync_armed_indicators();
    }

    /// Copies the free-text crossing altitude into the settings.
    fn apply_cross_text(&mut self) -> Result<(), String> {
        let t = self.cross_text.trim();
        if t.is_empty() {
            self.settings.tod_cross_ft = None;
            return Ok(());
        }
        match parse_number(t) {
            Some(v) => {
                self.settings.tod_cross_ft = Some(v);
                Ok(())
            }
            None => Err("ToD crossing altitude must be a number, or blank for automatic".into()),
        }
    }

    fn disarm(&mut self, message: &str) {
        self.armed = None;
        self.message = message.into();
        self.sync_armed_indicators();
    }

    fn sync_armed_indicators(&self) {
        if let Some(menu) = &self.menu {
            menu.set_checked(MENU_ARM_INDEX, self.armed.is_some());
        }
    }

    fn is_paused(&self) -> bool {
        self.refs.paused.is_some_and(|r| r.i32() != 0)
    }

    fn sample(&self, tod_ref: Option<DataRef>) -> AircraftState {
        let r = &self.refs;
        let f = |d: Option<DataRef>| d.map_or(0.0, |d| d.f32() as f64);
        AircraftState {
            lat: r.lat.map_or(0.0, DataRef::f64),
            lon: r.lon.map_or(0.0, DataRef::f64),
            alt_msl_ft: f(r.alt_ind_ft),
            alt_agl_ft: f(r.agl_m) * FT_PER_M,
            vs_fpm: f(r.vs_fpm),
            flight_time_s: f(r.flight_time),
            fms_tod_nm: tod_ref.map(|d| d.f32() as f64),
        }
    }

    /// Called from the flight loop. Returns the delay until the next call.
    pub fn tick(&mut self) -> f32 {
        if !self.is_paused() {
            if let Some(tod_ref) = self.armed.as_ref().map(|a| a.tod_ref) {
                let s = self.sample(tod_ref);
                if let Some(fired) = self.armed.as_mut().and_then(|a| a.monitor.update(&s)) {
                    self.fire(&fired.reason);
                }
            }
        }
        0.1
    }

    fn fire(&mut self, reason: &str) {
        xp::log(&format!("pausing: {reason}"));
        match Command::find("sim/operation/pause_on") {
            Some(c) => c.once(),
            None => {
                if !self.is_paused() {
                    if let Some(c) = Command::find("sim/operation/pause_toggle") {
                        c.once();
                    }
                }
            }
        }
        self.disarm(&format!("PAUSED: {reason}"));
        self.window.set_visible(true);
    }

    /// Draw callback of our window: refreshes previews, runs the UI, applies its actions.
    pub fn draw(&mut self) {
        let here = self.sample(None);
        if self.armed.is_none() {
            self.refresh_previews(&here);
        }
        let statuses = self.armed.as_ref().map(|a| a.monitor.status(&self.sample(a.tod_ref))).unwrap_or_default();
        let wpt_preview = self.settings.wpt_enabled.then(|| preview_for(&self.wpt_lookup, &here, None)).flatten();
        let tod_preview = self.settings.tod_enabled.then(|| self.tod_preview(&here));
        let paused = self.is_paused();

        let mut actions = Vec::new();
        let mut vm = ViewModel {
            settings: &mut self.settings,
            cross_text: &mut self.cross_text,
            armed: self.armed.is_some(),
            paused,
            message: &self.message,
            statuses: &statuses,
            wpt_preview,
            tod_preview,
        };
        self.window.draw(|ui, popped_out| view::show(ui, &mut vm, popped_out, &mut actions));

        for a in actions {
            match a {
                Action::ToggleArm => self.handle(Event::ToggleArm),
            }
        }
    }

    fn refresh_previews(&mut self, here: &AircraftState) {
        let (lat, lon) = (here.lat, here.lon);
        if self.settings.wpt_enabled {
            let id = self.settings.wpt_id.trim().to_string();
            self.wpt_lookup.poll(&id, || if id.is_empty() { None } else { xp::find_waypoint(&id, lat, lon) });
        }
        if self.settings.tod_enabled {
            let id = self.settings.tod_point.trim().to_string();
            if id.is_empty() {
                if self.fms_checked.is_none_or(|t| t.elapsed() >= FMS_REFRESH) {
                    self.fms_arrival = self.read_fms_arrival();
                    self.fms_checked = Some(Instant::now());
                }
            } else {
                self.tod_lookup.poll(&id, || xp::find_waypoint(&id, lat, lon));
            }
        }
    }

    /// The arrival airport of X-Plane's FMS flight plan, logging what was found
    /// whenever it changes.
    fn read_fms_arrival(&mut self) -> Option<Waypoint> {
        let plan = xp::flight_plan();
        let arrival = plan.arrival();
        let note = match &arrival {
            Some(w) => format!("FMS arrival {} from the {} ({} entries)", w.id, plan.source, plan.entries.len()),
            None => format!("no FMS flight plan found ({}, {} entries)", plan.source, plan.entries.len()),
        };
        if self.fms_logged.as_deref() != Some(note.as_str()) {
            xp::log(&note);
            self.fms_logged = Some(note);
        }
        arrival
    }

    fn tod_preview(&self, here: &AircraftState) -> Preview {
        let point = if self.settings.tod_point.trim().is_empty() {
            match &self.fms_arrival {
                Some(w) => Some(w.clone()),
                None => {
                    return Preview {
                        text: "No flight plan in X-Plane's FMS. Aircraft with their own FMS (Zibo, ToLiss...)                                aren't visible to plugins: type the arrival airport. Until then, the descent                                from cruise is the only cue."
                            .into(),
                        tone: Tone::Muted,
                    }
                }
            }
        } else {
            match &self.tod_lookup.result {
                None => return Preview { text: "Looking up...".into(), tone: Tone::Muted },
                Some(None) => {
                    return Preview { text: format!("{} not found", self.settings.tod_point), tone: Tone::Bad }
                }
                Some(Some(w)) => Some(w.clone()),
            }
        };
        let w = point.expect("handled above");
        let cross = match parse_number(&self.cross_text) {
            Some(v) => format!("cross at {} ft", thousands(v)),
            None if !self.cross_text.trim().is_empty() => {
                return Preview { text: "Crossing altitude must be a number".into(), tone: Tone::Bad }
            }
            None => match w.elevation_ft {
                Some(e) => format!(
                    "cross at {} ft (field {} + {})",
                    thousands(e + CROSS_ABOVE_FIELD_FT),
                    thousands(e),
                    thousands(CROSS_ABOVE_FIELD_FT)
                ),
                None => format!("cross at {} ft", thousands(CROSS_ABOVE_FIELD_FT)),
            },
        };
        preview_for_waypoint(&w, here, Some(&cross))
    }

    pub fn save(&self) {
        if let Some(dir) = self.prefs_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&self.prefs_path, self.settings.serialize()) {
            xp::log(&format!("could not save {}: {e}", self.prefs_path.display()));
        }
    }

    /// Captures in-progress edits before the plugin is unloaded.
    pub fn shutdown(&mut self) {
        let _ = self.apply_cross_text();
        self.save();
    }
}

fn preview_for(lookup: &Lookup, here: &AircraftState, extra: Option<&str>) -> Option<Preview> {
    if lookup.query.is_empty() {
        return None;
    }
    Some(match &lookup.result {
        None => Preview { text: "Looking up...".into(), tone: Tone::Muted },
        Some(None) => Preview { text: format!("{} not found", lookup.query), tone: Tone::Bad },
        Some(Some(w)) => preview_for_waypoint(w, here, extra),
    })
}

fn preview_for_waypoint(w: &Waypoint, here: &AircraftState, extra: Option<&str>) -> Preview {
    let d = pauseattod_core::geo::distance_nm(here.lat, here.lon, w.lat, w.lon);
    let mut text = format!("{} · {} · {:.0} nm away", w.id, w.kind, d);
    if let Some(e) = extra {
        text.push_str(" · ");
        text.push_str(e);
    }
    Preview { text, tone: Tone::Good }
}
