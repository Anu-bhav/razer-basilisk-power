//! Colour-wheel picker for the Basilisk V3 Pro and Mouse Dock Pro LEDs.
//!
//! The picker uses the same layout as OpenRGB's colour wheel: a full hue ring
//! around an inner saturation/value square. Hue is picked on the ring (red at
//! 3 o'clock, increasing counter-clockwise), saturation on the square's X
//! axis and value on its Y axis. The brightness slider maps to the devices'
//! 0-255 hardware brightness, independently of the colour value.
//!
//! Each device has its own colour, because the same RGB value renders
//! slightly differently on the mouse and the dock's underglow. On launch the
//! picker asks both devices for their stored static colour (`GET_EFFECT`,
//! class `0x0F` / command `0x82`). If a device is asleep or undocked the last
//! successfully applied colour is used instead.
//!
//! Moving the wheel pushes the colour to the selected device as you drag, so
//! the LEDs follow the picker live (the way OpenRGB's picker feels). Writes
//! are coalesced through a single background worker: at most one hardware
//! write is in flight, intermediate positions are skipped, and the final
//! resting colour is what gets saved for the next launch.

#![windows_subsystem = "windows"]

use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use eframe::egui;
use razer_battery::lighting::{self, Rgb, Target};

const TAU: f32 = std::f32::consts::TAU;
const ACCENT: egui::Color32 = egui::Color32::from_rgb(0, 202, 255);
const ACCENT_MAGENTA: egui::Color32 = egui::Color32::from_rgb(255, 43, 255);
const PANEL: egui::Color32 = egui::Color32::from_rgb(18, 19, 23);

const DEFAULT_MOUSE: Rgb = (0x16, 0x00, 0x54);
const DEFAULT_DOCK: Rgb = (0x16, 0x00, 0x54);

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([400.0, 540.0])
            .with_min_inner_size([360.0, 500.0])
            .with_title("Razer LED colour"),
        ..Default::default()
    };

    eframe::run_native(
        "Razer LED colour",
        options,
        Box::new(|cc| Ok(Box::new(LedApp::new(cc)))),
    )
}

#[derive(Clone, Copy)]
struct DeviceColor {
    hue: f32,
    sat: f32,
    val: f32,
}

impl DeviceColor {
    fn from_rgb(rgb: Rgb) -> Self {
        let (hue, sat, val) = rgb_to_hsv(rgb);
        Self { hue, sat, val }
    }

    fn rgb(self) -> Rgb {
        hsv_to_rgb(self.hue, self.sat, self.val)
    }

