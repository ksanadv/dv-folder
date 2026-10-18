// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
fn inside_round_rect(x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> bool {
    if x < x0 || x > x1 || y < y0 || y > y1 {
        return false;
    }
    let cx = x.clamp(x0 + r, (x1 - r).max(x0 + r));
    let cy = y.clamp(y0 + r, (y1 - r).max(y0 + r));
    (x - cx).powi(2) + (y - cy).powi(2) <= r * r
}

fn over(dst: &mut [f32; 4], src: [f32; 4]) {
    let a = src[3] + dst[3] * (1.0 - src[3]);
    if a > 0.0 {
        for i in 0..3 {
            dst[i] = (src[i] * src[3] + dst[i] * dst[3] * (1.0 - src[3])) / a;
        }
    }
    dst[3] = a;
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

// a manila folder on a transparent background
fn sample(x: f32, y: f32, _size: f32) -> [f32; 4] {
    let mut c = [0.0f32; 4];
    // shadow
    if inside_round_rect(x, y, 0.09, 0.27, 0.95, 0.91, 0.06) {
        over(&mut c, [0.0, 0.0, 0.0, 0.18]);
    }
    // back panel with its tab
    let tab = inside_round_rect(x, y, 0.06, 0.16, 0.42, 0.34, 0.04)
        || (x > 0.30 && x < 0.48 && y > 0.22 && y < 0.34 && (y - 0.22) >= (x - 0.40) * 1.2);
    if tab || inside_round_rect(x, y, 0.06, 0.24, 0.94, 0.86, 0.05) {
        over(&mut c, [0.86, 0.63, 0.17, 1.0]);
    }
    // a sheet of paper peeking out
    if inside_round_rect(x, y, 0.13, 0.29, 0.87, 0.6, 0.02) {
        over(&mut c, [0.97, 0.97, 0.95, 1.0]);
    }
    // front panel
    if inside_round_rect(x, y, 0.06, 0.38, 0.94, 0.88, 0.05) {
        let p = mix([1.0, 0.84, 0.38], [0.97, 0.73, 0.24], ((y - 0.38) / 0.5).clamp(0.0, 1.0));
        over(&mut c, [p[0], p[1], p[2], 1.0]);
    }
    // highlight along the front panel's top edge
    if x > 0.08 && x < 0.92 && y > 0.385 && y < 0.405 {
        over(&mut c, [1.0, 0.95, 0.75, 0.8]);
    }
    c
}

pub fn rgba(size: u32) -> Vec<u8> {
    const SS: u32 = 4;
    let s = size as f32;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let (mut r, mut g, mut b, mut a) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) / s;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) / s;
                    let c = sample(x, y, s);
                    r += c[0] * c[3];
                    g += c[1] * c[3];
                    b += c[2] * c[3];
                    a += c[3];
                }
            }
            let n = (SS * SS) as f32;
            let (cr, cg, cb) = if a > 0.0 { (r / a, g / a, b / a) } else { (0.0, 0.0, 0.0) };
            out.extend_from_slice(&[(cr * 255.0) as u8, (cg * 255.0) as u8, (cb * 255.0) as u8, (a / n * 255.0) as u8]);
        }
    }
    out
}

pub fn ico(sizes: &[u32]) -> Vec<u8> {
    let mut images: Vec<Vec<u8>> = Vec::new();
    for &size in sizes {
        let px = rgba(size);
        let mut d = Vec::new();
        d.extend_from_slice(&40u32.to_le_bytes()); // BITMAPINFOHEADER
        d.extend_from_slice(&(size as i32).to_le_bytes());
        d.extend_from_slice(&(size as i32 * 2).to_le_bytes()); // height covers XOR + AND masks
        d.extend_from_slice(&1u16.to_le_bytes());
        d.extend_from_slice(&32u16.to_le_bytes());
        d.extend_from_slice(&[0; 24]);
        for y in (0..size).rev() {
            for x in 0..size {
                let i = ((y * size + x) * 4) as usize;
                d.extend_from_slice(&[px[i + 2], px[i + 1], px[i], px[i + 3]]); // BGRA, bottom-up
            }
        }
        d.resize(d.len() + (((size + 31) / 32) * 4 * size) as usize, 0); // AND mask: all opaque (alpha rules)
        images.push(d);
    }
    let mut out = Vec::new();
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&size, img) in sizes.iter().zip(&images) {
        out.extend_from_slice(&[(size % 256) as u8, (size % 256) as u8, 0, 0, 1, 0, 32, 0]);
        out.extend_from_slice(&(img.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for img in images {
        out.extend_from_slice(&img);
    }
    out
}
