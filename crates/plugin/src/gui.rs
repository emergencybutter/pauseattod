//! An egui context living in a modern (XPLM 3) floating window.
//!
//! X-Plane hands us boxel coordinates and raw key codes. This module turns
//! them into egui input, runs a frame from the draw callback, and paints the
//! result with egui_glow while preserving X-Plane's GL state.

use std::ffi::{c_char, c_int};
use std::sync::Arc;
use std::time::Instant;

use egui::{Event, Key, Modifiers, PointerButton, Pos2};

use crate::ffi::*;
use crate::gl::{self, SavedState, Transform};
use crate::xp::{self, cstring, screen_bounds};

/// Height, in boxels, of the strip X-Plane uses for its own controls on popped-out windows.
const POPPED_OUT_TOP_STRIP: f32 = 28.0;

pub struct Callbacks {
    pub draw: XPLMDrawWindow_f,
    pub mouse: XPLMHandleMouseClick_f,
    pub right_mouse: XPLMHandleMouseClick_f,
    pub key: XPLMHandleKey_f,
    pub cursor: XPLMHandleCursor_f,
    pub wheel: XPLMHandleMouseWheel_f,
}

enum Painter {
    NotYet,
    Ready(Box<egui_glow::Painter>),
    Failed,
}

pub struct EguiWindow {
    id: XPLMWindowID,
    ctx: egui::Context,
    painter: Painter,
    /// Transform from the last draw, used to place mouse events between frames.
    xform: Option<Transform>,
    events: Vec<Event>,
    modifiers: Modifiers,
    pointer_inside: bool,
    last_pointer: Option<Pos2>,
    start: Instant,
    fit: FitState,
}

/// Closed-loop sizing of popped-out windows. The SDK doesn't say whether the
/// OS geometry getter and setter both include the OS title bar, so instead of
/// trusting them we measure what each resize really did and learn the offset.
#[derive(Default)]
struct FitState {
    popped_out: bool,
    want: f32,
    attempts: u8,
    /// Frames to wait for the OS to apply the last resize.
    cooldown: u8,
    /// Last request: the change we wanted (OS px) and the client height before (points).
    pending: Option<(i32, f32)>,
    /// Learned difference between the height we set and the height we get.
    offset_px: i32,
}

/// Popped-out resize attempts per content change; bounds any mis-estimate.
const FIT_MAX_ATTEMPTS: u8 = 6;
const FIT_COOLDOWN_FRAMES: u8 = 6;

impl EguiWindow {
    pub fn new(title: &str, width: i32, height: i32, cb: Callbacks) -> Self {
        let (sl, st, sr, sb) = screen_bounds();
        let left = (sl + sr) / 2 - width / 2;
        let top = (st + sb) / 2 + height / 2;
        let mut params = XPLMCreateWindow_t {
            structSize: std::mem::size_of::<XPLMCreateWindow_t>() as c_int,
            left,
            top,
            right: left + width,
            bottom: top - height,
            visible: 0,
            drawWindowFunc: cb.draw,
            handleMouseClickFunc: cb.mouse,
            handleKeyFunc: cb.key,
            handleCursorFunc: cb.cursor,
            handleMouseWheelFunc: cb.wheel,
            refcon: std::ptr::null_mut(),
            decorateAsFloatingWindow: xplm_WindowDecorationRoundRectangle,
            layer: xplm_WindowLayerFloatingWindows,
            handleRightClickFunc: cb.right_mouse,
        };
        let id = unsafe { XPLMCreateWindowEx(&mut params) };
        let t = cstring(title);
        unsafe {
            XPLMSetWindowTitle(id, t.as_ptr());
            XPLMSetWindowPositioningMode(id, xplm_WindowPositionFree, -1);
            // Height follows the content (see `fit_height`); only width is the user's.
            XPLMSetWindowResizingLimits(id, 360, 120, 900, 1400);
        }
        Self {
            id,
            ctx: egui::Context::default(),
            painter: Painter::NotYet,
            xform: None,
            events: Vec::new(),
            modifiers: Modifiers::default(),
            pointer_inside: false,
            last_pointer: None,
            start: Instant::now(),
            fit: FitState::default(),
        }
    }

    pub fn context(&self) -> &egui::Context {
        &self.ctx
    }

    pub fn is_visible(&self) -> bool {
        unsafe { XPLMGetWindowIsVisible(self.id) != 0 }
    }

    pub fn set_visible(&self, visible: bool) {
        unsafe {
            XPLMSetWindowIsVisible(self.id, visible as c_int);
            if visible {
                XPLMBringWindowToFront(self.id);
            } else if XPLMHasKeyboardFocus(self.id) != 0 {
                XPLMTakeKeyboardFocus(std::ptr::null_mut());
            }
        }
    }

    fn to_points(&self, x: c_int, y: c_int) -> Option<Pos2> {
        self.xform.map(|t| t.to_points(x as f32, y as f32))
    }

    // ---------------------------------------------------------------------
    // Input from X-Plane callbacks