    fn hex(self) -> String {
        hex(self.rgb())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WheelZone {
    Ring,
    Square,
}

struct LiveRequest {
    target: Target,
    rgb: Rgb,
    brightness: Option<u8>,
}

/// Minimum gap between live hardware writes. The in-flight gate already
/// limits the rate; this keeps very fast devices from being hammered.
const LIVE_INTERVAL: Duration = Duration::from_millis(60);

struct LedApp {
    mouse: DeviceColor,
    dock: DeviceColor,
    active: Target,
    brightness: u8,
    pending: bool,
    status: String,
    status_ok: Option<bool>,
    read_note: String,
    rx: Option<Receiver<(String, bool)>>,
    ring: Option<egui::TextureHandle>,
    square: Option<egui::TextureHandle>,
    square_hue: f32,
    drag_zone: Option<WheelZone>,
    dirty: [bool; 2],
    brightness_dirty: bool,
    live_inflight: bool,
    live_rx: Option<Receiver<(LiveRequest, lighting::DeviceOutcome)>>,
    apply_queued: bool,
    last_live_at: Option<Instant>,
}

impl LedApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut style = (*cc.egui_ctx.style()).clone();
        style.visuals = egui::Visuals::dark();
        style.visuals.panel_fill = PANEL;
        style.visuals.extreme_bg_color = egui::Color32::from_rgb(12, 13, 16);
        style.visuals.selection.bg_fill = ACCENT;
        style.visuals.selection.stroke = egui::Stroke::new(1.0_f32, egui::Color32::BLACK);
        style.visuals.widgets.active.bg_fill = ACCENT_MAGENTA;
        style.visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(44, 47, 55);
        style.visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(34, 36, 42);
        style.spacing.item_spacing = egui::vec2(10.0, 7.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        cc.egui_ctx.set_style(style);

        let ring = cc.egui_ctx.load_texture(
            "hue-ring",
            make_ring(512),
            egui::TextureOptions::LINEAR,
        );
        let mouse = DeviceColor::from_rgb(DEFAULT_MOUSE);
        let square = cc.egui_ctx.load_texture(
            "sv-square",
            make_square(mouse.hue, 192),
            egui::TextureOptions::LINEAR,
        );

        let mut app = Self {
            mouse,
            dock: DeviceColor::from_rgb(DEFAULT_DOCK),
            active: Target::Mouse,
            brightness: 100,
            pending: false,
            status: String::new(),
            status_ok: None,
            read_note: String::new(),
            rx: None,
            ring: Some(ring),
            square: Some(square),
            square_hue: mouse.hue,
            drag_zone: None,
            dirty: [false, false],
            brightness_dirty: false,
            live_inflight: false,
            live_rx: None,
            apply_queued: false,
            last_live_at: None,
        };
        app.load_current_colors();
        app
    }

    /// Ask both devices for their stored static colour, falling back to the
    /// last colour we applied, then to the theme defaults.
    fn load_current_colors(&mut self) {
        let readings = lighting::read_colors(&[Target::Mouse, Target::Dock]);
        let mut note = Vec::new();

        for reading in readings {
            let target = reading.target;
            let name = target.label();
            match lighting::effective_color(&reading) {
                Some((rgb, "device")) => {
                    self.set_color(target, rgb);
                    note.push(format!("{name} {} (device)", hex(rgb)));
                }
                Some((rgb, source)) => {
                    self.set_color(target, rgb);
                    note.push(format!("{name} {} ({source})", hex(rgb)));
                }
                None => {
                    let reason = reading
                        .error
                        .or_else(|| {
                            reading
                                .effect
                                .map(|id| format!("effect {id:#04x} is not static"))
                        })
                        .unwrap_or_else(|| "no static colour".to_string());
                    note.push(format!("{name}: {reason}"));
                }
            }
        }

        self.read_note = note.join("  ·  ");
        self.status = "Drag the wheel to preview live.".to_string();
        self.status_ok = None;
        self.refresh_square();
    }

    fn color(&self, target: Target) -> &DeviceColor {
        match target {
            Target::Mouse => &self.mouse,
            Target::Dock => &self.dock,
        }
    }

    fn color_mut(&mut self, target: Target) -> &mut DeviceColor {
        match target {
            Target::Mouse => &mut self.mouse,
            Target::Dock => &mut self.dock,
        }
    }

    fn set_color(&mut self, target: Target, rgb: Rgb) {
        *self.color_mut(target) = DeviceColor::from_rgb(rgb);
    }

    fn slot(target: Target) -> usize {
        match target {
            Target::Mouse => 0,
            Target::Dock => 1,
        }
    }

    fn is_dirty(&self, target: Target) -> bool {
        self.dirty[Self::slot(target)]
    }

    fn mark_dirty(&mut self, target: Target) {
        self.dirty[Self::slot(target)] = true;
    }

    fn clear_dirty(&mut self, target: Target) {
        self.dirty[Self::slot(target)] = false;
    }

    /// Rebuild the saturation/value square when the edited hue changes.
    fn refresh_square(&mut self) {
        let hue = self.color(self.active).hue;
        if (hue - self.square_hue).abs() < 0.0005 {
            return;
        }
        if let Some(texture) = &mut self.square {
            texture.set(make_square(hue, 192), egui::TextureOptions::LINEAR);
        }
        self.square_hue = hue;
    }

    fn set_active_hue(&mut self, hue: f32) {
        self.color_mut(self.active).hue = hue;
        self.refresh_square();
        self.mark_dirty(self.active);
    }

    fn poll_result(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.rx else { return };
        match rx.try_recv() {
            Ok((summary, ok)) => {
                self.status = summary;
                self.status_ok = Some(ok);
                self.pending = false;
                self.rx = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(Duration::from_millis(120));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.status = "The LED worker stopped unexpectedly.".to_string();
                self.status_ok = Some(false);
                self.pending = false;
                self.rx = None;
            }
        }
    }

    /// Coalesced live preview: at most one write is in flight, and only the
    /// newest colour is sent once it finishes. The final settled value is
    /// persisted so the next launch opens on it.
    fn pump_live(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.live_rx {
            match rx.try_recv() {
                Ok((request, outcome)) => {
                    self.live_inflight = false;
                    self.live_rx = None;

                    if outcome.ok() {
                        if !self.is_dirty(request.target)
                            && self.color(request.target).rgb() == request.rgb
                        {
                            lighting::save_color(request.target, request.rgb);
                        }
                        self.status = format!(
                            "Live · {} {}",
                            request.target.label(),
                            hex(request.rgb)
                        );
                        self.status_ok = None;
                    } else {
                        self.status = format!(
                            "Live preview failed: {}",
                            live_error(&outcome)
                        );
                        self.status_ok = Some(false);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(25));
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.live_inflight = false;
                    self.live_rx = None;
                    self.status = "The live preview worker stopped.".to_string();
                    self.status_ok = Some(false);
                }
            }
        }

        if self.live_inflight || self.pending {
            return;
        }

        if self.apply_queued {
            self.apply_queued = false;
            self.start_apply();
            return;
        }

        let other = match self.active {
            Target::Mouse => Target::Dock,
            Target::Dock => Target::Mouse,
        };
        let target = if self.is_dirty(self.active) {
            Some(self.active)
        } else if self.is_dirty(other) {
            Some(other)
        } else if self.brightness_dirty {
            Some(self.active)
        } else {
            None
        };
        let Some(target) = target else { return };

        if let Some(last) = self.last_live_at {
            let elapsed = last.elapsed();
            if elapsed < LIVE_INTERVAL {
                ctx.request_repaint_after(LIVE_INTERVAL - elapsed);
                return;
            }
        }

        let request = LiveRequest {
            target,
            rgb: self.color(target).rgb(),
            brightness: if self.brightness_dirty {
                Some(lighting::percent_to_brightness(self.brightness))
            } else {
                None
            },
        };

        self.clear_dirty(target);
        if request.brightness.is_some() {
            self.brightness_dirty = false;
        }
        self.last_live_at = Some(Instant::now());
        self.live_inflight = true;
        self.live_rx = Some(spawn_preview(request));
    }

    fn apply(&mut self) {
        // Do not interleave the dock's multi-report sequence with a live
        // write; queue the full apply until the preview finishes.
        if self.live_inflight {
            self.apply_queued = true;
            self.status = "Finishing the live preview...".to_string();
            self.status_ok = None;
            return;
        }
        self.start_apply();
    }

    fn start_apply(&mut self) {
        self.dirty = [false, false];
        self.brightness_dirty = false;

        let requests = vec![
            lighting::ColorRequest {
                target: Target::Mouse,
                rgb: self.mouse.rgb(),
            },
            lighting::ColorRequest {
                target: Target::Dock,
                rgb: self.dock.rgb(),
            },
        ];

        let brightness = Some(lighting::percent_to_brightness(self.brightness));
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            // Write both colours, then re-apply each one until the device
            // echoes it back. The dock re-asserts its charging LED state after
            // a dock/charge transition, which is what made the colour look
            // like it "reset" right after Apply.
            let mut outcomes = lighting::apply(&requests, brightness);
            for request in &requests {
                let mut stuck = false;
                for _ in 0..4 {
                    let (color, writes) = lighting::reapply(request.target, request.rgb, None);
                    if writes == 0 {
                        break;
                    }
                    stuck = color == Some(request.rgb);
                    if stuck {
                        break;
                    }
                }
                if let Some(outcome) =
                    outcomes.iter_mut().find(|outcome| outcome.target == request.target)
                {
                    outcome.steps.push(lighting::Step {
                        label: "verify",
                        result: if stuck {
                            Ok(())
                        } else {
                            Err(razer_battery::RazerError::Timeout { attempts: 4 })
                        },
                    });
                }
            }
            let ok = outcomes.iter().all(|outcome| outcome.ok());
            let _ = tx.send((summarize(&outcomes), ok));
        });

