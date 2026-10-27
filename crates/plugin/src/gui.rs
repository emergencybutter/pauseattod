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
}

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
            XPLMSetWindowResizingLimits(id, 360, 300, 900, 1200);
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

    /// Runs one egui frame and paints it. Call from the window's draw callback.
    pub fn draw(&mut self, ui: impl FnMut(&mut egui::Ui)) {
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
        // Popped out, X-Plane draws its "pop back in" button inside our client
        // area, top right. Leave that strip to X-Plane.
        if unsafe { XPLMWindowIsPoppedOut(self.id) } != 0 {
            rect.min.y = (rect.min.y + POPPED_OUT_TOP_STRIP).min(rect.max.y);
        }

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

        let mut out = self.ctx.run_ui(raw, ui);
        let prims = self.ctx.tessellate(std::mem::take(&mut out.shapes), out.pixels_per_point);

        unsafe {
            let saved = SavedState::capture(gl.as_ref());
            painter.paint_and_update_textures(xform.target_size_px(), out.pixels_per_point, &prims, &mut out.textures_delta);
            saved.restore(gl.as_ref());
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
