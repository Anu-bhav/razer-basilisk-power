// Draws the tray icon as a battery glyph: outline + level bar, colored by
// severity, with a lightning bolt while charging. Rendered directly at the
// tray's pixel size (no downscaling) with 4x4 supersampling, so edges stay
// crisp at 16-32 px.

use crate::reminder::LOW_LEVEL;

pub const RED_LEVEL: i32 = 20;

const GREEN: [u8; 3] = [0x44, 0xD6, 0x2C];
const ORANGE: [u8; 3] = [0xF5, 0xA5, 0x24];
const RED: [u8; 3] = [0xE5, 0x48, 0x4D];
const GRAY: [u8; 3] = [0x8A, 0x8A, 0x8A];
const DARK: [u8; 3] = [0x1A, 0x1A, 0x1A];
const WHITE: [u8; 3] = [0xFF, 0xFF, 0xFF];

const SUPERSAMPLE: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub outline: [u8; 3],
    pub fill: [u8; 3],
    pub bolt: bool,
}

/// Color logic for the tray icon. `level` is `None` when the last read
/// failed (mouse asleep / out of range).
pub fn tray_style(level: Option<i32>, charging: bool, light_taskbar: bool) -> Style {
    let outline = if light_taskbar { DARK } else { WHITE };
    let Some(level) = level else {
        return Style {
            outline: GRAY,
            fill: GRAY,
            bolt: false,
        };
    };
    let fill = match level {
        _ if charging => GREEN,
        l if l <= RED_LEVEL => RED,
        l if l <= LOW_LEVEL => ORANGE,
        _ => outline,
    };
    Style {
        outline,
        fill,
        bolt: charging,
    }
}

#[derive(Clone, Copy)]
enum Shape {
    RoundRect {
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        r: f32,
    },
    Bolt {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    },
}

// Bolt outline, normalized to a 1x1 box.
const BOLT: [(f32, f32); 6] = [
    (0.62, 0.0),
    (0.08, 0.58),
    (0.44, 0.58),
    (0.36, 1.0),
    (0.92, 0.40),
    (0.56, 0.40),
];

impl Shape {
    fn contains(&self, px: f32, py: f32) -> bool {
        match *self {
            Shape::RoundRect { x0, y0, x1, y1, r } => {
                if px < x0 || px >= x1 || py < y0 || py >= y1 {
                    return false;
                }
                let cx = px.clamp(x0 + r, x1 - r);
                let cy = py.clamp(y0 + r, y1 - r);
                (px - cx).powi(2) + (py - cy).powi(2) <= r * r
            }
            Shape::Bolt { x, y, w, h } => {
                let (u, v) = ((px - x) / w, (py - y) / h);
                let mut inside = false;
                let mut j = BOLT.len() - 1;
                for (i, &(xi, yi)) in BOLT.iter().enumerate() {
                    let (xj, yj) = BOLT[j];
                    if (yi > v) != (yj > v) && u < (xj - xi) * (v - yi) / (yj - yi) + xi {
                        inside = !inside;
                    }
                    j = i;
                }
                inside
            }
        }
    }
}

