//! PauseAtToD: pauses X-Plane at top of descent, near a waypoint, at an
//! altitude, or after a timer.
//!
//! This file holds the C entry points X-Plane calls. Every callback is wrapped
//! in [`guard`] so a Rust panic is logged instead of crashing the simulator,
//! and every callback funnels into [`App`] through [`post`] / [`with_app`],
//! which serialise access even when X-Plane re-enters us (e.g. moving keyboard
//! focus synchronously calls our key callback).

// Keep the SDK's own spelling for its constants, also when matched in patterns.
#![allow(non_upper_case_globals)]
// The `XPlugin*` entry points are only ever called by X-Plane.
#![allow(clippy::missing_safety_doc)]

mod app;
mod ffi;
mod gl;
mod gui;
mod view;
mod xp;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::{c_char, c_float, c_int, c_void};
use std::panic::{catch_unwind, AssertUnwindSafe};

use app::{App, Event, MENU_ARM, MENU_SETTINGS};
use ffi::*;

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static QUEUE: RefCell<VecDeque<Event>> = const { RefCell::new(VecDeque::new()) };
    static BUSY: Cell<bool> = const { Cell::new(false) };
    static COMMANDS: RefCell<Vec<xp::OwnedCommand>> = const { RefCell::new(Vec::new()) };
}

const CMD_TOGGLE_WINDOW: usize = 1;
const CMD_TOGGLE_ARM: usize = 2;

fn guard<R>(what: &str, fallback: R, f: impl FnOnce() -> R) -> R {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|e| {
        let msg = e
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| e.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        xp::log(&format!("internal error in {what}: {msg}"));
        fallback
    })
}

/// Clears the busy flag even if the work panicked.
struct BusyGuard;
impl Drop for BusyGuard {
    fn drop(&mut self) {
        BUSY.with(|b| b.set(false));
    }
}

/// Runs `f` on the app, then drains queued events. If we are already inside
/// the app (re-entrant call), returns `None` and leaves queued events for the
/// outer call to drain.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    if BUSY.with(|b| b.replace(true)) {
        return None;
    }
    let _busy = BusyGuard;
    let out = APP.with(|a| a.borrow_mut().as_mut().map(f));
    while let Some(ev) = QUEUE.with(|q| q.borrow_mut().pop_front()) {
        APP.with(|a| {
            if let Some(app) = a.borrow_mut().as_mut() {
                app.handle(ev)
            }
        });
    }
    out
}

fn post(ev: Event) {
    QUEUE.with(|q| q.borrow_mut().push_back(ev));
    with_app(|_| ());
}

fn copy_out(dst: *mut c_char, s: &str) {
    // X-Plane gives us 256-byte buffers.
    let bytes = s.as_bytes();
    let n = bytes.len().min(255);
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr().cast::<c_char>(), dst, n);
        *dst.add(n) = 0;
    }
}

// ---------------------------------------------------------------------------
// Callbacks

unsafe extern "C" fn flight_loop(_: c_float, _: c_float, _: c_int, _: *mut c_void) -> c_float {
    guard("flight loop", 1.0, || with_app(App::tick).unwrap_or(0.1))
}

unsafe extern "C" fn menu_handler(_: *mut c_void, item: *mut c_void) {
    guard("menu", (), || match item as usize {
        MENU_SETTINGS => post(Event::ToggleWindow),
        MENU_ARM => post(Event::ToggleArm),
        _ => {}
    })
}

unsafe extern "C" fn command_handler(_: XPLMCommandRef, phase: c_int, refcon: *mut c_void) -> c_int {
    guard("command", 1, || {
        if phase == xplm_CommandBegin {
            match refcon as usize {
                CMD_TOGGLE_WINDOW => post(Event::ToggleWindow),
                CMD_TOGGLE_ARM => post(Event::ToggleArm),
                _ => {}
            }
        }
        0
    })
}

unsafe extern "C" fn draw_window(_: XPLMWindowID, _: *mut c_void) {
    guard("draw", (), || {
        with_app(App::draw);
    })
}

unsafe extern "C" fn mouse_click(_: XPLMWindowID, x: c_int, y: c_int, status: c_int, _: *mut c_void) -> c_int {
    guard("mouse", 1, || {
        with_app(|a| a.window().on_mouse(x, y, status, egui::PointerButton::Primary));
        1
    })
}