    pub fn on_mouse(&mut self, x: c_int, y: c_int, status: c_int, button: PointerButton) {
        let Some(pos) = self.to_points(x, y) else { return };
        self.events.push(Event::PointerMoved(pos));
        self.last_pointer = Some(pos);
        let pressed = match status {
            xplm_MouseDown => true,
            xplm_MouseUp => false,
            _ => return, // xplm_MouseDrag: the move above is all egui needs
        };
        self.events.push(Event::PointerButton { pos, button, pressed, modifiers: self.modifiers });
    }

    pub fn on_wheel(&mut self, x: c_int, y: c_int, wheel: c_int, clicks: c_int) {
        let Some(pos) = self.to_points(x, y) else { return };
        let delta = if wheel == 0 { egui::vec2(0.0, clicks as f32) } else { egui::vec2(clicks as f32, 0.0) };
        self.events.push(Event::PointerMoved(pos));
        self.events.push(Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta,
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: self.modifiers,
        });
    }

    pub fn on_key(&mut self, key: c_char, flags: c_int, vkey: c_char, losing_focus: bool) {
        if losing_focus {
            self.ctx.memory_mut(|m| m.stop_text_input());
            return;
        }
        let mods = Modifiers {
            alt: flags & xplm_OptionAltFlag != 0,
            ctrl: flags & xplm_ControlFlag != 0,
            shift: flags & xplm_ShiftFlag != 0,
            mac_cmd: false,
            command: flags & xplm_ControlFlag != 0,
        };
        self.modifiers = mods;
        let pressed = flags & xplm_DownFlag != 0;
        if !pressed && flags & xplm_UpFlag == 0 {
            return; // a modifier-only update
        }
        if let Some(k) = map_key(vkey as u8) {
            self.events.push(Event::Key { key: k, physical_key: None, pressed, repeat: false, modifiers: mods });
        }
        let ch = key as u8;
        if pressed && !mods.ctrl && (0x20..0x7f).contains(&ch) {
            self.events.push(Event::Text((ch as char).to_string()));
        }
    }

    // ---------------------------------------------------------------------
    // Frame

    /// Runs one egui frame and paints it, then fits the window's height to the
    /// content. `ui` returns the height its content needs, in points.
    /// Call from the window's draw callback.
    pub fn draw(&mut self, mut ui: impl FnMut(&mut egui::Ui) -> f32) {
        gl::set_xplm_state_for_ui();
        let painter = match &mut self.painter {
            Painter::Failed => return,
            Painter::Ready(p) => p,
            Painter::NotYet => {
                let gl = Arc::new(unsafe { gl::load_context() });
                match egui_glow::Painter::new(gl, "", None, false) {
                    Ok(p) => {
                        self.painter = Painter::Ready(Box::new(p));
                        let Painter::Ready(p) = &mut self.painter else { unreachable!() };
                        p
                    }
                    Err(e) => {
                        xp::log(&format!("cannot draw the window, OpenGL setup failed: {e}"));
                        self.painter = Painter::Failed;
                        return;
                    }
                }
            }
        };
        let gl = painter.gl().clone();

        let Some(xform) = (unsafe { Transform::current(gl.as_ref()) }) else { return };
        self.xform = Some(xform);
        let ppp = xform.pixels_per_boxel();

        let (mut l, mut t, mut r, mut b) = (0, 0, 0, 0);
        unsafe { XPLMGetWindowGeometry(self.id, &mut l, &mut t, &mut r, &mut b) };
        let mut rect = egui::Rect::from_two_pos(
            xform.to_points(l as f32, t as f32),
            xform.to_points(r as f32, b as f32),
        );
        let full_height = rect.height();
        // Popped out, X-Plane draws its "pop back in" button inside our client
        // area, top right. Leave that strip to X-Plane.
        let popped_out = unsafe { XPLMWindowIsPoppedOut(self.id) } != 0;
        let top_strip = if popped_out { POPPED_OUT_TOP_STRIP } else { 0.0 };
        rect.min.y = (rect.min.y + top_strip).min(rect.max.y);

        // X-Plane only reports clicks and drags, so poll for hover.
        let (mut mx, mut my) = (0, 0);
        unsafe { XPLMGetMouseLocationGlobal(&mut mx, &mut my) };
        let mouse = xform.to_points(mx as f32, my as f32);
        let inside = rect.contains(mouse);
        if inside && self.last_pointer != Some(mouse) {
            self.events.push(Event::PointerMoved(mouse));
            self.last_pointer = Some(mouse);
        } else if !inside && self.pointer_inside && !self.ctx.egui_is_using_pointer() {
            self.events.push(Event::PointerGone);
            self.last_pointer = None;
        }
        self.pointer_inside = inside;

        let mut raw = egui::RawInput {
            screen_rect: Some(rect),
            time: Some(self.start.elapsed().as_secs_f64()),
            events: std::mem::take(&mut self.events),
            focused: true,
            max_texture_side: Some(painter.max_texture_side()),
            ..Default::default()
        };
        raw.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point = Some(ppp);

        let mut content_height = None;
        let mut out = self.ctx.run_ui(raw, |root| content_height = Some(ui(root)));
        let prims = self.ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);

        unsafe {
            let saved = SavedState::capture(gl.as_ref());
            painter.paint_and_update_textures(xform.target_size_px(), out.pixels_per_point, &prims, &mut out.textures_delta);
            saved.restore(gl.as_ref());
        }

        for cmd in std::mem::take(&mut out.platform_output.commands) {
            if let egui::OutputCommand::OpenUrl(u) = cmd {
                open_url(&u.url);
            }
        }

        if let Some(h) = content_height {
            self.fit_height((h + top_strip).ceil(), full_height, popped_out, ppp);
        }

        // Route typing to us only while a text field is focused.
        let has_focus = unsafe { XPLMHasKeyboardFocus(self.id) != 0 };
        let wants = self.ctx.egui_wants_keyboard_input();
        if wants && !has_focus {
            unsafe { XPLMTakeKeyboardFocus(self.id) };
        } else if !wants && has_focus {
            unsafe { XPLMTakeKeyboardFocus(std::ptr::null_mut()) };
        }
    }
}