        self.rx = Some(rx);
        self.pending = true;
        self.status = "Writing the static colours...".to_string();
        self.status_ok = None;
    }

    fn paint_wheel(&mut self, ui: &mut egui::Ui) {
        let size = 190.0;
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(size, size),
            egui::Sense::click_and_drag(),
        );

        let center = rect.center();
        let outer = size * 0.5;
        let ring_width = size * 0.10;
        let inner = outer - ring_width;
        let square_radius = inner - 2.0;
        let side = 2.0 * square_radius / std::f32::consts::SQRT_2;
        let square_rect = egui::Rect::from_center_size(center, egui::vec2(side, side));

        let zone_at = |pos: egui::Pos2| -> Option<WheelZone> {
            let distance = (pos - center).length();
            if distance >= inner && distance <= outer {
                Some(WheelZone::Ring)
            } else if square_rect.contains(pos) {
                Some(WheelZone::Square)
            } else {
                None
            }
        };

        if response.drag_started() {
            self.drag_zone = response.interact_pointer_pos().and_then(zone_at);
        }
        if response.clicked() {
            self.drag_zone = response.interact_pointer_pos().and_then(zone_at);
        }

        if let (Some(zone), Some(pos)) = (self.drag_zone, response.interact_pointer_pos()) {
            if response.dragged() || response.clicked() {
                match zone {
                    WheelZone::Ring => {
                        let dx = pos.x - center.x;
                        let dy = center.y - pos.y;
                        let hue = dy.atan2(dx).rem_euclid(TAU) / TAU;
                        self.set_active_hue(hue);
                    }
                    WheelZone::Square => {
                        let sat = ((pos.x - square_rect.left()) / side).clamp(0.0, 1.0);
                        let val = ((pos.y - square_rect.top()) / side).clamp(0.0, 1.0);
                        let active = self.color_mut(self.active);
                        active.sat = sat;
                        active.val = val;
                        self.mark_dirty(self.active);
                    }
                }
            }
        }

        if response.drag_stopped() {
            self.drag_zone = None;
        }

        let painter = ui.painter_at(rect);
        if let Some(texture) = &self.ring {
            painter.image(
                texture.id(),
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
        if let Some(texture) = &self.square {
            painter.image(
                texture.id(),
                square_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }

        // Hue marker on the middle of the ring.
        let active = *self.color(self.active);
        let angle = active.hue * TAU;
        let indicator_radius = (outer + inner) * 0.5;
        let marker = center + egui::vec2(angle.cos(), -angle.sin()) * indicator_radius;
        let hue_degrees = active.hue * 360.0;
        let marker_color = if (20.0..200.0).contains(&hue_degrees) {
            egui::Color32::BLACK
        } else {
            egui::Color32::WHITE
        };
        painter.circle_stroke(marker, 5.0, egui::Stroke::new(3.0_f32, marker_color));

        // Saturation/value marker inside the square.
        let picker = egui::pos2(
            square_rect.left() + active.sat * side,
            square_rect.top() + active.val * side,
        );
        let picker_color = if active.sat > 0.12 || active.val < 0.2 {
            egui::Color32::WHITE
        } else {
            egui::Color32::BLACK
        };
        painter.circle_stroke(picker, 5.0, egui::Stroke::new(3.0_f32, picker_color));
    }
}

impl eframe::App for LedApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_result(ctx);

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("RAZER")
                        .size(13.0)
                        .strong()
                        .color(ACCENT_MAGENTA),
                );
                ui.label(
                    egui::RichText::new("LED COLOUR")
                        .size(13.0)
                        .strong()
                        .color(ACCENT),
                );
            });
            if !self.read_note.is_empty() {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&self.read_note)
                            .size(10.5)
                            .color(egui::Color32::from_rgb(130, 140, 150)),
                    )
                    .truncate(),
                );
            }
            ui.add_space(6.0);

            // ---- device selector + per-device swatches --------------------
            let width = (ui.available_width() - 12.0) * 0.5;
            ui.horizontal(|ui| {
                for target in [Target::Mouse, Target::Dock] {
                    let color = self.color(target).rgb();
                    let selected = self.active == target;
                    let button = egui::Button::new(
                        egui::RichText::new(format!(
                            "{}\n{}",
                            target.label(),
                            self.color(target).hex()
                        ))
                        .size(12.0)
                        .strong()
                        .color(text_on(color)),
                    )
                    .fill(egui::Color32::from_rgb(color.0, color.1, color.2))
                    .stroke(if selected {
                        egui::Stroke::new(2.0_f32, egui::Color32::WHITE)
                    } else {
                        egui::Stroke::new(1.0_f32, egui::Color32::from_gray(70))
                    })
                    .min_size(egui::vec2(width, 40.0));

                    if ui.add(button).clicked() {
                        self.active = target;
                        self.refresh_square();
                    }
                }
            });

            ui.add_space(2.0);

            // ---- OpenRGB-style hue ring + saturation/value square ---------
            ui.vertical_centered(|ui| {
                self.paint_wheel(ui);
            });

            // ---- selected colour -----------------------------------------
            let active = *self.color(self.active);
            let (r, g, b) = active.rgb();
            ui.horizontal(|ui| {
                let (swatch, _) =
                    ui.allocate_exact_size(egui::vec2(36.0, 24.0), egui::Sense::hover());
                ui.painter()
                    .rect_filled(swatch, 5.0, egui::Color32::from_rgb(r, g, b));
                ui.painter().rect_stroke(
                    swatch,
                    5.0,
                    egui::Stroke::new(1.0_f32, egui::Color32::from_gray(90)),
                );
                ui.label(
                    egui::RichText::new(format!("{r:02X}{g:02X}{b:02X}"))
                        .monospace()
                        .size(17.0)
                        .strong(),
                );
                if ui.button("Copy to other").clicked() {
                    let other = match self.active {
                        Target::Mouse => Target::Dock,
                        Target::Dock => Target::Mouse,
                    };
                    self.set_color(other, (r, g, b));
                    self.mark_dirty(other);
                }
            });

            let brightness_before = self.brightness;
            ui.add(
                egui::Slider::new(&mut self.brightness, 0..=100)
                    .text("LED brightness")
                    .suffix("%"),
            );
            if self.brightness != brightness_before {
                self.brightness_dirty = true;
                self.mark_dirty(self.active);
            }

            // ---- presets --------------------------------------------------
            ui.horizontal_wrapped(|ui| {
                for (name, (pr, pg, pb)) in PRESETS {
                    let fill = egui::Color32::from_rgb(pr / 3 + 8, pg / 3 + 8, pb / 3 + 8);
                    let text_color = if (pr as u16 + pg as u16 + pb as u16) > 420 {
                        egui::Color32::BLACK
                    } else {
                        egui::Color32::from_gray(235)
                    };
                    let button = egui::Button::new(
                        egui::RichText::new(name)
                            .size(11.0)
                            .color(text_color)
                            .strong(),
                    )
                    .fill(fill)
                    .min_size(egui::vec2(58.0, 22.0));
                    if ui.add(button).clicked() {
                        self.set_color(self.active, (pr, pg, pb));
                        self.refresh_square();
                        self.mark_dirty(self.active);
                    }
                }
            });

            ui.add_space(4.0);

            // ---- apply ----------------------------------------------------
            let busy = self.pending || self.apply_queued;
            let can_apply = !busy;
            let apply = egui::Button::new(
                egui::RichText::new(if busy {
                    "Applying..."
                } else {
                    "Apply to both LEDs"
                })
                .size(14.5)
                .strong()
                .color(egui::Color32::BLACK),
            )
            .fill(if can_apply {
                ACCENT
            } else {
                egui::Color32::from_gray(70)
            })
            .min_size(egui::vec2(ui.available_width(), 34.0));

            if ui.add_enabled(can_apply, apply).clicked() {
                self.apply();
            }

            if self.pending {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(egui::RichText::new(&self.status).weak());
                });
            } else if !self.status.is_empty() {
                let color = match self.status_ok {
                    Some(true) => egui::Color32::from_rgb(120, 230, 160),
                    Some(false) => egui::Color32::from_rgb(255, 120, 120),
                    None => egui::Color32::from_gray(180),
                };
                ui.label(egui::RichText::new(&self.status).size(10.5).color(color));
            }
        });

        self.pump_live(ctx);
    }
}

