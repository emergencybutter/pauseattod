//! Thin safe wrappers over [`crate::ffi`]. Everything here must be called from
//! X-Plane's main thread, which is the only thread plugins are called on.

use std::ffi::{c_char, c_int, CString};
use std::path::PathBuf;
use std::ptr;

use pauseattod_core::geo::distance_nm;
use pauseattod_core::Waypoint;

use crate::ffi::*;

pub fn cstring(s: &str) -> CString {
    CString::new(s.replace('\0', "")).unwrap_or_default()
}

pub fn log(msg: &str) {
    let line = cstring(&format!("PauseAtToD: {msg}\n"));
    unsafe { XPLMDebugString(line.as_ptr()) }
}

pub fn enable_feature(name: &str) {
    let s = cstring(name);
    unsafe { XPLMEnableFeature(s.as_ptr(), 1) }
}

/// The X-Plane root folder (requires `XPLM_USE_NATIVE_PATHS` on macOS).
pub fn system_path() -> PathBuf {
    let mut buf = [0 as c_char; 1024];
    unsafe { XPLMGetSystemPath(buf.as_mut_ptr()) };
    PathBuf::from(buf_to_string(&buf))
}

pub fn buf_to_string(buf: &[c_char]) -> String {
    // SAFETY: c_char and u8 have the same layout.
    let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), buf.len()) };
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

#[derive(Clone, Copy)]
pub struct DataRef(XPLMDataRef);

impl DataRef {
    pub fn find(name: &str) -> Option<Self> {
        let s = cstring(name);
        let r = unsafe { XPLMFindDataRef(s.as_ptr()) };
        (!r.is_null()).then_some(Self(r))
    }

    pub fn f32(self) -> f32 {
        unsafe { XPLMGetDataf(self.0) }
    }

    pub fn f64(self) -> f64 {
        unsafe { XPLMGetDatad(self.0) }
    }

    pub fn i32(self) -> i32 {
        unsafe { XPLMGetDatai(self.0) }
    }
}

#[derive(Clone, Copy)]
pub struct Command(XPLMCommandRef);

impl Command {
    pub fn find(name: &str) -> Option<Self> {
        let s = cstring(name);
        let r = unsafe { XPLMFindCommand(s.as_ptr()) };
        (!r.is_null()).then_some(Self(r))
    }

    pub fn once(self) {
        unsafe { XPLMCommandOnce(self.0) }
    }
}

/// A command this plugin owns, routed to `handler` with `refcon` on press.
pub struct OwnedCommand {
    cmd: XPLMCommandRef,
    handler: XPLMCommandCallback_f,
    refcon: usize,
}

impl OwnedCommand {
    pub fn new(name: &str, desc: &str, handler: XPLMCommandCallback_f, refcon: usize) -> Self {
        let (n, d) = (cstring(name), cstring(desc));
        unsafe {
            let cmd = XPLMCreateCommand(n.as_ptr(), d.as_ptr());
            XPLMRegisterCommandHandler(cmd, handler, 1, refcon as *mut _);
            Self { cmd, handler, refcon }
        }
    }
}

impl Drop for OwnedCommand {
    fn drop(&mut self) {
        unsafe { XPLMUnregisterCommandHandler(self.cmd, self.handler, 1, self.refcon as *mut _) }
    }
}

/// Our submenu under Plugins.
pub struct Menu {
    id: XPLMMenuID,
    parent_item: c_int,
}

impl Menu {
    pub fn new(title: &str, items: &[(&str, usize)], handler: XPLMMenuHandler_f) -> Option<Self> {
        let t = cstring(title);
        unsafe {
            let plugins = XPLMFindPluginsMenu();
            let parent_item = XPLMAppendMenuItem(plugins, t.as_ptr(), ptr::null_mut(), 0);
            if parent_item < 0 {
                return None;
            }
            let id = XPLMCreateMenu(t.as_ptr(), plugins, parent_item, Some(handler), ptr::null_mut());
            if id.is_null() {
                return None;
            }
            for (name, item_ref) in items {
                let n = cstring(name);
                XPLMAppendMenuItem(id, n.as_ptr(), *item_ref as *mut _, 0);
            }
            Some(Self { id, parent_item })
        }
    }

