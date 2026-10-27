//! OpenGL plumbing: loading entry points from X-Plane's context and keeping
//! X-Plane's GL state intact around our drawing.

use std::ffi::{c_void, CStr};
use std::num::NonZeroU32;

use egui_glow::glow::{self, HasContext};

use crate::ffi::*;

/// Builds a glow context on top of the GL context X-Plane draws with.
///
/// # Safety
/// Must be called with X-Plane's GL context current (i.e. from a draw callback).
pub unsafe fn load_context() -> glow::Context {
    glow::Context::from_loader_function_cstr(|name| proc_address(name))
}

#[cfg(windows)]
unsafe fn proc_address(name: &CStr) -> *const c_void {
    // wglGetProcAddress only knows extension/post-1.1 functions and may return
    // small sentinel values instead of null; GL 1.1 lives in opengl32.dll itself.
    let p = wglGetProcAddress(name.as_ptr());
    if !matches!(p as isize, -1..=3) {
        return p;
    }
    let module = GetModuleHandleA(c"opengl32.dll".as_ptr());
    GetProcAddress(module, name.as_ptr())
}

#[cfg(target_os = "linux")]
unsafe fn proc_address(name: &CStr) -> *const c_void {
    glXGetProcAddressARB(name.as_ptr().cast())
}

#[cfg(target_os = "macos")]
unsafe fn proc_address(name: &CStr) -> *const c_void {
    const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;
    dlsym(RTLD_DEFAULT, name.as_ptr())
}

// Fixed-function enums glow doesn't export.
const MODELVIEW_MATRIX: u32 = 0x0BA6;
const PROJECTION_MATRIX: u32 = 0x0BA7;

/// Maps X-Plane's 2D "boxel" coordinates to framebuffer pixels, using the
/// transform X-Plane set up for the current draw callback.
#[derive(Clone, Copy, Debug)]
pub struct Transform {
    /// projection * modelview, column major.
    mvp: [f32; 16],
    viewport: [i32; 4],
}

impl Transform {
    /// # Safety
    /// GL context must be current.
    pub unsafe fn current(gl: &glow::Context) -> Option<Self> {
        let mut mv = [0.0f32; 16];
        let mut proj = [0.0f32; 16];
        let mut viewport = [0i32; 4];
        gl.get_parameter_f32_slice(MODELVIEW_MATRIX, &mut mv);
        gl.get_parameter_f32_slice(PROJECTION_MATRIX, &mut proj);
        gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
        let mut mvp = [0.0f32; 16];
        for c in 0..4 {
            for r in 0..4 {
                mvp[c * 4 + r] = (0..4).map(|k| proj[k * 4 + r] * mv[c * 4 + k]).sum();
            }
        }
        let t = Self { mvp, viewport };
        (viewport[2] > 0 && viewport[3] > 0 && t.pixels_per_boxel() > 0.01).then_some(t)
    }

    /// Framebuffer pixel position, origin bottom-left like GL.
    pub fn to_px(self, x: f32, y: f32) -> (f32, f32) {
        let m = &self.mvp;
        let cx = m[0] * x + m[4] * y + m[12];
        let cy = m[1] * x + m[5] * y + m[13];
        let cw = m[3] * x + m[7] * y + m[15];
        let (nx, ny) = if cw.abs() > f32::EPSILON { (cx / cw, cy / cw) } else { (cx, cy) };
        let [vx, vy, vw, vh] = self.viewport.map(|v| v as f32);
        (vx + (nx + 1.0) * 0.5 * vw, vy + (ny + 1.0) * 0.5 * vh)
    }

    pub fn pixels_per_boxel(self) -> f32 {
        (self.to_px(1.0, 0.0).0 - self.to_px(0.0, 0.0).0).abs()
    }

    /// Size of the area egui paints into: from the framebuffer origin to the
    /// far corner of X-Plane's viewport.
    pub fn target_size_px(self) -> [u32; 2] {
        let [vx, vy, vw, vh] = self.viewport;
        [(vx + vw).max(1) as u32, (vy + vh).max(1) as u32]
    }

    /// Boxels to egui points (origin top-left, one point per boxel).
    pub fn to_points(self, x: f32, y: f32) -> egui::Pos2 {
        let (px, py) = self.to_px(x, y);
        let ppb = self.pixels_per_boxel();
        let height = self.target_size_px()[1] as f32;
        egui::pos2(px / ppb, (height - py) / ppb)
    }
}

