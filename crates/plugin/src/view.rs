//! The settings window's content, as plain egui code.

use egui::{
    vec2, Align, Button, Color32, ComboBox, CornerRadius, DragValue, Frame, Layout, Margin, ProgressBar, Response,
    RichText, Sense, Stroke, StrokeKind, TextEdit, Ui,
};
use pauseattod_core::{AltRef, ConditionKind, ConditionStatus, Direction, Settings};

const BG: Color32 = Color32::from_rgb(22, 25, 30);
const CARD: Color32 = Color32::from_rgb(31, 35, 42);
const BORDER: Color32 = Color32::from_rgb(45, 50, 59);
const TEXT: Color32 = Color32::from_rgb(226, 230, 236);
const MUTED: Color32 = Color32::from_rgb(140, 148, 160);
const ACCENT: Color32 = Color32::from_rgb(64, 140, 235);
const GREEN: Color32 = Color32::from_rgb(70, 190, 110);
const AMBER: Color32 = Color32::from_rgb(240, 170, 40);
const RED: Color32 = Color32::from_rgb(225, 85, 75);

const END_OF_DESCENT_HELP: &str = "Where your descent should end and at what altitude. This plugin will attempt to guess your destination based on your flight plan, as well as the airport elevation, however you should make sure destination and elevation are guessed or set correctly before arming.";

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Muted,
    Good,
    Bad,
}

/// A one-line hint under an input, e.g. the result of a waypoint lookup.
pub struct Preview {
    pub text: String,
    pub tone: Tone,
}

pub struct ViewModel<'a> {
    pub settings: &'a mut Settings,
    /// Text of the optional ToD crossing altitude; blank means automatic.
    pub cross_text: &'a mut String,
    pub armed: bool,
    pub paused: bool,
    pub message: &'a str,
    pub statuses: &'a [ConditionStatus],
    pub wpt_preview: Option<Preview>,
    pub tod_preview: Option<Preview>,
}

pub enum Action {
    ToggleArm,
}

pub fn install_style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = BG;
    v.extreme_bg_color = Color32::from_rgb(16, 18, 22);
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = ACCENT.linear_multiply(0.6);
    v.selection.stroke = Stroke::new(1.0, ACCENT);
    let radius = CornerRadius::same(5);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = radius;
    }
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(42, 47, 56);
    v.widgets.inactive.bg_fill = Color32::from_rgb(42, 47, 56);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(52, 58, 69);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT.linear_multiply(0.7));
    v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    ctx.set_visuals(v);
    ctx.style_mut_of(egui::Theme::Dark, |s| {
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.button_padding = vec2(8.0, 4.0);
        s.spacing.interact_size.y = 22.0;
        use egui::{FontFamily::Proportional, FontId, TextStyle};
        s.text_styles.insert(TextStyle::Body, FontId::new(14.0, Proportional));
        s.text_styles.insert(TextStyle::Button, FontId::new(14.0, Proportional));
        s.text_styles.insert(TextStyle::Small, FontId::new(11.5, Proportional));
        s.text_styles.insert(TextStyle::Heading, FontId::new(17.0, Proportional));
    });
    ctx.set_theme(egui::Theme::Dark);
}

/// Draws the window and returns the height its content needs, in points.
///
/// A floating window is resized to fit, so its content is laid out at full
/// height. A popped-out window keeps whatever size it has, so it scrolls.
pub fn show(ui: &mut Ui, vm: &mut ViewModel, scroll: bool, actions: &mut Vec<Action>) -> f32 {
    ui.painter().rect_filled(ui.max_rect(), 0.0, BG);
    let mut content = |ui: &mut Ui| {
        Frame::new()
            .inner_margin(Margin::same(12))
            .show(ui, |ui| {
                header(ui, vm, actions);
                ui.add_space(10.0);
                conditions(ui, vm);
                about(ui);
            })
            .response
            .rect
            .height()
    };
    if scroll {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, content).inner
    } else {
        content(ui)
    }
}

fn about(ui: &mut Ui) {
    ui.add_space(2.0);
    ui.separator();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let small = |t: &str| RichText::new(t).small().color(MUTED);
        ui.label(small(concat!("Pause at ToD v", env!("CARGO_PKG_VERSION"), " · Help and feature requests:")));
        ui.hyperlink_to(RichText::new("flyvoyager.net/discord").small().color(ACCENT), "https://flyvoyager.net/discord");
    });
    ui.label(
        RichText::new("Bind keys to pauseattod/toggle_arm and pauseattod/toggle_window.").small().color(MUTED),
    );
}

fn header(ui: &mut Ui, vm: &ViewModel, actions: &mut Vec<Action>) {
    let (color, label) = match (vm.armed, vm.paused) {
        (true, true) => (AMBER, "ARMED · sim paused"),
        (true, false) => (GREEN, "ARMED"),
        (false, _) => (MUTED, "DISARMED"),
    };
    ui.horizontal(|ui| {
        let (dot, _) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
        ui.painter().circle_filled(dot.center(), 5.0, color);
        ui.label(RichText::new(label).strong().size(16.0).color(color));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let (text, fill) = if vm.armed { ("Disarm", RED) } else { ("Arm", ACCENT) };
            let button = Button::new(RichText::new(text).strong().size(15.0).color(Color32::WHITE))
                .fill(fill)
                .min_size(vec2(100.0, 30.0))
                .corner_radius(CornerRadius::same(6));
            if ui.add(button).clicked() {
                actions.push(Action::ToggleArm);
            }
        });
    });
    if !vm.armed && !vm.message.is_empty() {
        let color = if vm.message.starts_with("Cannot") {
            RED
        } else if vm.message.starts_with("PAUSED") {
            AMBER
        } else {
            MUTED
        };
        ui.label(RichText::new(vm.message).color(color));
    }
}