/// Renders a `size`x`size` RGBA (straight alpha) battery glyph filled to
/// `level` percent.
pub fn render(size: u32, level: i32, style: Style) -> Vec<u8> {
    let s = size as f32;
    let px = |f: f32| (s * f).round().max(1.0);

    let t = (s / 12.0).round().max(1.0); // outline thickness
    let gap = (s / 16.0).round().max(1.0); // space between outline and bar
    let nub_w = px(0.1);
    let body_h = px(0.62);
    let y0 = ((s - body_h) / 2.0).floor();
    let y1 = y0 + body_h;
    let x0 = 0.0;
    let x1 = s - nub_w;
    let r = (s * 0.14).max(1.0);

    let inner_x0 = x0 + t + gap;
    let inner_x1 = x1 - t - gap;
    let fill_w = if level > 0 {
        ((inner_x1 - inner_x0) * level.clamp(0, 100) as f32 / 100.0).max(1.0)
    } else {
        0.0
    };
    let nub_h = (body_h * 0.42).round();
    let nub_y0 = y0 + ((body_h - nub_h) / 2.0).round();

    // Painter's layers; `None` clears to transparent.
    let mut layers: Vec<(Shape, Option<[u8; 3]>)> = vec![
        (Shape::RoundRect { x0, y0, x1, y1, r }, Some(style.outline)),
        (
            Shape::RoundRect {
                x0: x0 + t,
                y0: y0 + t,
                x1: x1 - t,
                y1: y1 - t,
                r: (r - t).max(0.0),
            },
            None,
        ),
        (
            Shape::RoundRect {
                x0: x1,
                y0: nub_y0,
                x1: s,
                y1: nub_y0 + nub_h,
                r: 0.0,
            },
            Some(style.outline),
        ),
    ];
    if fill_w > 0.0 {
        layers.push((
            Shape::RoundRect {
                x0: inner_x0,
                y0: y0 + t + gap,
                x1: inner_x0 + fill_w,
                y1: y1 - t - gap,
                r: 0.0,
            },
            Some(style.fill),
        ));
    }
    if style.bolt {
        // Fits inside the body so the green level bar stays visible.
        let h = body_h - t;
        let w = h * 0.62;
        let bx = (x1 - w) / 2.0;
        let by = y0 + t / 2.0;
        // Transparent halo keeps the bolt readable over the green bar.
        let halo = (s / 24.0).max(0.75);
        for (dx, dy) in [
            (-1.0, 0.0),
            (1.0, 0.0),
            (0.0, -1.0),
            (0.0, 1.0),
            (-0.7, -0.7),
            (0.7, -0.7),
            (-0.7, 0.7),
            (0.7, 0.7),
        ] {
            layers.push((
                Shape::Bolt {
                    x: bx + dx * halo,
                    y: by + dy * halo,
                    w,
                    h,
                },
                None,
            ));
        }
        layers.push((Shape::Bolt { x: bx, y: by, w, h }, Some(style.outline)));
    }

    let n = size as usize;
    let mut buf = vec![0u8; n * n * 4];
    let samples = (SUPERSAMPLE * SUPERSAMPLE) as f32;
    for py in 0..n {
        for pxl in 0..n {
            let mut acc = [0f32; 4]; // premultiplied
            for sy in 0..SUPERSAMPLE {
                for sx in 0..SUPERSAMPLE {
                    let fx = pxl as f32 + (sx as f32 + 0.5) / SUPERSAMPLE as f32;
                    let fy = py as f32 + (sy as f32 + 0.5) / SUPERSAMPLE as f32;
                    let mut color = None;
                    for (shape, c) in &layers {
                        if shape.contains(fx, fy) {
                            color = *c;
                        }
                    }
                    if let Some(c) = color {
                        acc[0] += c[0] as f32;
                        acc[1] += c[1] as f32;
                        acc[2] += c[2] as f32;
                        acc[3] += 1.0;
                    }
                }
            }
            let idx = (py * n + pxl) * 4;
            if acc[3] > 0.0 {
                buf[idx] = (acc[0] / acc[3]).round() as u8;
                buf[idx + 1] = (acc[1] / acc[3]).round() as u8;
                buf[idx + 2] = (acc[2] / acc[3]).round() as u8;
                buf[idx + 3] = (acc[3] / samples * 255.0).round() as u8;
            }
        }
    }
    buf
}

/// 3x5 bitmap digits, rows top→bottom, bit 2 = leftmost column.
const DIGIT_GLYPHS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111], // 0
    [0b010, 0b110, 0b010, 0b010, 0b111], // 1
    [0b111, 0b001, 0b111, 0b100, 0b111], // 2
    [0b111, 0b001, 0b011, 0b001, 0b111], // 3
    [0b101, 0b101, 0b111, 0b001, 0b001], // 4
    [0b111, 0b100, 0b111, 0b001, 0b111], // 5
    [0b111, 0b100, 0b111, 0b101, 0b111], // 6
    [0b111, 0b001, 0b010, 0b010, 0b010], // 7
    [0b111, 0b101, 0b111, 0b101, 0b111], // 8
    [0b111, 0b101, 0b111, 0b001, 0b111], // 9
];
/// Dash for the unknown state ("-").
const DASH_GLYPH: [u8; 5] = [0b000, 0b000, 0b111, 0b000, 0b000];