const PRESETS: [(&str, (u8, u8, u8)); 8] = [
    ("Purple", (0x16, 0x00, 0x54)),
    ("Cyan", (0, 202, 255)),
    ("Magenta", (255, 43, 255)),
    ("White", (255, 255, 255)),
    ("Warm", (255, 176, 96)),
    ("Red", (255, 40, 40)),
    ("Green", (40, 230, 120)),
    ("Blue", (60, 90, 255)),
];

fn hex(rgb: Rgb) -> String {
    format!("#{:02X}{:02X}{:02X}", rgb.0, rgb.1, rgb.2)
}

fn text_on(rgb: Rgb) -> egui::Color32 {
    let luminance = 0.2126 * rgb.0 as f32 + 0.7152 * rgb.1 as f32 + 0.0722 * rgb.2 as f32;
    if luminance > 145.0 {
        egui::Color32::BLACK
    } else {
        egui::Color32::from_gray(242)
    }
}

fn spawn_preview(request: LiveRequest) -> Receiver<(LiveRequest, lighting::DeviceOutcome)> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let outcome = lighting::preview(
            lighting::ColorRequest {
                target: request.target,
                rgb: request.rgb,
            },
            request.brightness,
        );
        let _ = tx.send((request, outcome));
    });
    rx
}

