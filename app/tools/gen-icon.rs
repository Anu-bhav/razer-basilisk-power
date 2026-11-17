// Dev tool: renders assets/basilisk-power.ico (multi-size) plus a 256 px
// PNG preview. Usage: gen-icon.exe [out.ico] [preview.png]
//
// Same style language as the tray icon: flat dark rounded tile, white
// mouse silhouette, Razer-green bolt. Geometry is analytic (polygon /
// ellipse coverage tests) with adaptive supersampling, so it stays crisp
// at every size. Pure offline rendering — no external assets needed.

use std::{env, fs};

const TILE: [u8; 3] = [0x1A, 0x1A, 0x1A];
const BODY: [u8; 3] = [0xEC, 0xEC, 0xEC];
const DARK: [u8; 3] = [0x14, 0x14, 0x14];
const GREEN: [u8; 3] = [0x44, 0xD6, 0x2C];

// Bolt outline, normalized to a 1x1 box (same polygon as the tray icon).
const BOLT: [(f32, f32); 6] = [
    (0.62, 0.0),
    (0.08, 0.58),
    (0.44, 0.58),
    (0.36, 1.0),
    (0.92, 0.40),
    (0.56, 0.40),
];

/// Side-view gaming mouse profile, nose to the right, y down. Clockwise
/// from the rear-bottom corner: tall rear hump, low front, flat base.
const MOUSE: [(f32, f32); 15] = [
    (0.235, 0.660),
    (0.220, 0.610),
    (0.235, 0.530),
    (0.275, 0.425),
    (0.330, 0.335),
    (0.400, 0.280),
    (0.470, 0.265),
    (0.540, 0.275),
    (0.590, 0.305),
    (0.635, 0.350),
    (0.675, 0.410),
    (0.705, 0.480),
    (0.725, 0.550),
    (0.730, 0.610),
    (0.725, 0.660),
];

fn ellipse(x: f32, y: f32, cx: f32, cy: f32, rx: f32, ry: f32) -> bool {
    let dx = (x - cx) / rx;
    let dy = (y - cy) / ry;
    dx * dx + dy * dy <= 1.0
}

fn round_rect(x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> bool {
    if x < x0 || x >= x1 || y < y0 || y >= y1 {
        return false;
    }
    let cx = x.clamp(x0 + r, x1 - r);
    let cy = y.clamp(y0 + r, y1 - r);
    (x - cx) * (x - cx) + (y - cy) * (y - cy) <= r * r
}

fn point_in_poly(px: f32, py: f32, poly: &[(f32, f32)]) -> bool {
    let mut inside = false;
    let mut j = poly.len() - 1;
    for (i, &(xi, yi)) in poly.iter().enumerate() {
        let (xj, yj) = poly[j];
        if (yi > py) != (yj > py) && px < (xj - xi) * (py - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn in_mouse(x: f32, y: f32) -> bool {
    point_in_poly(x, y, &MOUSE)
}

/// Scroll wheel: dark notch cut into the top of the hump.
fn in_wheel(x: f32, y: f32) -> bool {
    ellipse(x, y, 0.450, 0.320, 0.038, 0.058)
}

/// Button seam between the top edge and the body, subtle detail at large
/// sizes only.
fn in_seam(x: f32, y: f32) -> bool {
    (0.594..0.606).contains(&x) && (0.320..0.450).contains(&y)
}

fn in_bolt(x: f32, y: f32) -> bool {
    let (bx, by, bw, bh) = (0.775, 0.375, 0.165, 0.285);
    let (u, v) = ((x - bx) / bw, (y - by) / bh);
    (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v) && point_in_poly(u, v, &BOLT)
}

fn render_icon(size: u32) -> Vec<u8> {
    // 4x at tray sizes, 8x at 128+ so the polygon edges look smooth.
    let ss = (size / 32).clamp(4, 8) as usize;
    let n = size as usize;
    let mut buf = vec![0u8; n * n * 4];
    for py in 0..n {
        for px in 0..n {
            let mut acc = [0f32; 4];
            for sy in 0..ss {
                for sx in 0..ss {
                    let fx = (px as f32 + (sx as f32 + 0.5) / ss as f32) / size as f32;
                    let fy = (py as f32 + (sy as f32 + 0.5) / ss as f32) / size as f32;
                    let c = if in_bolt(fx, fy) {
                        Some(GREEN)
                    } else if in_wheel(fx, fy) || in_seam(fx, fy) {
                        Some(DARK)
                    } else if in_mouse(fx, fy) {
                        Some(BODY)
                    } else if round_rect(fx, fy, 0.02, 0.02, 0.98, 0.98, 0.16) {
                        Some(TILE)
                    } else {
                        None
                    };
                    if let Some(c) = c {
                        acc[0] += c[0] as f32;
                        acc[1] += c[1] as f32;
                        acc[2] += c[2] as f32;
                        acc[3] += 1.0;
                    }
                }
            }
            let idx = (py * n + px) * 4;
            if acc[3] > 0.0 {
                buf[idx] = (acc[0] / acc[3]).round() as u8;
                buf[idx + 1] = (acc[1] / acc[3]).round() as u8;
                buf[idx + 2] = (acc[2] / acc[3]).round() as u8;
                buf[idx + 3] = (acc[3] / (ss * ss) as f32 * 255.0).round() as u8;
            }
        }
    }
    buf
}

fn png_bytes(rgba: Vec<u8>, w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_raw(w, h, rgba).expect("buffer size");
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("png encode");
    out
}

fn write_ico(path: &str, sizes: &[u32]) {
    let frames: Vec<(u32, Vec<u8>)> = sizes
        .iter()
        .map(|&s| (s, png_bytes(render_icon(s), s, s)))
        .collect();
    let mut out = Vec::new();
    // ICONDIR: reserved, type (1 = icon), count.
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(frames.len() as u16).to_le_bytes());
    let mut offset: u32 = 6 + 16 * frames.len() as u32;
    for (s, png) in &frames {
        let d = (*s).min(256);
        // 0 in the width/height bytes means 256.
        out.push(if d == 256 { 0 } else { d as u8 });
        out.push(if d == 256 { 0 } else { d as u8 });
        out.push(0); // palette color count
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // color planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += png.len() as u32;
    }
    for (_, png) in &frames {
        out.extend_from_slice(png);
    }
    fs::write(path, out).expect("write ico");
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let ico = args
        .get(1)
        .map(String::as_str)
        .unwrap_or("assets/basilisk-power.ico");
    let preview = args
        .get(2)
        .map(String::as_str)
        .unwrap_or("assets/icon-preview.png");
    write_ico(ico, &[16, 20, 24, 32, 48, 64, 128, 256]);
    fs::write(preview, png_bytes(render_icon(256), 256, 256)).expect("write preview");
    println!("wrote {ico} + preview {preview}");
}