fn conditions(ui: &mut Ui, vm: &mut ViewModel) {
    let editable = !vm.armed;
    let status = |kind| vm.statuses.iter().find(|s| s.kind == kind);
    let s = &mut *vm.settings;

    card(
        ui,
        "Top of descent",
        "FMS T/D, 3:1 rule or descent onset",
        &mut s.tod_enabled,
        editable,
        status(ConditionKind::TopOfDescent),
        vm.tod_preview.as_ref(),
        |ui| {
            ui.horizontal(|ui| {
                ui.label("Reach").on_hover_text(END_OF_DESCENT_HELP);
                id_field(ui, &mut s.tod_point, "arrival").on_hover_text(END_OF_DESCENT_HELP);
                ui.label("at").on_hover_text(END_OF_DESCENT_HELP);
                ui.add(TextEdit::singleline(vm.cross_text).hint_text("auto").desired_width(56.0).char_limit(6))
                    .on_hover_text(END_OF_DESCENT_HELP);
                ui.label("ft MSL").on_hover_text(END_OF_DESCENT_HELP);
            });
            ui.horizontal(|ui| {
                ui.label("Pause");
                ui.add(DragValue::new(&mut s.tod_lead_nm).range(0.0..=50.0).speed(0.2).suffix(" nm"));
                ui.label("before the top of descent");
            });
        },
    );

    card(
        ui,
        "Near waypoint",
        "great-circle distance",
        &mut s.wpt_enabled,
        editable,
        status(ConditionKind::Waypoint),
        vm.wpt_preview.as_ref(),
        |ui| {
            ui.horizontal(|ui| {
                ui.label("Within");
                ui.add(DragValue::new(&mut s.wpt_within_nm).range(0.0..=500.0).speed(0.5).suffix(" nm"));
                ui.label("of");
                id_field(ui, &mut s.wpt_id, "ident");
            });
        },
    );

    card(
        ui,
        "Altitude",
        "on an actual crossing",
        &mut s.alt_enabled,
        editable,
        status(ConditionKind::Altitude),
        None,
        |ui| {
            ui.horizontal(|ui| {
                ComboBox::from_id_salt("alt_dir")
                    .selected_text(capitalized(s.alt_dir.label()))
                    .width(110.0)
                    .show_ui(ui, |ui| {
                        for d in Direction::ALL {
                            ui.selectable_value(&mut s.alt_dir, d, capitalized(d.label()));
                        }
                    });
                ui.label("through");
                ui.add(DragValue::new(&mut s.alt_ft).range(-2000.0..=60000.0).speed(50.0).suffix(" ft"));
                for r in [AltRef::Msl, AltRef::Agl] {
                    ui.selectable_value(&mut s.alt_ref, r, r.label());
                }
            });
        },
    );

    card(
        ui,
        "Timer",
        "paused time doesn't count",
        &mut s.timer_enabled,
        editable,
        status(ConditionKind::Timer),
        None,
        |ui| {
            ui.horizontal(|ui| {
                ui.label("After");
                ui.add(DragValue::new(&mut s.timer_min).range(1.0..=1440.0).speed(1.0).suffix(" min"));
                ui.label("of sim time");
            });
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn card(
    ui: &mut Ui,
    title: &str,
    subtitle: &str,
    enabled: &mut bool,
    editable: bool,
    status: Option<&ConditionStatus>,
    preview: Option<&Preview>,
    body: impl FnOnce(&mut Ui),
) {
    let stroke = if *enabled { Stroke::new(1.0, ACCENT.linear_multiply(0.55)) } else { Stroke::new(1.0, BORDER) };
    Frame::new()
        .fill(CARD)
        .stroke(stroke)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add_enabled_ui(editable, |ui| toggle(ui, enabled));
                ui.label(RichText::new(title).strong().size(15.0).color(if *enabled { TEXT } else { MUTED }));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(subtitle).small().color(MUTED));
                });
            });
            if !*enabled {
                return;
            }
            ui.add_space(4.0);
            ui.add_enabled_ui(editable, body);
            if let (Some(p), true) = (preview, editable) {
                let color = match p.tone {
                    Tone::Muted => MUTED,
                    Tone::Good => GREEN,
                    Tone::Bad => RED,
                };
                ui.label(RichText::new(&p.text).small().color(color));
            }
            if let Some(st) = status {
                ui.add_space(4.0);
                ui.label(&st.text);
                if let Some(p) = st.progress {
                    ui.add(ProgressBar::new(p).desired_height(6.0).fill(ACCENT).corner_radius(CornerRadius::same(3)));
                }
            }
        });
    ui.add_space(6.0);
}

/// An uppercase navaid identifier field.
fn id_field(ui: &mut Ui, text: &mut String, hint: &str) -> Response {
    let r = ui.add(TextEdit::singleline(text).hint_text(hint).desired_width(72.0).char_limit(7));
    if r.changed() {
        *text = text.to_ascii_uppercase();
    }
    r
}

/// An iOS-style switch.
fn toggle(ui: &mut Ui, on: &mut bool) -> Response {
    let size = vec2(2.0, 1.0) * ui.spacing().interact_size.y;
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let off_fill = Color32::from_rgb(58, 64, 75);
        let fill = if ui.is_enabled() { lerp_color(off_fill, ACCENT, t) } else { off_fill.linear_multiply(0.6) };
        let radius = rect.height() / 2.0;
        ui.painter().rect(rect, radius, fill, Stroke::NONE, StrokeKind::Inside);
        let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), t);
        ui.painter().circle_filled(egui::pos2(x, rect.center().y), radius - 3.0, Color32::WHITE);
    }
    response
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}