fn live_error(outcome: &lighting::DeviceOutcome) -> String {
    if let Some(error) = &outcome.error {
        return error.clone();
    }
    for step in &outcome.steps {
        if let Err(error) = &step.result {
            return format!("{}: {error}", step.label);
        }
    }
    "unknown error".to_string()
}

fn summarize(outcomes: &[lighting::DeviceOutcome]) -> String {
    let mut lines = Vec::new();
    for outcome in outcomes {
        if let Some(error) = &outcome.error {
            lines.push(format!("{}: {error}", outcome.target.label()));
            continue;
        }
        let mut steps = Vec::new();
        for step in &outcome.steps {
            match &step.result {
                Ok(()) => steps.push(format!("{} OK", step.label)),
                Err(error) => steps.push(format!("{} failed ({error})", step.label)),
            }
        }
        lines.push(format!("{}: {}", outcome.target.label(), steps.join(" / ")));
    }
    if lines.is_empty() {
        "Nothing to apply.".to_string()
    } else {
        lines.join("\n")
    }
}

/// Hue ring with a transparent centre, red at 3 o'clock and hue increasing
/// counter-clockwise (the OpenRGB layout).
fn make_ring(size: usize) -> egui::ColorImage {
    let mut pixels = vec![0u8; size * size * 4];
    let center = (size as f32 - 1.0) * 0.5;
    let outer = center;
    let thickness = size as f32 * 0.10;
    let inner = outer - thickness;

    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let distance = (dx * dx + dy * dy).sqrt();
            let index = (y * size + x) * 4;

            if distance > outer + 1.0 || distance < inner - 1.0 {
                continue;
            }

            let hue = (-dy).atan2(dx).rem_euclid(TAU) / TAU;
            let (r, g, b) = hsv_to_rgb(hue, 1.0, 1.0);

            // One-pixel soft edges instead of a jagged circle.
            let outer_edge = (outer + 0.5 - distance).clamp(0.0, 1.0);
            let inner_edge = (distance - inner + 0.5).clamp(0.0, 1.0);
            let alpha = (outer_edge.min(inner_edge) * 255.0) as u8;

            pixels[index] = r;
            pixels[index + 1] = g;
            pixels[index + 2] = b;
            pixels[index + 3] = alpha;
        }
    }

    egui::ColorImage::from_rgba_unmultiplied([size, size], &pixels)
}