unsafe extern "C" fn right_click(_: XPLMWindowID, x: c_int, y: c_int, status: c_int, _: *mut c_void) -> c_int {
    guard("mouse", 1, || {
        with_app(|a| a.window().on_mouse(x, y, status, egui::PointerButton::Secondary));
        1
    })
}

unsafe extern "C" fn key_press(_: XPLMWindowID, key: c_char, flags: c_int, vkey: c_char, _: *mut c_void, losing: c_int) {
    guard("key", (), || {
        with_app(|a| a.window().on_key(key, flags, vkey, losing != 0));
    })
}

unsafe extern "C" fn cursor(_: XPLMWindowID, _: c_int, _: c_int, _: *mut c_void) -> c_int {
    xplm_CursorDefault
}

unsafe extern "C" fn mouse_wheel(_: XPLMWindowID, x: c_int, y: c_int, wheel: c_int, clicks: c_int, _: *mut c_void) -> c_int {
    guard("wheel", 1, || {
        with_app(|a| a.window().on_wheel(x, y, wheel, clicks));
        1
    })
}

// ---------------------------------------------------------------------------
// Plugin entry points

#[no_mangle]
pub unsafe extern "C" fn XPluginStart(name: *mut c_char, sig: *mut c_char, desc: *mut c_char) -> c_int {
    copy_out(name, "Pause at ToD");
    copy_out(sig, "com.github.pauseattod");
    copy_out(desc, "Pauses the sim at top of descent, a waypoint, an altitude or a timer.");
    guard("start", 0, || {
        xp::enable_feature("XPLM_USE_NATIVE_PATHS");

        let menu = xp::Menu::new(
            "Pause at ToD",
            &[("Settings...", MENU_SETTINGS), ("Armed", MENU_ARM)],
            menu_handler,
        );
        if menu.is_none() {
            xp::log("could not create the menu");
        }
        let window = gui::EguiWindow::new(
            "Pause at ToD",
            430,
            560,
            gui::Callbacks {
                draw: draw_window,
                mouse: mouse_click,
                right_mouse: right_click,
                key: key_press,
                cursor,
                wheel: mouse_wheel,
            },
        );
        let app = App::new(window, menu);
        APP.with(|a| *a.borrow_mut() = Some(app));

        COMMANDS.with(|c| {
            let mut c = c.borrow_mut();
            c.push(xp::OwnedCommand::new(
                "pauseattod/toggle_window",
                "Pause at ToD: show/hide settings",
                command_handler,
                CMD_TOGGLE_WINDOW,
            ));
            c.push(xp::OwnedCommand::new(
                "pauseattod/toggle_arm",
                "Pause at ToD: arm/disarm",
                command_handler,
                CMD_TOGGLE_ARM,
            ));
        });
        xp::log(concat!("started v", env!("CARGO_PKG_VERSION")));
        1
    })
}

#[no_mangle]
pub unsafe extern "C" fn XPluginStop() {
    guard("stop", (), || {
        COMMANDS.with(|c| c.borrow_mut().clear());
        with_app(App::shutdown);
        // Tearing the window down can call back into us; keep BUSY set so
        // anything posted meanwhile is only queued, then discard it.
        BUSY.with(|b| b.set(true));
        let _busy = BusyGuard;
        let app = APP.with(|a| a.borrow_mut().take());
        drop(app);
        QUEUE.with(|q| q.borrow_mut().clear());
    })
}

#[no_mangle]
pub unsafe extern "C" fn XPluginEnable() -> c_int {
    guard("enable", 0, || {
        XPLMRegisterFlightLoopCallback(flight_loop, 1.0, std::ptr::null_mut());
        1
    })
}

#[no_mangle]
pub unsafe extern "C" fn XPluginDisable() {
    guard("disable", (), || {
        XPLMUnregisterFlightLoopCallback(flight_loop, std::ptr::null_mut());
        // A disabled plugin can't watch the flight; don't leave a stale arm behind.
        post(Event::NewFlight);
    })
}

#[no_mangle]
pub unsafe extern "C" fn XPluginReceiveMessage(_from: XPLMPluginID, msg: c_int, param: *mut c_void) {
    guard("message", (), || {
        let user_plane = param.is_null();
        if msg == XPLM_MSG_AIRPORT_LOADED || (msg == XPLM_MSG_PLANE_LOADED && user_plane) {
            post(Event::NewFlight);
        }
    })
}