impl EguiWindow {
    /// Resizes the window, keeping its top edge, so the client area is `want`
    /// points tall. `current` is the client height in points right now.
    fn fit_height(&mut self, want: f32, current: f32, popped_out: bool, pixels_per_point: f32) {
        if current <= 0.0 {
            return;
        }
        if !popped_out {
            self.fit = FitState::default();
            if (want - current).abs() > 1.0 {
                // Floating windows are in boxels, and one egui point is one boxel.
                let (mut l, mut t, mut r, mut b) = (0, 0, 0, 0);
                unsafe {
                    XPLMGetWindowGeometry(self.id, &mut l, &mut t, &mut r, &mut b);
                    XPLMSetWindowGeometry(self.id, l, t, r, t - want as c_int);
                }
            }
            return;
        }

        let f = &mut self.fit;
        if !f.popped_out {
            *f = FitState { popped_out: true, ..FitState::default() };
        }
        if f.cooldown > 0 {
            f.cooldown -= 1;
            return;
        }
        if let Some((wanted, before)) = f.pending.take() {
            let got = ((current - before) * pixels_per_point).round() as i32;
            f.offset_px = (f.offset_px - (got - wanted)).clamp(-200, 200);
        }
        if (want - f.want).abs() > 0.5 {
            f.want = want;
            f.attempts = 0;
        }
        if (want - current).abs() <= 1.0 || f.attempts >= FIT_MAX_ATTEMPTS {
            return;
        }
        f.attempts += 1;
        let wanted = ((want - current) * pixels_per_point).round() as i32;
        let (mut l, mut t, mut r, mut b) = (0, 0, 0, 0);
        unsafe { XPLMGetWindowGeometryOS(self.id, &mut l, &mut t, &mut r, &mut b) };
        let h = ((t - b).abs() + wanted + f.offset_px).max(100);
        let bottom = if t > b { t - h } else { t + h };
        unsafe { XPLMSetWindowGeometryOS(self.id, l, t, r, bottom) };
        f.pending = Some((wanted, current));
        f.cooldown = FIT_COOLDOWN_FRAMES;
    }
}

/// Opens a link from the UI in the default browser.
fn open_url(url: &str) {
    if !url.starts_with("https://") {
        return;
    }
    #[cfg(windows)]
    let opener = "explorer";
    #[cfg(target_os = "macos")]
    let opener = "open";
    #[cfg(target_os = "linux")]
    let opener = "xdg-open";
    if let Err(e) = std::process::Command::new(opener).arg(url).spawn() {
        xp::log(&format!("could not open {url}: {e}"));
    }
}

impl Drop for EguiWindow {
    fn drop(&mut self) {
        if let Painter::Ready(p) = &mut self.painter {
            p.destroy();
        }
        unsafe { XPLMDestroyWindow(self.id) };
    }
}

fn map_key(vk: u8) -> Option<Key> {
    Some(match vk {
        XPLM_VK_BACK => Key::Backspace,
        XPLM_VK_TAB => Key::Tab,
        XPLM_VK_RETURN | XPLM_VK_ENTER | XPLM_VK_NUMPAD_ENT => Key::Enter,
        XPLM_VK_ESCAPE => Key::Escape,
        XPLM_VK_END => Key::End,
        XPLM_VK_HOME => Key::Home,
        XPLM_VK_LEFT => Key::ArrowLeft,
        XPLM_VK_UP => Key::ArrowUp,
        XPLM_VK_RIGHT => Key::ArrowRight,
        XPLM_VK_DOWN => Key::ArrowDown,
        XPLM_VK_DELETE => Key::Delete,
        // XPLM letter and digit codes are their ASCII values.
        b'A'..=b'Z' | b'0'..=b'9' => return Key::from_name(&(vk as char).to_string()),
        _ => return None,
    })
}