    pub fn set_checked(&self, index: i32, checked: bool) {
        let state = if checked { xplm_Menu_Checked } else { xplm_Menu_Unchecked };
        unsafe { XPLMCheckMenuItem(self.id, index, state) }
    }
}

impl Drop for Menu {
    fn drop(&mut self) {
        unsafe {
            XPLMDestroyMenu(self.id);
            XPLMRemoveMenuItem(XPLMFindPluginsMenu(), self.parent_item);
        }
    }
}

pub fn screen_bounds() -> (i32, i32, i32, i32) {
    let (mut l, mut t, mut r, mut b) = (0, 0, 0, 0);
    unsafe { XPLMGetScreenBoundsGlobal(&mut l, &mut t, &mut r, &mut b) };
    (l, t, r, b)
}

// ---------------------------------------------------------------------------
// Navigation lookups

fn kind_name(kind: XPLMNavType) -> &'static str {
    match kind {
        xplm_Nav_Airport => "airport",
        xplm_Nav_NDB => "NDB",
        xplm_Nav_VOR => "VOR",
        xplm_Nav_Fix => "fix",
        xplm_Nav_DME => "DME",
        xplm_Nav_LatLon => "lat/lon",
        _ => "navaid",
    }
}

struct NavInfo {
    kind: XPLMNavType,
    id: String,
    lat: f64,
    lon: f64,
    /// Raw `outHeight`. The SDK docs say feet, but X-Plane actually returns meters.
    height: f32,
}