/// Renders the raw battery number ("36", "100" or "--") instead of the
/// battery glyph, colored by severity, with a thin green underline while
/// charging. Same supersampled pipeline as `render`.
pub fn render_digits(size: u32, level: Option<i32>, charging: bool, light_taskbar: bool) -> Vec<u8> {
    let color = tray_style(level, charging, light_taskbar).fill;
    let text = match level {
        Some(l) => l.to_string(),
        None => "--".to_owned(),
    };
    let s = size as f32;
    let glyphs: Vec<&[u8; 5]> = text
        .chars()
        .map(|c| match c {
            d @ '0'..='9' => &DIGIT_GLYPHS[d as usize - '0' as usize],
            _ => &DASH_GLYPH,
        })
        .collect();
    let n = glyphs.len() as f32;
    // Cell height ≈ 60% of the icon; shrink to fit three digits ("100").
    let cell = (((s * 0.60) / 5.0).min(s * 0.8 / (3.0 * n + (n - 1.0) * 0.6))).max(1.0);
    let total_w = 3.0 * n * cell + (n - 1.0) * 0.6 * cell;
    let x0 = (s - total_w) / 2.0;
    let y0 = (s - 5.0 * cell) / 2.0;

    let n_px = size as usize;
    let mut buf = vec![0u8; n_px * n_px * 4];
    let samples = (SUPERSAMPLE * SUPERSAMPLE) as f32;
    for py in 0..n_px {
        for pxl in 0..n_px {
            let mut acc = 0f32;
            for sy in 0..SUPERSAMPLE {
                for sx in 0..SUPERSAMPLE {
                    let fx = pxl as f32 + (sx as f32 + 0.5) / SUPERSAMPLE as f32;
                    let fy = py as f32 + (sy as f32 + 0.5) / SUPERSAMPLE as f32;
                    if digit_hit(&glyphs, fx, fy, x0, y0, cell) {
                        acc += 1.0;
                    }
                }
            }
            if acc > 0.0 {
                let idx = (py * n_px + pxl) * 4;
                buf[idx] = color[0];
                buf[idx + 1] = color[1];
                buf[idx + 2] = color[2];
                buf[idx + 3] = (acc / samples * 255.0).round() as u8;
            }
        }
    }
    // Charging underline strip below the digits.
    if charging {
        let h = ((s / 16.0).round().max(1.0) as usize).min(n_px);
        let w = (total_w.round() as usize).min(n_px);
        let sx = (x0.round() as usize).min(n_px.saturating_sub(w));
        let sy = (((y0 + 5.0 * cell + cell * 0.4).round()) as usize).min(n_px.saturating_sub(h));
        for py in sy..sy + h {
            for px in sx..sx + w {
                let idx = (py * n_px + px) * 4;
                buf[idx] = GREEN[0];
                buf[idx + 1] = GREEN[1];
                buf[idx + 2] = GREEN[2];
                buf[idx + 3] = 255;
            }
        }
    }
    buf
}

/// True when the sample point lands on a filled cell of the digit strip.
fn digit_hit(glyphs: &[&[u8; 5]], fx: f32, fy: f32, x0: f32, y0: f32, cell: f32) -> bool {
    let rel_x = fx - x0;
    let rel_y = fy - y0;
    if rel_x < 0.0 || rel_y < 0.0 {
        return false;
    }
    let gap = 0.6 * cell;
    let slot_w = 3.0 * cell + gap;
    let idx = (rel_x / slot_w) as usize;
    if idx >= glyphs.len() {
        return false;
    }
    let col = ((rel_x - idx as f32 * slot_w) / cell) as usize;
    let row = (rel_y / cell) as usize;
    if col >= 3 || row >= 5 {
        return false;
    }
    glyphs[idx][row] & (0b100 >> col) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_colors() {
        assert_eq!(tray_style(Some(50), false, true).fill, DARK);
        assert_eq!(tray_style(Some(50), false, false).fill, WHITE);
        assert_eq!(tray_style(Some(30), false, true).fill, ORANGE);
        assert_eq!(tray_style(Some(21), false, true).fill, ORANGE);
        assert_eq!(tray_style(Some(20), false, true).fill, RED);
        assert_eq!(tray_style(Some(3), false, true).fill, RED);
        let charging = tray_style(Some(10), true, true);
        assert_eq!(charging.fill, GREEN);
        assert!(charging.bolt);
        assert_eq!(tray_style(None, false, true).outline, GRAY);
    }

    #[test]
    fn renders_all_tray_sizes() {
        for size in [16, 20, 24, 32, 64] {
            let style = tray_style(Some(55), true, false);
            let buf = render(size, 55, style);
            assert_eq!(buf.len(), (size * size * 4) as usize);
            assert!(buf.chunks(4).any(|p| p[3] == 255));
        }
    }

    #[test]
    fn empty_battery_has_no_fill_color() {
        let style = tray_style(Some(0), false, true);
        let buf = render(32, 0, style);
        assert!(!buf.chunks(4).any(|p| p[3] > 0 && p[..3] == RED));
    }
}