/// Everything egui_glow touches, captured before painting and put back after.
pub struct SavedState {
    viewport: [i32; 4],
    scissor_box: [i32; 4],
    scissor_test: bool,
    blend: bool,
    cull_face: bool,
    depth_test: bool,
    framebuffer_srgb: bool,
    blend_src_rgb: i32,
    blend_dst_rgb: i32,
    blend_src_alpha: i32,
    blend_dst_alpha: i32,
    blend_eq_rgb: i32,
    blend_eq_alpha: i32,
    program: i32,
    vertex_array: i32,
    array_buffer: i32,
    active_texture: i32,
    texture_2d: i32,
    unpack_alignment: i32,
    unpack_row_length: i32,
}

fn nz(v: i32) -> Option<NonZeroU32> {
    NonZeroU32::new(v as u32)
}

impl SavedState {
    /// # Safety
    /// GL context must be current.
    pub unsafe fn capture(gl: &glow::Context) -> Self {
        let mut viewport = [0; 4];
        let mut scissor_box = [0; 4];
        gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
        gl.get_parameter_i32_slice(glow::SCISSOR_BOX, &mut scissor_box);
        let active_texture = gl.get_parameter_i32(glow::ACTIVE_TEXTURE);
        // egui paints with unit 0, so remember what X-Plane has bound there.
        gl.active_texture(glow::TEXTURE0);
        let texture_2d = gl.get_parameter_i32(glow::TEXTURE_BINDING_2D);
        Self {
            viewport,
            scissor_box,
            scissor_test: gl.is_enabled(glow::SCISSOR_TEST),
            blend: gl.is_enabled(glow::BLEND),
            cull_face: gl.is_enabled(glow::CULL_FACE),
            depth_test: gl.is_enabled(glow::DEPTH_TEST),
            framebuffer_srgb: gl.is_enabled(glow::FRAMEBUFFER_SRGB),
            blend_src_rgb: gl.get_parameter_i32(glow::BLEND_SRC_RGB),
            blend_dst_rgb: gl.get_parameter_i32(glow::BLEND_DST_RGB),
            blend_src_alpha: gl.get_parameter_i32(glow::BLEND_SRC_ALPHA),
            blend_dst_alpha: gl.get_parameter_i32(glow::BLEND_DST_ALPHA),
            blend_eq_rgb: gl.get_parameter_i32(glow::BLEND_EQUATION_RGB),
            blend_eq_alpha: gl.get_parameter_i32(glow::BLEND_EQUATION_ALPHA),
            program: gl.get_parameter_i32(glow::CURRENT_PROGRAM),
            vertex_array: gl.get_parameter_i32(glow::VERTEX_ARRAY_BINDING),
            array_buffer: gl.get_parameter_i32(glow::ARRAY_BUFFER_BINDING),
            active_texture,
            texture_2d,
            unpack_alignment: gl.get_parameter_i32(glow::UNPACK_ALIGNMENT),
            unpack_row_length: gl.get_parameter_i32(glow::UNPACK_ROW_LENGTH),
        }
    }

    /// # Safety
    /// GL context must be current.
    pub unsafe fn restore(&self, gl: &glow::Context) {
        let set = |cap: u32, on: bool| if on { gl.enable(cap) } else { gl.disable(cap) };
        let [x, y, w, h] = self.viewport;
        gl.viewport(x, y, w, h);
        let [x, y, w, h] = self.scissor_box;
        gl.scissor(x, y, w, h);
        set(glow::SCISSOR_TEST, self.scissor_test);
        set(glow::BLEND, self.blend);
        set(glow::CULL_FACE, self.cull_face);
        set(glow::DEPTH_TEST, self.depth_test);
        set(glow::FRAMEBUFFER_SRGB, self.framebuffer_srgb);
        gl.blend_equation_separate(self.blend_eq_rgb as u32, self.blend_eq_alpha as u32);
        gl.blend_func_separate(
            self.blend_src_rgb as u32,
            self.blend_dst_rgb as u32,
            self.blend_src_alpha as u32,
            self.blend_dst_alpha as u32,
        );
        gl.use_program(nz(self.program).map(glow::NativeProgram));
        gl.bind_vertex_array(nz(self.vertex_array).map(glow::NativeVertexArray));
        gl.bind_buffer(glow::ARRAY_BUFFER, nz(self.array_buffer).map(glow::NativeBuffer));
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, nz(self.texture_2d).map(glow::NativeTexture));
        gl.active_texture(self.active_texture as u32);
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, self.unpack_alignment);
        gl.pixel_store_i32(glow::UNPACK_ROW_LENGTH, self.unpack_row_length);
    }
}

/// Tells XPLM which fixed-function state we rely on before raw GL drawing.
pub fn set_xplm_state_for_ui() {
    unsafe { XPLMSetGraphicsState(0, 0, 0, 0, 1, 0, 0) }
}