fn nav_info(r: XPLMNavRef) -> NavInfo {
    let (mut kind, mut lat, mut lon, mut height) = (0, 0.0f32, 0.0f32, 0.0f32);
    let mut id = [0 as c_char; 32];
    unsafe {
        XPLMGetNavAidInfo(
            r,
            &mut kind,
            &mut lat,
            &mut lon,
            &mut height,
            ptr::null_mut(),
            ptr::null_mut(),
            id.as_mut_ptr(),
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    NavInfo { kind, id: buf_to_string(&id), lat: lat as f64, lon: lon as f64, height }
}

/// Field elevation of an airport nav entry. Other kinds have no useful elevation.
fn airport_elevation_ft(n: &NavInfo) -> Option<f64> {
    const FT_PER_M: f64 = 3.28084;
    if n.kind != xplm_Nav_Airport {
        return None;
    }
    let ft = n.height as f64 * FT_PER_M;
    // Real airports sit between the Dead Sea (-1,400 ft) and Daocheng Yading (14,472 ft).
    let plausible = (-1500.0..=15000.0).contains(&ft);
    log(&format!(
        "{} field elevation {:.0} ft (nav height {:.1} m){}",
        n.id,
        ft,
        n.height,
        if plausible { "" } else { ", implausible, ignored" }
    ));
    plausible.then_some(ft)
}

fn waypoint_from_nav(n: NavInfo) -> Waypoint {
    let elevation_ft = airport_elevation_ft(&n);
    Waypoint { kind: kind_name(n.kind).into(), id: n.id, lat: n.lat, lon: n.lon, elevation_ft }
}

struct FmsEntry {
    kind: XPLMNavType,
    id: String,
    nav: XPLMNavRef,
    lat: f64,
    lon: f64,
}

impl FmsEntry {
    /// Elevation for airport legs. The entry's nav reference is filled asynchronously
    /// after flight plan edits, so fall back to an exact ID search near the entry.
    fn elevation_ft(&self) -> Option<f64> {
        if self.kind != xplm_Nav_Airport {
            return None;
        }
        let nav = if self.nav != XPLM_NAV_NOT_FOUND {
            self.nav
        } else {
            let id = cstring(&self.id);
            let (mut lat, mut lon) = (self.lat as f32, self.lon as f32);
            unsafe { XPLMFindNavAid(ptr::null(), id.as_ptr(), &mut lat, &mut lon, ptr::null_mut(), xplm_Nav_Airport) }
        };
        if nav == XPLM_NAV_NOT_FOUND {
            return None;
        }
        let n = nav_info(nav);
        n.id.eq_ignore_ascii_case(&self.id).then(|| airport_elevation_ft(&n)).flatten()
    }
}

fn fms_entry(index: c_int) -> FmsEntry {
    let (mut kind, mut nav, mut alt, mut lat, mut lon) = (0, XPLM_NAV_NOT_FOUND, 0, 0.0f32, 0.0f32);
    let mut id = [0 as c_char; 256];
    unsafe { XPLMGetFMSEntryInfo(index, &mut kind, id.as_mut_ptr(), &mut nav, &mut alt, &mut lat, &mut lon) };
    FmsEntry { kind, id: buf_to_string(&id), nav, lat: lat as f64, lon: lon as f64 }
}

/// The destination entry of X-Plane's built-in FMS, if a flight plan is loaded.
pub fn fms_destination() -> Option<Waypoint> {
    let count = unsafe { XPLMCountFMSEntries() };
    let dest = unsafe { XPLMGetDestinationFMSEntry() };
    if count <= 0 || dest < 0 || dest >= count {
        return None;
    }
    let e = fms_entry(dest);
    if e.lat == 0.0 && e.lon == 0.0 {
        return None;
    }
    let elevation_ft = e.elevation_ft();
    let id = if e.id.is_empty() { format!("FMS#{}", dest + 1) } else { e.id };
    Some(Waypoint { id, kind: format!("FMS dest, {}", kind_name(e.kind)), lat: e.lat, lon: e.lon, elevation_ft })
}

/// Resolves an identifier to a position. The loaded FMS flight plan wins (it is
/// what the pilot means by "that waypoint"); otherwise the nav database entry
/// with exactly that ID closest to the aircraft is used.
pub fn find_waypoint(ident: &str, near_lat: f64, near_lon: f64) -> Option<Waypoint> {
    let ident = ident.trim().to_ascii_uppercase();
    if ident.is_empty() {
        return None;
    }

    let count = unsafe { XPLMCountFMSEntries() };
    for i in 0..count.max(0) {
        let e = fms_entry(i);
        if e.id.eq_ignore_ascii_case(&ident) {
            return Some(Waypoint {
                id: ident,
                kind: format!("FMS leg {}, {}", i + 1, kind_name(e.kind)),
                lat: e.lat,
                lon: e.lon,
                elevation_ft: e.elevation_ft(),
            });
        }
    }

    const TYPES: [XPLMNavType; 4] = [xplm_Nav_Airport, xplm_Nav_VOR, xplm_Nav_NDB, xplm_Nav_Fix];
    let all = TYPES.iter().fold(0, |a, t| a | t);

    // Fast path: the SDK's own nearest search. It matches ID *fragments*, so verify.
    let id_c = cstring(&ident);
    let (mut lat, mut lon) = (near_lat as f32, near_lon as f32);
    let r = unsafe { XPLMFindNavAid(ptr::null(), id_c.as_ptr(), &mut lat, &mut lon, ptr::null_mut(), all) };
    if r != XPLM_NAV_NOT_FOUND {
        let n = nav_info(r);
        if n.id.eq_ignore_ascii_case(&ident) {
            return Some(waypoint_from_nav(n));
        }
    }

    // Slow path: exact scan of each type's contiguous block (one-off, on Arm).
    let mut best: Option<(f64, NavInfo)> = None;
    for t in TYPES {
        let (first, last) = unsafe { (XPLMFindFirstNavAidOfType(t), XPLMFindLastNavAidOfType(t)) };
        if first == XPLM_NAV_NOT_FOUND || last == XPLM_NAV_NOT_FOUND {
            continue;
        }
        for r in first..=last {
            let n = nav_info(r);
            if n.id.eq_ignore_ascii_case(&ident) {
                let d = distance_nm(near_lat, near_lon, n.lat, n.lon);
                if best.as_ref().is_none_or(|(bd, _)| d < *bd) {
                    best = Some((d, n));
                }
            }
        }
    }
    best.map(|(_, n)| waypoint_from_nav(n))
}