/// Saturation (X) / value (Y) square for one hue, matching the OpenRGB
/// orientation: black at the top, full value at the bottom.
fn make_square(hue: f32, size: usize) -> egui::ColorImage {
    let mut pixels = vec![0u8; size * size * 4];
    let range = (size as f32 - 1.0).max(1.0);

    for y in 0..size {
        for x in 0..size {
            let sat = x as f32 / range;
            let val = y as f32 / range;
            let (r, g, b) = hsv_to_rgb(hue, sat, val);
            let index = (y * size + x) * 4;
            pixels[index] = r;
            pixels[index + 1] = g;
            pixels[index + 2] = b;
            pixels[index + 3] = 255;
        }
    }

    egui::ColorImage::from_rgba_unmultiplied([size, size], &pixels)
}

fn hsv_to_rgb(hue: f32, sat: f32, value: f32) -> (u8, u8, u8) {
    let h = hue.rem_euclid(1.0) * 6.0;
    let sector = h.floor() as i32;
    let f = h - sector as f32;

    let p = value * (1.0 - sat);
    let q = value * (1.0 - sat * f);
    let t = value * (1.0 - sat * (1.0 - f));

    let (r, g, b) = match sector.rem_euclid(6) {
        0 => (value, t, p),
        1 => (q, value, p),
        2 => (p, value, t),
        3 => (p, q, value),
        4 => (t, p, value),
        _ => (value, p, q),
    };

    (
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    )
}

fn rgb_to_hsv(rgb: Rgb) -> (f32, f32, f32) {
    let r = rgb.0 as f32 / 255.0;
    let g = rgb.1 as f32 / 255.0;
    let b = rgb.2 as f32 / 255.0;

    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;

    let hue = if delta == 0.0 {
        0.0
    } else if max == r {
        ((g - b) / delta).rem_euclid(6.0)
    } else if max == g {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    };

    (
        hue / 6.0,
        if max == 0.0 { 0.0 } else { delta / max },
        max,
    )
}
