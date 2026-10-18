// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! dvfiles' own icons, every one in the window: shapes in the spirit of Windows 10's with a little depth (a gentle
//! bevel lit from above, a darker rim, a soft drop shadow, a light gloss on the coloured marks), a folder drawn as
//! Windows 10 draws it, the toolbar's pictures, and the thin one-colour marks (arrows, chevrons, close).
//!
//! Every shape is a signed distance field in the unit square, which gives all of that cheaply: edges are smoothed
//! from the distance (one sample per pixel, sharp at 16 px as at 256), the bevel's light comes from the field's slope,
//! and a shadow is the same field offset and softened. Each icon is drawn once per size and kept, so lists never ask
//! the shell for icons; only programs and shortcuts, whose icon is their own, still come from the files.
use crate::ui::gfx::{Theme, R};
use crate::ui::thumbs::Image;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Special {
    Desktop,
    Downloads,
    Documents,
    Pictures,
    Music,
    Videos,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    Generic,
    Text,
    Code,
    Image,
    Video,
    Audio,
    Archive,
    Pdf,
    Doc,
    Sheet,
    Slides,
    App,
    Font,
    Disk,
    Web,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Art {
    Folder,
    Special(Special),
    Drive,
    Removable,
    Pc,
    Star,
    File(Kind),
    /// the shortcut arrow laid over a shortcut's icon
    Arrow,
    Tool(Tool),
}

/// the toolbar's and the dialogs' pictures
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Tool {
    Search,
    Copy,
    Props,
    Warning,
    Stop,
    Tick,
}

/// the small one-colour marks, and the navigation arrows
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Mark {
    Back,
    Forward,
    Rise,
    Refresh,
    Sort,
    Grid,
    List,
    Down,
    Up,
    Right,
    Close,
    Add,
    Check,
    More,
    Pause,
    Play,
    // the command bar's line icons
    Cut,
    Copy,
    Paste,
    Rename,
    Delete,
    Info,
}

/// how an icon shows its button's state
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Look {
    Normal,
    Hot,
}

pub fn kind_of(ext: &str) -> Kind {
    match ext {
        "txt" | "log" | "md" | "ini" | "cfg" | "conf" | "nfo" | "rtf" => Kind::Text,
        "rs" | "py" | "js" | "ts" | "jsx" | "tsx" | "c" | "h" | "cpp" | "hpp" | "cs" | "java" | "kt" | "go" | "rb"
        | "php" | "lua" | "sh" | "ps1" | "bat" | "cmd" | "json" | "xml" | "yaml" | "yml" | "toml" | "sql" | "css"
        | "swift" | "gradle" | "reg" | "vbs" => Kind::Code,
        "jpg" | "jpeg" | "jpe" | "jfif" | "png" | "gif" | "bmp" | "dib" | "tif" | "tiff" | "webp" | "heic" | "heif"
        | "avif" | "svg" | "psd" | "raw" | "cr2" | "cr3" | "nef" | "arw" | "dng" | "ico" | "tga" | "dds" => Kind::Image,
        "mp4" | "m4v" | "mov" | "avi" | "wmv" | "mkv" | "webm" | "mpg" | "mpeg" | "3gp" | "m2ts" | "flv" => Kind::Video,
        "mp3" | "flac" | "wav" | "m4a" | "aac" | "ogg" | "opus" | "wma" | "mid" | "midi" | "aiff" => Kind::Audio,
        "zip" | "7z" | "rar" | "tar" | "gz" | "bz2" | "xz" | "zst" | "cab" | "tgz" => Kind::Archive,
        "pdf" => Kind::Pdf,
        "doc" | "docx" | "odt" | "pages" => Kind::Doc,
        "xls" | "xlsx" | "ods" | "csv" | "tsv" | "numbers" => Kind::Sheet,
        "ppt" | "pptx" | "odp" | "key" => Kind::Slides,
        "exe" | "msi" | "dll" | "sys" | "com" | "scr" | "appx" | "msix" | "jar" => Kind::App,
        "ttf" | "otf" | "woff" | "woff2" | "fon" => Kind::Font,
        "iso" | "img" | "vhd" | "vhdx" | "vmdk" | "dmg" => Kind::Disk,
        "html" | "htm" | "url" | "website" | "mhtml" => Kind::Web,
        _ => Kind::Generic,
    }
}

/// the type written at the foot of a big file icon ("WAV", "PDF"), as Windows 7 wrote it
pub fn band_label(ext: &str) -> Option<String> {
    (!ext.is_empty() && ext.len() <= 5 && ext.chars().all(|c| c.is_ascii_alphanumeric())).then(|| ext.to_uppercase())
}

/// where that label goes on a file icon of `size` pixels drawn at (x, y)
pub fn band_rect(x: i32, y: i32, size: i32) -> R {
    let s = size as f32;
    let (x0, y0, x1, y1) =
        ((0.20 * s).round() as i32, (0.75 * s).round() as i32, (0.82 * s).round() as i32, (0.93 * s).round() as i32);
    R::new(x + x0, y + y0, x1 - x0, y1 - y0)
}

/// the label's colour: Windows 7's grey letters
pub fn band_color() -> crate::ui::gfx::Rgb {
    crate::ui::gfx::Rgb::hex(0x6A7380)
}

type C3 = [f32; 3];

const fn hex(v: u32) -> C3 {
    [((v >> 16) & 0xFF) as f32 / 255.0, ((v >> 8) & 0xFF) as f32 / 255.0, (v & 0xFF) as f32 / 255.0]
}

const BLACK: C3 = [0.0, 0.0, 0.0];
const WHITE: C3 = [1.0, 1.0, 1.0];

fn mix(a: C3, b: C3, t: f32) -> C3 {
    let t = t.clamp(0.0, 1.0);
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn over(dst: &mut [f32; 4], c: C3, a: f32) {
    if a <= 0.0 {
        return;
    }
    let out = a + dst[3] * (1.0 - a);
    for i in 0..3 {
        dst[i] = (c[i] * a + dst[i] * dst[3] * (1.0 - a)) / out;
    }
    dst[3] = out;
}

// ---------------------------------------------------------------------------------------------------------------
// distance fields (negative inside)

fn sd_box(x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> f32 {
    let (cx, cy) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0);
    let (hx, hy) = ((x1 - x0) / 2.0 - r, (y1 - y0) / 2.0 - r);
    let (qx, qy) = ((x - cx).abs() - hx, (y - cy).abs() - hy);
    (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r
}

fn sd_circle(x: f32, y: f32, cx: f32, cy: f32, r: f32) -> f32 {
    ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r
}

fn sd_ellipse(x: f32, y: f32, cx: f32, cy: f32, rx: f32, ry: f32) -> f32 {
    let k = (((x - cx) / rx).powi(2) + ((y - cy) / ry).powi(2)).sqrt();
    (k - 1.0) * rx.min(ry)
}

fn sd_poly(x: f32, y: f32, v: &[(f32, f32)]) -> f32 {
    let mut d = (x - v[0].0).powi(2) + (y - v[0].1).powi(2);
    let mut s = 1.0;
    let n = v.len();
    let mut j = n - 1;
    for i in 0..n {
        let (ex, ey) = (v[j].0 - v[i].0, v[j].1 - v[i].1);
        let (wx, wy) = (x - v[i].0, y - v[i].1);
        let t = ((wx * ex + wy * ey) / (ex * ex + ey * ey).max(1e-9)).clamp(0.0, 1.0);
        let (bx, by) = (wx - ex * t, wy - ey * t);
        d = d.min(bx * bx + by * by);
        let c = (y >= v[i].1, y < v[j].1, ex * wy > ey * wx);
        if (c.0 && c.1 && c.2) || (!c.0 && !c.1 && !c.2) {
            s = -s;
        }
        j = i;
    }
    s * d.sqrt()
}

fn sd_seg(x: f32, y: f32, a: (f32, f32), b: (f32, f32), r: f32) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = (((x - a.0) * dx + (y - a.1) * dy) / (dx * dx + dy * dy).max(1e-9)).clamp(0.0, 1.0);
    ((a.0 + t * dx - x).powi(2) + (a.1 + t * dy - y).powi(2)).sqrt() - r
}

// ---------------------------------------------------------------------------------------------------------------
// painting

/// the point being drawn and the size of a pixel, in unit-square terms
#[derive(Clone, Copy)]
struct P {
    x: f32,
    y: f32,
    px: f32,
}

/// a surface: a top-to-bottom gradient between y0 and y1, a darker rim about a pixel wide, a bevel `bevel` deep lit
/// from the upper left, and a glossy reflection over its top `gloss` of height
#[derive(Clone, Copy)]
struct Mat {
    top: C3,
    bottom: C3,
    y0: f32,
    y1: f32,
    rim: Option<C3>,
    bevel: f32,
    gloss: f32,
}

impl Mat {
    fn flat(top: C3, bottom: C3, y0: f32, y1: f32) -> Mat {
        Mat { top, bottom, y0, y1, rim: None, bevel: 0.0, gloss: 0.0 }
    }

    fn rim(mut self, c: C3) -> Mat {
        self.rim = Some(c);
        self
    }

    fn bevel(mut self, b: f32) -> Mat {
        self.bevel = b;
        self
    }

    fn gloss(mut self, g: f32) -> Mat {
        self.gloss = g;
        self
    }
}

// How much depth the icons have: the light and shade of a bevel, the glossy reflection and the drop shadows. Kept
// gentle, as Windows 10's icons are: shapes and colours do the work, the light only rounds them a little.
const BEVEL_LIGHT: f32 = 0.38;
const BEVEL_SHADE: f32 = 0.18;
const GLOSS: f32 = 0.22;
const SHADOW: f32 = 0.6;

fn cover(d: f32, px: f32) -> f32 {
    (0.5 - d / px).clamp(0.0, 1.0)
}

fn paint(c: &mut [f32; 4], p: P, f: &dyn Fn(f32, f32) -> f32, m: Mat) {
    let d = f(p.x, p.y);
    if d > p.px {
        return;
    }
    let mut col = mix(m.top, m.bottom, (p.y - m.y0) / (m.y1 - m.y0).max(1e-6));
    if m.bevel > 0.0 && -d < m.bevel {
        let e = p.px * 0.5;
        let (gx, gy) = (f(p.x + e, p.y) - f(p.x - e, p.y), f(p.x, p.y + e) - f(p.x, p.y - e));
        let l = (gx * gx + gy * gy).sqrt().max(1e-9);
        // facing up and to the left catches the light; facing down falls into shade
        let lit = -(gx / l * 0.45 + gy / l * 0.89);
        let k = (1.0 - (-d / m.bevel)).clamp(0.0, 1.0).powf(1.5);
        col = if lit > 0.0 { mix(col, WHITE, lit * k * BEVEL_LIGHT) } else { mix(col, BLACK, -lit * k * BEVEL_SHADE) };
    }
    if m.gloss > 0.0 {
        // the reflection: strong at the top, gone by `gloss` below it, its lower edge a gentle slant
        let top = m.y0;
        let edge = top + m.gloss * (1.0 - 0.25 * (p.x - 0.5));
        if p.y < edge {
            col = mix(col, WHITE, GLOSS * (1.0 - (p.y - top) / (edge - top).max(1e-6)).clamp(0.0, 1.0));
        }
    }
    if let Some(r) = m.rim {
        let w = p.px * 1.15;
        if -d < w {
            col = mix(col, r, 1.0 - (-d / w).clamp(0.0, 1.0));
        }
    }
    over(c, col, cover(d, p.px));
}

/// a soft shadow of a shape, moved by (dx, dy) and spread over `blur`
fn shadow(c: &mut [f32; 4], p: P, f: &dyn Fn(f32, f32) -> f32, dx: f32, dy: f32, blur: f32, alpha: f32) {
    let d = f(p.x - dx, p.y - dy);
    let alpha = alpha * SHADOW;
    let a = if d <= 0.0 { alpha } else { alpha * (1.0 - d / blur).clamp(0.0, 1.0).powi(2) };
    over(c, BLACK, a);
}

/// a colour laid over a shape, softly
fn tint(c: &mut [f32; 4], p: P, f: &dyn Fn(f32, f32) -> f32, col: C3, a: f32) {
    over(c, col, a * cover(f(p.x, p.y), p.px));
}

// ---------------------------------------------------------------------------------------------------------------
// the icons

fn folder_back(x: f32, y: f32) -> f32 {
    // the back, with its tab at the upper left, the tab's right side sloping down
    sd_box(x, y, 0.06, 0.19, 0.94, 0.86, 0.03)
        .min(sd_poly(x, y, &[(0.06, 0.12), (0.35, 0.12), (0.42, 0.21), (0.06, 0.21)]) - 0.02)
}

fn folder_front(x: f32, y: f32) -> f32 {
    sd_box(x, y, 0.06, 0.33, 0.94, 0.86, 0.03)
}

/// a folder as Windows 10 draws it, seen straight on: the darker back and its tab, a white sheet, the lighter front
fn folder(c: &mut [f32; 4], p: P) {
    shadow(c, p, &folder_back, 0.0, 0.02, 0.045, 0.30);
    paint(c, p, &folder_back, Mat::flat(hex(0xEDB13C), hex(0xD8961F), 0.12, 0.86).rim(hex(0xB97C14)));
    paint(
        c,
        p,
        &|x, y| sd_box(x, y, 0.12, 0.25, 0.88, 0.60, 0.01),
        Mat::flat(WHITE, hex(0xEFEFEF), 0.25, 0.60).rim(hex(0xCFCFCF)),
    );
    // the front casts a faint shade on the sheet, and catches the light along its top edge
    shadow(c, p, &folder_front, 0.0, -0.015, 0.03, 0.25);
    paint(c, p, &folder_front, Mat::flat(hex(0xFFD970), hex(0xF5BD3E), 0.33, 0.86).rim(hex(0xCF961F)).bevel(0.03));
    tint(c, p, &|x, y| sd_box(x, y, 0.075, 0.345, 0.925, 0.345 + (p.px * 0.9).max(0.012), 0.0), hex(0xFFF1BE), 0.9);
}

/// what marks one of the user's folders, on the folder's front
fn emblem(c: &mut [f32; 4], p: P, s: Special) {
    let shade = |c: &mut [f32; 4], f: &dyn Fn(f32, f32) -> f32| shadow(c, p, f, 0.008, 0.016, 0.03, 0.30);
    match s {
        Special::Desktop => {
            let bezel = |x: f32, y: f32| sd_box(x, y, 0.33, 0.50, 0.67, 0.74, 0.02);
            shade(c, &bezel);
            paint(
                c,
                p,
                &|x, y| sd_poly(x, y, &[(0.46, 0.73), (0.54, 0.73), (0.57, 0.81), (0.43, 0.81)]),
                Mat::flat(hex(0xC9CED6), hex(0x8A919B), 0.73, 0.81),
            );
            paint(c, p, &bezel, Mat::flat(hex(0x5B626C), hex(0x2A2F36), 0.50, 0.74).rim(hex(0x1E2228)));
            paint(
                c,
                p,
                &|x, y| sd_box(x, y, 0.355, 0.525, 0.645, 0.715, 0.005),
                Mat::flat(hex(0x6FB3F2), hex(0x1449A8), 0.525, 0.715).gloss(0.08),
            );
        }
        Special::Downloads => {
            // flat, as Windows 10's: a blue arrow down onto a line
            let arrow = |x: f32, y: f32| {
                sd_poly(
                    x,
                    y,
                    &[(0.45, 0.46), (0.55, 0.46), (0.55, 0.62), (0.63, 0.62), (0.50, 0.75), (0.37, 0.62), (0.45, 0.62)],
                ) - 0.008
            };
            tint(c, p, &arrow, hex(0x1F7AD8), 1.0);
            tint(c, p, &|x, y| sd_box(x, y, 0.35, 0.78, 0.65, 0.82, 0.0), hex(0x1F7AD8), 1.0);
        }
        Special::Documents => {
            let page = |x: f32, y: f32| sd_box(x, y, 0.37, 0.47, 0.63, 0.81, 0.012);
            shade(c, &page);
            paint(c, p, &page, Mat::flat(WHITE, hex(0xE4E9F0), 0.47, 0.81).rim(hex(0x8C96A6)));
            for k in 0..4 {
                let ly = 0.54 + 0.06 * k as f32;
                let end = if k == 3 { 0.53 } else { 0.58 };
                tint(c, p, &|x, y| sd_box(x, y, 0.42, ly, end, ly + 0.022, 0.01), hex(0x4F86D6), 1.0);
            }
        }
        Special::Pictures => {
            let frame = |x: f32, y: f32| sd_poly(x, y, &[(0.31, 0.50), (0.67, 0.47), (0.69, 0.76), (0.33, 0.79)]);
            shade(c, &frame);
            paint(c, p, &frame, Mat::flat(WHITE, hex(0xE6E6E6), 0.47, 0.79).rim(hex(0x9A9A9A)));
            let photo = |x: f32, y: f32| sd_poly(x, y, &[(0.35, 0.53), (0.64, 0.51), (0.655, 0.725), (0.365, 0.745)]);
            paint(c, p, &photo, Mat::flat(hex(0x8FD3FA), hex(0x3D8CD8), 0.51, 0.75));
            paint(
                c,
                p,
                &|x, y| {
                    sd_poly(x, y, &[(0.36, 0.745), (0.44, 0.63), (0.52, 0.70), (0.57, 0.66), (0.655, 0.725)])
                        .max(photo(x, y))
                },
                Mat::flat(hex(0x8CD35C), hex(0x2E7F22), 0.63, 0.75),
            );
            paint(c, p, &|x, y| sd_circle(x, y, 0.58, 0.58, 0.03), Mat::flat(hex(0xFFF3A0), hex(0xF5C531), 0.55, 0.61));
        }
        Special::Music => {
            let note = |x: f32, y: f32| {
                sd_ellipse(x, y, 0.45, 0.74, 0.075, 0.058)
                    .min(sd_box(x, y, 0.495, 0.47, 0.535, 0.75, 0.01))
                    .min(sd_poly(x, y, &[(0.52, 0.47), (0.64, 0.52), (0.64, 0.61), (0.52, 0.55)]) - 0.008)
            };
            shade(c, &note);
            paint(
                c,
                p,
                &note,
                Mat::flat(hex(0x8BD0FF), hex(0x1C61C9), 0.47, 0.80).rim(hex(0x174C9A)).bevel(0.035).gloss(0.12),
            );
        }
        Special::Videos => {
            let strip = |x: f32, y: f32| sd_poly(x, y, &[(0.32, 0.50), (0.68, 0.47), (0.70, 0.77), (0.34, 0.80)]);
            shade(c, &strip);
            paint(c, p, &strip, Mat::flat(hex(0x3B3842), hex(0x15131A), 0.47, 0.80).rim(hex(0x0B0A0E)));
            for k in 0..5 {
                let hx = 0.355 + 0.068 * k as f32;
                for hy in [0.505, 0.745] {
                    tint(c, p, &|x, y| sd_box(x, y, hx, hy, hx + 0.03, hy + 0.025, 0.005), hex(0xE6E6E6), 1.0);
                }
            }
            paint(
                c,
                p,
                &|x, y| sd_box(x, y, 0.36, 0.545, 0.665, 0.73, 0.004),
                Mat::flat(hex(0x9BD7FF), hex(0xE8913A), 0.545, 0.73),
            );
            paint(
                c,
                p,
                &|x, y| sd_poly(x, y, &[(0.47, 0.58), (0.57, 0.64), (0.47, 0.70)]),
                Mat::flat(WHITE, hex(0xDDDDDD), 0.58, 0.70),
            );
        }
    }
}

fn page_shape(x: f32, y: f32) -> f32 {
    sd_poly(x, y, &[(0.17, 0.05), (0.63, 0.05), (0.84, 0.26), (0.84, 0.95), (0.17, 0.95)])
}

/// a glossy tile in a kind's colour, with white marks on it (documents, sheets, slides, PDFs, programs)
fn tile(c: &mut [f32; 4], p: P, top: C3, bottom: C3, rim: C3) {
    let t = |x: f32, y: f32| sd_box(x, y, 0.29, 0.27, 0.73, 0.69, 0.06);
    shadow(c, p, &t, 0.008, 0.018, 0.035, 0.28);
    paint(c, p, &t, Mat::flat(top, bottom, 0.27, 0.69).rim(rim).bevel(0.05).gloss(0.18));
}

fn white_lines(c: &mut [f32; 4], p: P, n: usize) {
    for k in 0..n {
        let ly = 0.37 + 0.075 * k as f32;
        let end = if k + 1 == n { 0.55 } else { 0.64 };
        tint(c, p, &|x, y| sd_box(x, y, 0.38, ly, end, ly + 0.035, 0.015), WHITE, 0.95);
    }
}

fn glyph(c: &mut [f32; 4], p: P, k: Kind) {
    let shade = |c: &mut [f32; 4], f: &dyn Fn(f32, f32) -> f32| shadow(c, p, f, 0.008, 0.018, 0.035, 0.28);
    match k {
        Kind::Generic => {}
        Kind::Text => {
            for i in 0..6 {
                let ly = 0.30 + 0.075 * i as f32;
                let end = if i == 5 { 0.52 } else { 0.70 };
                tint(c, p, &|x, y| sd_box(x, y, 0.28, ly, end, ly + 0.026, 0.012), hex(0x93A0B4), 1.0);
            }
        }
        Kind::Doc => {
            tile(c, p, hex(0x7FB2F5), hex(0x1D57B8), hex(0x163F86));
            white_lines(c, p, 4);
        }
        Kind::Pdf => {
            tile(c, p, hex(0xF58A7F), hex(0xB8231B), hex(0x861810));
            white_lines(c, p, 3);
            // a turned corner on the tile, as a page would have
            tint(c, p, &|x, y| sd_poly(x, y, &[(0.60, 0.69), (0.73, 0.56), (0.73, 0.69)]), hex(0xFFD2CD), 0.9);
        }
        Kind::Sheet => {
            tile(c, p, hex(0x7FD69A), hex(0x1C8443), hex(0x13612F));
            for i in 0..4 {
                let gx = 0.36 + 0.08 * i as f32;
                tint(c, p, &|x, y| sd_box(x, y, gx, 0.35, gx + 0.022, 0.62, 0.0), WHITE, 0.85);
            }
            for i in 0..4 {
                let gy = 0.36 + 0.08 * i as f32;
                tint(c, p, &|x, y| sd_box(x, y, 0.35, gy, 0.66, gy + 0.022, 0.0), WHITE, 0.85);
            }
        }
        Kind::Slides => {
            tile(c, p, hex(0xFFAE7A), hex(0xC9461B), hex(0x8F2E10));
            // a small chart
            for (i, h) in [0.10f32, 0.17, 0.24].iter().enumerate() {
                let bx = 0.39 + 0.085 * i as f32;
                tint(c, p, &|x, y| sd_box(x, y, bx, 0.61 - h, bx + 0.055, 0.61, 0.008), WHITE, 0.95);
            }
        }
        Kind::App => {
            tile(c, p, hex(0xB5C2D3), hex(0x56657A), hex(0x3C4757));
            tint(c, p, &|x, y| sd_box(x, y, 0.36, 0.40, 0.66, 0.61, 0.012), WHITE, 0.95);
            tint(c, p, &|x, y| sd_box(x, y, 0.36, 0.34, 0.66, 0.40, 0.0), hex(0x2F7FD8), 1.0);
        }
        Kind::Audio => {
            let note = |x: f32, y: f32| {
                sd_ellipse(x, y, 0.43, 0.60, 0.085, 0.066)
                    .min(sd_box(x, y, 0.485, 0.26, 0.53, 0.61, 0.01))
                    .min(sd_poly(x, y, &[(0.515, 0.26), (0.66, 0.33), (0.66, 0.44), (0.515, 0.36)]) - 0.01)
            };
            shade(c, &note);
            paint(
                c,
                p,
                &note,
                Mat::flat(hex(0x8BD0FF), hex(0x1A5CC4), 0.26, 0.67).rim(hex(0x164A99)).bevel(0.05).gloss(0.14),
            );
        }
        Kind::Video => {
            let strip = |x: f32, y: f32| sd_box(x, y, 0.27, 0.27, 0.74, 0.66, 0.02);
            shade(c, &strip);
            paint(c, p, &strip, Mat::flat(hex(0x3B3842), hex(0x15131A), 0.27, 0.66).rim(hex(0x0B0A0E)));
            for i in 0..6 {
                let hx = 0.295 + 0.072 * i as f32;
                for hy in [0.285, 0.625] {
                    tint(c, p, &|x, y| sd_box(x, y, hx, hy, hx + 0.034, hy + 0.022, 0.004), hex(0xE6E6E6), 1.0);
                }
            }
            paint(
                c,
                p,
                &|x, y| sd_box(x, y, 0.29, 0.325, 0.72, 0.61, 0.005),
                Mat::flat(hex(0x9BD7FF), hex(0xF0A04A), 0.325, 0.61).gloss(0.10),
            );
            let play = |x: f32, y: f32| sd_poly(x, y, &[(0.45, 0.39), (0.59, 0.47), (0.45, 0.55)]) - 0.01;
            paint(c, p, &play, Mat::flat(WHITE, hex(0xDADADA), 0.39, 0.55).rim(hex(0x707070)));
        }
        Kind::Image => {
            let frame = |x: f32, y: f32| sd_poly(x, y, &[(0.25, 0.30), (0.71, 0.25), (0.75, 0.62), (0.29, 0.67)]);
            shade(c, &frame);
            paint(c, p, &frame, Mat::flat(WHITE, hex(0xE4E4E4), 0.25, 0.67).rim(hex(0x9A9A9A)));
            let photo = |x: f32, y: f32| sd_poly(x, y, &[(0.295, 0.335), (0.68, 0.293), (0.71, 0.585), (0.325, 0.627)]);
            paint(c, p, &photo, Mat::flat(hex(0x8FD3FA), hex(0x3D8CD8), 0.29, 0.63));
            paint(
                c,
                p,
                &|x, y| {
                    sd_poly(x, y, &[(0.31, 0.63), (0.43, 0.46), (0.53, 0.56), (0.59, 0.50), (0.715, 0.60)])
                        .max(photo(x, y))
                },
                Mat::flat(hex(0x8CD35C), hex(0x2E7F22), 0.46, 0.63),
            );
            paint(c, p, &|x, y| sd_circle(x, y, 0.60, 0.38, 0.04), Mat::flat(hex(0xFFF3A0), hex(0xF5C531), 0.34, 0.42));
        }
        Kind::Archive => {
            // a zip running down the page
            let band = |x: f32, y: f32| sd_box(x, y, 0.42, 0.05, 0.58, 0.68, 0.0);
            tint(c, p, &band, hex(0xE9D8B4), 0.9);
            for i in 0..7 {
                let zy = 0.08 + 0.075 * i as f32;
                let (zx0, zx1) = if i % 2 == 0 { (0.44, 0.51) } else { (0.49, 0.56) };
                paint(
                    c,
                    p,
                    &|x, y| sd_box(x, y, zx0, zy, zx1, zy + 0.045, 0.01),
                    Mat::flat(hex(0xF5DDA0), hex(0xA97A2C), zy, zy + 0.045).rim(hex(0x7D5818)),
                );
            }
            let pull = |x: f32, y: f32| sd_box(x, y, 0.44, 0.58, 0.56, 0.70, 0.02);
            shade(c, &pull);
            paint(c, p, &pull, Mat::flat(hex(0xF8E2A8), hex(0xB0832F), 0.58, 0.70).rim(hex(0x7D5818)).bevel(0.03));
        }
        Kind::Code => {
            let br = |x: f32, y: f32| {
                let w = 0.03;
                sd_seg(x, y, (0.42, 0.30), (0.30, 0.46), w)
                    .min(sd_seg(x, y, (0.30, 0.46), (0.42, 0.62), w))
                    .min(sd_seg(x, y, (0.58, 0.30), (0.70, 0.46), w))
                    .min(sd_seg(x, y, (0.70, 0.46), (0.58, 0.62), w))
            };
            shade(c, &br);
            paint(c, p, &br, Mat::flat(hex(0x7FE0D8), hex(0x16807A), 0.28, 0.64).rim(hex(0x0F5E59)).bevel(0.03));
        }
        Kind::Font => {
            let a = |x: f32, y: f32| {
                let legs = sd_seg(x, y, (0.50, 0.28), (0.33, 0.64), 0.045).min(sd_seg(
                    x,
                    y,
                    (0.50, 0.28),
                    (0.67, 0.64),
                    0.045,
                ));
                legs.min(sd_box(x, y, 0.38, 0.50, 0.62, 0.54, 0.0))
            };
            shade(c, &a);
            paint(c, p, &a, Mat::flat(hex(0x8A8F99), hex(0x2E323A), 0.26, 0.66).rim(hex(0x1F2228)).bevel(0.03));
        }
        Kind::Disk => {
            let disc = |x: f32, y: f32| sd_circle(x, y, 0.50, 0.46, 0.22);
            shade(c, &disc);
            paint(c, p, &disc, Mat::flat(hex(0xF4F6F9), hex(0xA9B2BF), 0.24, 0.68).rim(hex(0x7D8796)));
            // a rainbow sheen across the disc
            tint(c, p, &|x, y| sd_seg(x, y, (0.36, 0.32), (0.64, 0.60), 0.035).max(disc(x, y)), hex(0xB7E3FF), 0.55);
            tint(c, p, &|x, y| sd_seg(x, y, (0.40, 0.30), (0.66, 0.56), 0.015).max(disc(x, y)), hex(0xF4D27A), 0.45);
            paint(
                c,
                p,
                &|x, y| sd_circle(x, y, 0.50, 0.46, 0.065),
                Mat::flat(hex(0xD8DEE6), hex(0x8B96A4), 0.40, 0.52).rim(hex(0x6D7684)),
            );
            paint(
                c,
                p,
                &|x, y| sd_circle(x, y, 0.50, 0.46, 0.028),
                Mat::flat(WHITE, hex(0xE9EDF2), 0.43, 0.49).rim(hex(0x8B96A4)),
            );
        }
        Kind::Web => {
            let globe = |x: f32, y: f32| sd_circle(x, y, 0.50, 0.46, 0.21);
            shade(c, &globe);
            paint(c, p, &globe, Mat::flat(hex(0x79C6F5), hex(0x1C5FA8), 0.25, 0.67).rim(hex(0x144A84)));
            // land
            let land = |x: f32, y: f32| {
                sd_ellipse(x, y, 0.43, 0.40, 0.09, 0.07)
                    .min(sd_ellipse(x, y, 0.57, 0.55, 0.08, 0.10))
                    .min(sd_ellipse(x, y, 0.62, 0.36, 0.05, 0.04))
                    .max(globe(x, y))
            };
            paint(c, p, &land, Mat::flat(hex(0x9BDB6A), hex(0x2F8A26), 0.30, 0.65));
            // the sphere's highlight
            tint(c, p, &|x, y| sd_ellipse(x, y, 0.44, 0.34, 0.11, 0.07), WHITE, 0.35);
        }
    }
}

/// At small sizes the mark is what tells kinds apart, so it is drawn larger about (cx, cy), as Windows' own 16 px
/// icons exaggerate theirs.
fn enlarged(p: P, cx: f32, cy: f32) -> P {
    let k = if p.px > 1.0 / 28.0 {
        0.70
    } else if p.px > 1.0 / 40.0 {
        0.85
    } else {
        1.0
    };
    P { x: cx + (p.x - cx) * k, y: cy + (p.y - cy) * k, px: p.px * k }
}

fn file(c: &mut [f32; 4], p: P, k: Kind) {
    shadow(c, p, &page_shape, 0.015, 0.03, 0.06, 0.28);
    paint(c, p, &page_shape, Mat::flat(WHITE, hex(0xEAEEF3), 0.05, 0.95).rim(hex(0x8E98A7)));
    // the turned-down corner, and its shadow on the page
    let fold = |x: f32, y: f32| sd_poly(x, y, &[(0.63, 0.05), (0.84, 0.26), (0.63, 0.26)]);
    shadow(c, p, &|x, y| fold(x, y).max(page_shape(x, y)), -0.012, 0.018, 0.035, 0.22);
    paint(c, p, &fold, Mat::flat(hex(0xFDFDFE), hex(0xC3CBD6), 0.05, 0.26).rim(hex(0x8E98A7)));
    glyph(c, enlarged(p, 0.50, 0.47), k);
}

fn drive(c: &mut [f32; 4], p: P) {
    shadow(c, p, &|x, y| sd_ellipse(x, y, 0.50, 0.80, 0.46, 0.05), 0.0, 0.0, 0.06, 0.35);
    let top = |x: f32, y: f32| sd_poly(x, y, &[(0.20, 0.30), (0.80, 0.30), (0.90, 0.41), (0.10, 0.41)]) - 0.01;
    paint(c, p, &top, Mat::flat(WHITE, hex(0xCDD3DA), 0.30, 0.41).rim(hex(0x8A929C)));
    let front = |x: f32, y: f32| sd_box(x, y, 0.08, 0.40, 0.92, 0.78, 0.04);
    paint(
        c,
        p,
        &front,
        Mat::flat(hex(0xEEF1F4), hex(0x9AA2AD), 0.40, 0.78).rim(hex(0x666E79)).bevel(0.035).gloss(0.12),
    );
    let panel = |x: f32, y: f32| sd_box(x, y, 0.13, 0.58, 0.87, 0.72, 0.02);
    paint(c, p, &panel, Mat::flat(hex(0x6C747F), hex(0x3E444D), 0.58, 0.72).rim(hex(0x2B3037)));
    for k in 0..6 {
        let gx = 0.18 + 0.06 * k as f32;
        tint(c, p, &|x, y| sd_box(x, y, gx, 0.615, gx + 0.03, 0.685, 0.005), hex(0x2B3037), 0.8);
    }
    paint(
        c,
        p,
        &|x, y| sd_circle(x, y, 0.80, 0.65, 0.032),
        Mat::flat(hex(0xC8FF9B), hex(0x22A41F), 0.62, 0.68).rim(hex(0x166C14)).gloss(0.02),
    );
}

fn removable(c: &mut [f32; 4], p: P) {
    shadow(c, p, &|x, y| sd_ellipse(x, y, 0.50, 0.85, 0.30, 0.045), 0.0, 0.0, 0.05, 0.32);
    let plug = |x: f32, y: f32| sd_box(x, y, 0.36, 0.10, 0.64, 0.34, 0.015);
    paint(c, p, &plug, Mat::flat(hex(0xF4F6F8), hex(0x9AA2AD), 0.10, 0.34).rim(hex(0x6B737E)));
    for hx in [0.42, 0.53] {
        tint(c, p, &|x, y| sd_box(x, y, hx, 0.17, hx + 0.05, 0.22, 0.0), hex(0x4A515B), 1.0);
    }
    let body = |x: f32, y: f32| sd_box(x, y, 0.24, 0.30, 0.76, 0.84, 0.08);
    paint(c, p, &body, Mat::flat(hex(0x7FA6D8), hex(0x24467A), 0.30, 0.84).rim(hex(0x1A3258)).bevel(0.05).gloss(0.2));
    paint(c, p, &|x, y| sd_circle(x, y, 0.50, 0.72, 0.03), Mat::flat(hex(0xC8FF9B), hex(0x22A41F), 0.69, 0.75));
}

fn pc(c: &mut [f32; 4], p: P) {
    shadow(c, p, &|x, y| sd_ellipse(x, y, 0.50, 0.88, 0.30, 0.05), 0.0, 0.0, 0.05, 0.32);
    paint(
        c,
        p,
        &|x, y| sd_ellipse(x, y, 0.50, 0.86, 0.26, 0.06),
        Mat::flat(hex(0xF2F4F6), hex(0x9AA1AB), 0.80, 0.92).rim(hex(0x737A84)),
    );
    paint(
        c,
        p,
        &|x, y| sd_poly(x, y, &[(0.44, 0.66), (0.56, 0.66), (0.58, 0.85), (0.42, 0.85)]),
        Mat::flat(hex(0xD5DAE0), hex(0x8D949E), 0.66, 0.85).rim(hex(0x737A84)),
    );
    let bezel = |x: f32, y: f32| sd_box(x, y, 0.05, 0.08, 0.95, 0.70, 0.04);
    shadow(c, p, &bezel, 0.01, 0.025, 0.05, 0.25);
    paint(c, p, &bezel, Mat::flat(hex(0xF5F7F9), hex(0xA9B0BA), 0.08, 0.70).rim(hex(0x666E79)).bevel(0.035));
    let screen = |x: f32, y: f32| sd_box(x, y, 0.11, 0.14, 0.89, 0.63, 0.01);
    paint(c, p, &screen, Mat::flat(hex(0x3F86E6), hex(0x061F66), 0.14, 0.63).rim(hex(0x0D1B3D)));
    // Aero's reflection across the glass
    tint(
        c,
        p,
        &|x, y| sd_poly(x, y, &[(0.11, 0.14), (0.66, 0.14), (0.36, 0.63), (0.11, 0.63)]).max(screen(x, y)),
        WHITE,
        0.2,
    );
    tint(c, p, &|x, y| sd_box(x, y, 0.12, 0.145, 0.88, 0.17, 0.0), WHITE, 0.25);
}

fn star(c: &mut [f32; 4], p: P) {
    let mut pts = Vec::with_capacity(10);
    for i in 0..10 {
        let r = if i % 2 == 0 { 0.44 } else { 0.19 };
        let a = std::f32::consts::PI * (i as f32 / 5.0) - std::f32::consts::FRAC_PI_2;
        pts.push((0.5 + r * a.cos(), 0.54 + r * a.sin()));
    }
    let s = move |x: f32, y: f32| sd_poly(x, y, &pts) - 0.02;
    shadow(c, p, &s, 0.01, 0.03, 0.05, 0.3);
    paint(c, p, &s, Mat::flat(hex(0xFFF0A0), hex(0xEE9A0C), 0.08, 0.96).rim(hex(0xA9660A)).bevel(0.07).gloss(0.30));
}

/// a shortcut's arrow: a small white card with a glossy blue arrow bending up and to the right
/// a shortcut's arrow, flat as Windows 10's: a white square with a grey edge and a blue arrow bending up and right
fn arrow(c: &mut [f32; 4], p: P) {
    let card = |x: f32, y: f32| sd_box(x, y, 0.04, 0.04, 0.96, 0.96, 0.06);
    paint(c, p, &card, Mat::flat(WHITE, WHITE, 0.0, 1.0).rim(hex(0x8C8C8C)));
    let shaft = |x: f32, y: f32| {
        sd_seg(x, y, (0.28, 0.80), (0.32, 0.58), 0.085).min(sd_seg(x, y, (0.32, 0.58), (0.54, 0.43), 0.085))
    };
    let head = |x: f32, y: f32| sd_poly(x, y, &[(0.46, 0.22), (0.82, 0.22), (0.76, 0.58)]) - 0.01;
    tint(c, p, &|x, y| shaft(x, y).min(head(x, y)), hex(0x1F7AD8), 1.0);
}

// ---------------------------------------------------------------------------------------------------------------
// the toolbar's icons, in the same light: glossy objects for the commands (scissors, a clipboard, a pencil, a red
// cross). The navigation arrows are thin lines in the text's colour, as Windows 10 draws them (the marks below).

/// an arc of a ring about (cx, cy), from angle a0 clockwise to a1 (radians, 0 to the right, y down), `w` thick
fn sd_arc(x: f32, y: f32, cx: f32, cy: f32, r: f32, w: f32, a0: f32, a1: f32) -> f32 {
    let (dx, dy) = (x - cx, y - cy);
    let tau = std::f32::consts::TAU;
    let ang = dy.atan2(dx);
    let span = (a1 - a0).rem_euclid(tau);
    if (ang - a0).rem_euclid(tau) <= span {
        ((dx * dx + dy * dy).sqrt() - r).abs() - w
    } else {
        let end = |a: f32| sd_circle(x, y, cx + r * a.cos(), cy + r * a.sin(), w);
        end(a0).min(end(a1))
    }
}

const GREEN: (u32, u32, u32) = (0xA8EC74, 0x2C8A1C, 0x1D6512);

fn glossy(top: u32, bottom: u32, rim: u32, y0: f32, y1: f32) -> Mat {
    Mat::flat(hex(top), hex(bottom), y0, y1).rim(hex(rim)).bevel(0.07).gloss((y1 - y0) * 0.45)
}

fn drop_shadow(c: &mut [f32; 4], p: P, f: &dyn Fn(f32, f32) -> f32) {
    shadow(c, p, f, 0.0, 0.035, 0.06, 0.32);
}

fn magnifier(c: &mut [f32; 4], p: P) {
    let lens = |x: f32, y: f32| sd_circle(x, y, 0.41, 0.41, 0.30);
    let handle = |x: f32, y: f32| sd_seg(x, y, (0.62, 0.62), (0.86, 0.86), 0.085);
    drop_shadow(c, p, &|x, y| lens(x, y).min(handle(x, y)));
    paint(c, p, &handle, Mat::flat(hex(0x6B7380), hex(0x1E232B), 0.58, 0.92).rim(hex(0x14181E)).bevel(0.05));
    paint(c, p, &lens, Mat::flat(hex(0xF4F6F9), hex(0x8B95A3), 0.11, 0.71).rim(hex(0x56606D)).bevel(0.06));
    let glass = |x: f32, y: f32| sd_circle(x, y, 0.41, 0.41, 0.215);
    paint(c, p, &glass, Mat::flat(hex(0xD8F1FF), hex(0x4F9BDA), 0.19, 0.63).rim(hex(0x3D6F9E)).gloss(0.20));
    tint(c, p, &|x, y| sd_ellipse(x, y, 0.35, 0.31, 0.11, 0.06), WHITE, 0.6);
}

fn sheet(c: &mut [f32; 4], p: P, x0: f32, y0: f32, x1: f32, y1: f32, lines: bool) {
    let s = move |x: f32, y: f32| sd_box(x, y, x0, y0, x1, y1, 0.035);
    shadow(c, p, &s, 0.01, 0.025, 0.045, 0.30);
    paint(c, p, &s, Mat::flat(WHITE, hex(0xE6EBF1), y0, y1).rim(hex(0x7F8A9A)));
    if lines {
        let n = ((y1 - y0) / 0.13) as i32 - 1;
        for k in 0..n.max(1) {
            let ly = y0 + 0.13 + 0.12 * k as f32;
            let end = if k == n - 1 { x0 + (x1 - x0) * 0.55 } else { x1 - 0.10 };
            tint(c, p, &move |x, y| sd_box(x, y, x0 + 0.10, ly, end, ly + 0.045, 0.02), hex(0x4F86D6), 1.0);
        }
    }
}

/// an error: a red disc with a white cross
fn stop(c: &mut [f32; 4], p: P) {
    let o = |x: f32, y: f32| sd_circle(x, y, 0.5, 0.5, 0.42);
    drop_shadow(c, p, &o);
    paint(c, p, &o, Mat::flat(hex(0xF26B63), hex(0xC0211A), 0.08, 0.92).rim(hex(0x8C1510)).bevel(0.08).gloss(0.38));
    let x = |x: f32, y: f32| {
        sd_seg(x, y, (0.35, 0.35), (0.65, 0.65), 0.06).min(sd_seg(x, y, (0.65, 0.35), (0.35, 0.65), 0.06))
    };
    tint(c, p, &x, WHITE, 1.0);
}

fn info(c: &mut [f32; 4], p: P) {
    let o = |x: f32, y: f32| sd_circle(x, y, 0.5, 0.5, 0.42);
    drop_shadow(c, p, &o);
    paint(c, p, &o, Mat::flat(hex(0x8CCBFA), hex(0x1453B2), 0.08, 0.92).rim(hex(0x0C3A80)).bevel(0.08).gloss(0.38));
    let i = |x: f32, y: f32| sd_circle(x, y, 0.5, 0.29, 0.075).min(sd_box(x, y, 0.425, 0.42, 0.575, 0.76, 0.03));
    shadow(c, p, &i, 0.0, 0.02, 0.03, 0.35);
    paint(c, p, &i, Mat::flat(WHITE, hex(0xDCE8F5), 0.22, 0.76));
}

fn warning(c: &mut [f32; 4], p: P) {
    let tri = |x: f32, y: f32| sd_poly(x, y, &[(0.5, 0.10), (0.92, 0.86), (0.08, 0.86)]) - 0.04;
    drop_shadow(c, p, &tri);
    paint(c, p, &tri, Mat::flat(hex(0xFFEE9A), hex(0xE9A20F), 0.06, 0.90).rim(hex(0x9A6400)).bevel(0.07).gloss(0.30));
    let mark = |x: f32, y: f32| sd_box(x, y, 0.455, 0.34, 0.545, 0.62, 0.035).min(sd_circle(x, y, 0.5, 0.73, 0.055));
    tint(c, p, &mark, hex(0x2A2208), 1.0);
}

fn tick(c: &mut [f32; 4], p: P) {
    let t = |x: f32, y: f32| {
        sd_seg(x, y, (0.16, 0.52), (0.40, 0.76), 0.10).min(sd_seg(x, y, (0.40, 0.76), (0.84, 0.24), 0.10))
    };
    drop_shadow(c, p, &t);
    paint(c, p, &t, glossy(GREEN.0, GREEN.1, GREEN.2, 0.14, 0.86));
}

fn tool(c: &mut [f32; 4], p: P, t: Tool) {
    match t {
        Tool::Search => magnifier(c, p),
        Tool::Copy => {
            sheet(c, p, 0.10, 0.06, 0.62, 0.68, true);
            sheet(c, p, 0.38, 0.32, 0.90, 0.94, true);
        }
        Tool::Props => info(c, p),
        Tool::Stop => stop(c, p),
        Tool::Warning => warning(c, p),
        Tool::Tick => tick(c, p),
    }
}

/// The small marks that are drawn in the text's colour (chevrons, a check, the close cross): plain lines, as thin as
/// text at any size, made from the same distances and tinted when drawn.
fn mark_cover(m: Mark, x: f32, y: f32, px: f32) -> f32 {
    // strokes a little over a pixel wide whatever the size, like the text beside them; the line icons (arrows, sort,
    // views) about a pixel at 16 px as Windows 10's, growing with the screen's scale
    let thin = matches!(
        m,
        Mark::Back
            | Mark::Forward
            | Mark::Rise
            | Mark::Refresh
            | Mark::Sort
            | Mark::Grid
            | Mark::List
            | Mark::Cut
            | Mark::Copy
            | Mark::Paste
            | Mark::Rename
            | Mark::Delete
            | Mark::Info
    );
    let w = if thin { (px * 0.55).max(0.032) } else { (px * 0.62).max(0.045) };
    let line = |pts: &[(f32, f32)]| pts.windows(2).map(|s| sd_seg(x, y, s[0], s[1], w)).fold(f32::MAX, f32::min);
    let frame = |x0: f32, y0: f32, x1: f32, y1: f32| sd_box(x, y, x0, y0, x1, y1, 0.04).abs() - w;
    let d = match m {
        // one arrow up, one down
        Mark::Sort => line(&[(0.31, 0.84), (0.31, 0.16)])
            .min(line(&[(0.13, 0.34), (0.31, 0.16), (0.49, 0.34)]))
            .min(line(&[(0.69, 0.16), (0.69, 0.84)]))
            .min(line(&[(0.51, 0.66), (0.69, 0.84), (0.87, 0.66)])),
        Mark::Grid => frame(0.15, 0.15, 0.43, 0.43)
            .min(frame(0.57, 0.15, 0.85, 0.43))
            .min(frame(0.15, 0.57, 0.43, 0.85))
            .min(frame(0.57, 0.57, 0.85, 0.85)),
        // a small solid square for each row's icon, a line for its name
        Mark::List => [0.25f32, 0.5, 0.75]
            .iter()
            .map(|&ly| sd_box(x, y, 0.13, ly - 0.055, 0.24, ly + 0.055, 0.01).min(line(&[(0.36, ly), (0.88, ly)])))
            .fold(f32::MAX, f32::min),
        Mark::Back => line(&[(0.14, 0.5), (0.86, 0.5)]).min(line(&[(0.44, 0.2), (0.14, 0.5), (0.44, 0.8)])),
        Mark::Forward => line(&[(0.14, 0.5), (0.86, 0.5)]).min(line(&[(0.56, 0.2), (0.86, 0.5), (0.56, 0.8)])),
        Mark::Rise => line(&[(0.5, 0.14), (0.5, 0.86)]).min(line(&[(0.2, 0.44), (0.5, 0.14), (0.8, 0.44)])),
        Mark::Refresh => {
            // a ring running clockwise from the right round to the top, its head pointing on
            let (r, a0, a1) = (0.32f32, -0.25f32, -1.45f32);
            let (tx, ty) = (0.5 + r * a1.cos(), 0.5 + r * a1.sin());
            let (dx, dy) = (-a1.sin(), a1.cos());
            let (nx, ny) = (a1.cos(), a1.sin());
            let h = 0.17;
            let head = line(&[
                (tx - dx * h + nx * h, ty - dy * h + ny * h),
                (tx, ty),
                (tx - dx * h - nx * h, ty - dy * h - ny * h),
            ]);
            sd_arc(x, y, 0.5, 0.5, r, w, a0, a1).min(head)
        }
        Mark::Down => line(&[(0.22, 0.36), (0.5, 0.64), (0.78, 0.36)]),
        Mark::Up => line(&[(0.22, 0.64), (0.5, 0.36), (0.78, 0.64)]),
        Mark::Right => line(&[(0.36, 0.22), (0.64, 0.5), (0.36, 0.78)]),
        Mark::Close => line(&[(0.2, 0.2), (0.8, 0.8)]).min(line(&[(0.8, 0.2), (0.2, 0.8)])),
        Mark::Add => line(&[(0.5, 0.14), (0.5, 0.86)]).min(line(&[(0.14, 0.5), (0.86, 0.5)])),
        Mark::Check => line(&[(0.16, 0.52), (0.40, 0.76), (0.86, 0.26)]),
        Mark::More => [0.2f32, 0.5, 0.8].iter().map(|&cx| sd_circle(x, y, cx, 0.5, 0.075)).fold(f32::MAX, f32::min),
        Mark::Pause => sd_box(x, y, 0.24, 0.16, 0.40, 0.84, 0.02).min(sd_box(x, y, 0.60, 0.16, 0.76, 0.84, 0.02)),
        Mark::Play => sd_poly(x, y, &[(0.26, 0.14), (0.82, 0.5), (0.26, 0.86)]) - 0.02,
        // two rings for the handles, two blades crossing above them
        Mark::Cut => (sd_circle(x, y, 0.27, 0.76, 0.13).abs() - w)
            .min(sd_circle(x, y, 0.73, 0.76, 0.13).abs() - w)
            .min(line(&[(0.35, 0.65), (0.70, 0.10)]))
            .min(line(&[(0.65, 0.65), (0.30, 0.10)])),
        // a sheet in front of another, the one behind showing at the top and the right
        Mark::Copy => frame(0.14, 0.30, 0.68, 0.90).min(line(&[
            (0.32, 0.30),
            (0.32, 0.12),
            (0.86, 0.12),
            (0.86, 0.70),
            (0.68, 0.70),
        ])),
        // a clipboard with its clip
        Mark::Paste => line(&[(0.34, 0.18), (0.18, 0.18), (0.18, 0.90), (0.82, 0.90), (0.82, 0.18), (0.66, 0.18)])
            .min(frame(0.34, 0.08, 0.66, 0.26)),
        // a text box and the typing cursor at its end
        Mark::Rename => line(&[(0.68, 0.30), (0.10, 0.30), (0.10, 0.70), (0.68, 0.70)])
            .min(line(&[(0.88, 0.30), (0.92, 0.30), (0.92, 0.70), (0.88, 0.70)]))
            .min(line(&[(0.78, 0.14), (0.78, 0.86)]))
            .min(line(&[(0.69, 0.14), (0.87, 0.14)]))
            .min(line(&[(0.69, 0.86), (0.87, 0.86)])),
        // a bin: lid, handle, body narrowing to its foot, two ribs
        Mark::Delete => line(&[(0.12, 0.24), (0.88, 0.24)])
            .min(line(&[(0.38, 0.24), (0.40, 0.11), (0.60, 0.11), (0.62, 0.24)]))
            .min(line(&[(0.22, 0.24), (0.28, 0.89), (0.72, 0.89), (0.78, 0.24)]))
            .min(line(&[(0.42, 0.40), (0.43, 0.73)]))
            .min(line(&[(0.58, 0.40), (0.57, 0.73)])),
        // an "i" in a ring
        Mark::Info => (sd_circle(x, y, 0.5, 0.5, 0.40).abs() - w)
            .min(sd_circle(x, y, 0.5, 0.31, w * 1.5))
            .min(line(&[(0.5, 0.45), (0.5, 0.72)])),
    };
    cover(d, px)
}

/// a mark of `size` pixels in colour `c`
pub fn mark(m: Mark, size: i32, c: crate::ui::gfx::Rgb) -> Image {
    let n = size.clamp(4, 256);
    let px = 1.0 / n as f32;
    let mut out = Vec::with_capacity((n * n) as usize);
    for py in 0..n {
        for pxi in 0..n {
            let a = mark_cover(m, (pxi as f32 + 0.5) * px, (py as f32 + 0.5) * px, px).clamp(0.0, 1.0);
            let q = |v: u8| (v as f32 * a).round() as u32;
            out.push(((a * 255.0).round() as u32) << 24 | q(c.0) << 16 | q(c.1) << 8 | q(c.2));
        }
    }
    Image { w: n, h: n, px: out.into_boxed_slice() }
}

/// an icon made to show a state: lit while the pointer is on it
fn restyle(img: &mut Image, look: Look) {
    for p in img.px.iter_mut() {
        let a = *p >> 24;
        if a == 0 {
            continue;
        }
        let (r, g, b) = ((*p >> 16 & 0xFF) as f32, (*p >> 8 & 0xFF) as f32, (*p & 0xFF) as f32);
        let (r, g, b, a) = match look {
            Look::Normal => return,
            // towards white, by premultiplied amounts
            Look::Hot => {
                let af = a as f32;
                (r + (af - r) * 0.18, g + (af - g) * 0.18, b + (af - b) * 0.18, a)
            }
        };
        let q = |v: f32| v.round().clamp(0.0, 255.0) as u32;
        *p = a.min(255) << 24 | q(r) << 16 | q(g) << 8 | q(b);
    }
}

fn sample(art: Art, x: f32, y: f32, px: f32) -> [f32; 4] {
    let mut c = [0.0f32; 4];
    let p = P { x, y, px };
    match art {
        Art::Folder => folder(&mut c, p),
        Art::Special(s) => {
            folder(&mut c, p);
            emblem(&mut c, enlarged(p, 0.50, 0.66), s);
        }
        Art::Drive => drive(&mut c, p),
        Art::Removable => removable(&mut c, p),
        Art::Pc => pc(&mut c, p),
        Art::Star => star(&mut c, p),
        Art::File(k) => file(&mut c, p, k),
        Art::Arrow => arrow(&mut c, p),
        Art::Tool(t) => tool(&mut c, p, t),
    }
    c
}

/// Draws an icon: one sample per pixel, at its centre, with edges smoothed from the distance to them.
pub fn render(art: Art, size: i32, _t: &Theme) -> Image {
    let n = size.clamp(8, 512);
    let px = 1.0 / n as f32;
    let mut out = Vec::with_capacity((n * n) as usize);
    for py in 0..n {
        for pxi in 0..n {
            let c = sample(art, (pxi as f32 + 0.5) * px, (py as f32 + 0.5) * px, px);
            let a = c[3].clamp(0.0, 1.0);
            // premultiplied BGRA
            let q = |v: f32| (v * a * 255.0).round().clamp(0.0, 255.0) as u32;
            out.push(((a * 255.0).round() as u32) << 24 | q(c[0]) << 16 | q(c[1]) << 8 | q(c[2]));
        }
    }
    Image { w: n, h: n, px: out.into_boxed_slice() }
}

// ---------------------------------------------------------------------------------------------------------------
// drawn ahead

#[derive(Clone, Copy, PartialEq)]
enum Ahead {
    Art(Art, i32, Look),
    Mark(Mark, i32, crate::ui::gfx::Rgb),
}

// icons drawn on a spare thread for the first frame, taken by the cache when it first wants them
static AHEAD: std::sync::Mutex<Vec<(Ahead, Image)>> = std::sync::Mutex::new(Vec::new());

fn take_ahead(k: Ahead) -> Option<Image> {
    let mut v = AHEAD.lock().ok()?;
    let i = v.iter().position(|(x, _)| *x == k)?;
    Some(v.swap_remove(i).1)
}

/// Draws what the first frame shows (the toolbar's icons and marks, the navigation pane's, folders and the commonest
/// files) on a spare thread while the window is being made, in the order the frame is painted. Whatever is not
/// ready by then is drawn when wanted, as before.
pub fn draw_ahead(scale: f32, t: Theme) {
    std::thread::spawn(move || {
        let s = |v: f32| (v * scale).round() as i32;
        let mut keys = Vec::new();
        for m in [Mark::Back, Mark::Forward, Mark::Rise] {
            keys.push(Ahead::Mark(m, s(16.0), t.fg));
            keys.push(Ahead::Mark(m, s(16.0), t.faint));
        }
        for c in [t.muted, t.faint, t.primary_fg] {
            keys.push(Ahead::Mark(Mark::Down, s(12.0), c));
        }
        keys.push(Ahead::Mark(Mark::Right, s(12.0), t.faint));
        keys.push(Ahead::Mark(Mark::Refresh, s(14.0), t.muted));
        keys.push(Ahead::Art(Art::Tool(Tool::Search), s(16.0), Look::Normal));
        for m in [
            Mark::Add,
            Mark::Cut,
            Mark::Copy,
            Mark::Paste,
            Mark::Rename,
            Mark::Delete,
            Mark::Sort,
            Mark::Grid,
            Mark::Info,
        ] {
            for c in [t.fg, t.faint] {
                keys.push(Ahead::Mark(m, s(16.0), c));
            }
        }
        keys.push(Ahead::Mark(Mark::Add, s(16.0), t.primary_fg));
        keys.push(Ahead::Mark(Mark::More, s(16.0), t.fg));
        for c in [t.accent, t.muted, t.faint] {
            keys.push(Ahead::Mark(Mark::List, s(16.0), c));
            keys.push(Ahead::Mark(Mark::Grid, s(16.0), c));
        }
        let small = s(16.0);
        for a in [Art::Star, Art::Pc, Art::Drive, Art::Folder]
            .into_iter()
            .chain(
                [
                    Special::Desktop,
                    Special::Downloads,
                    Special::Documents,
                    Special::Pictures,
                    Special::Music,
                    Special::Videos,
                ]
                .map(Art::Special),
            )
            .chain([Kind::Generic, Kind::App, Kind::Text].map(Art::File))
        {
            keys.push(Ahead::Art(a, small, Look::Normal));
        }
        keys.push(Ahead::Mark(Mark::Up, s(10.0), t.muted));
        keys.push(Ahead::Mark(Mark::Down, s(10.0), t.muted));
        for k in keys {
            let img = match k {
                Ahead::Art(a, size, look) => {
                    let mut img = render(a, size, &t);
                    restyle(&mut img, look);
                    img
                }
                Ahead::Mark(m, size, c) => mark(m, size, c),
            };
            if let Ok(mut v) = AHEAD.lock() {
                v.push((k, img));
            }
        }
    });
}

/// The icons drawn so far, by what and size. Small: a 20 px icon is 1.6 KB, the 96 px ones of Large icons 36 KB,
/// and a folder has only a handful of kinds.
#[derive(Default)]
pub struct Cache {
    map: HashMap<(Art, i32, Look), Rc<Image>>,
    marks: HashMap<(Mark, i32, crate::ui::gfx::Rgb), Rc<Image>>,
}

impl Cache {
    pub fn get(&mut self, art: Art, size: i32, t: &Theme) -> Rc<Image> {
        self.look(art, size, Look::Normal, t)
    }

    pub fn look(&mut self, art: Art, size: i32, look: Look, t: &Theme) -> Rc<Image> {
        self.map
            .entry((art, size, look))
            .or_insert_with(|| {
                Rc::new(take_ahead(Ahead::Art(art, size, look)).unwrap_or_else(|| {
                    let mut img = render(art, size, t);
                    restyle(&mut img, look);
                    img
                }))
            })
            .clone()
    }

    pub fn mark(&mut self, m: Mark, size: i32, c: crate::ui::gfx::Rgb) -> Rc<Image> {
        self.marks
            .entry((m, size, c))
            .or_insert_with(|| Rc::new(take_ahead(Ahead::Mark(m, size, c)).unwrap_or_else(|| mark(m, size, c))))
            .clone()
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.marks.clear();
        if let Ok(mut v) = AHEAD.lock() {
            *v = Vec::new();
        }
    }
}

/// the art for a folder the user knows by name (Desktop, Downloads, ...)
pub fn special_for(label: &str) -> Option<Special> {
    Some(match label {
        "Desktop" => Special::Desktop,
        "Downloads" => Special::Downloads,
        "Documents" => Special::Documents,
        "Pictures" => Special::Pictures,
        "Music" => Special::Music,
        "Videos" => Special::Videos,
        _ => return None,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn icons_cover_what_they_should() {
        let t = Theme::dark();
        for art in [Art::Folder, Art::Special(Special::Downloads), Art::Drive, Art::Pc, Art::Star, Art::File(Kind::Pdf)]
        {
            let img = render(art, 20, &t);
            let opaque = img.px.iter().filter(|p| *p >> 24 >= 250).count();
            let clear = img.px.iter().filter(|p| *p >> 24 == 0).count();
            // every icon is mostly solid shape with clear space around it, never empty or a full square
            assert!(opaque > 60, "{art:?} has {opaque} solid pixels");
            assert!(clear > 20, "{art:?} has {clear} clear pixels");
        }
        // the corners stay clear (no box around icons)
        let f = render(Art::Folder, 32, &t);
        assert_eq!(f.px[0] >> 24, 0);
        assert_eq!(f.px[31] >> 24, 0);
    }

    #[test]
    fn kinds() {
        assert_eq!(kind_of("pdf"), Kind::Pdf);
        assert_eq!(kind_of("jpeg"), Kind::Image);
        assert_eq!(kind_of("weird"), Kind::Generic);
        assert_eq!(band_label("zip").as_deref(), Some("ZIP"));
        assert_eq!(band_label("tar.gz"), None);
    }

    /// DVFILES_ARTSHEET=<file.png> writes every icon at 96, 48 and 20 px on dark and light, to look at
    #[test]
    fn sheet() {
        let Some(out) = std::env::var_os("DVFILES_ARTSHEET") else { return };
        let arts: Vec<Art> = [Art::Folder, Art::Pc, Art::Drive, Art::Removable, Art::Star, Art::Arrow]
            .into_iter()
            .chain(
                [
                    Special::Desktop,
                    Special::Downloads,
                    Special::Documents,
                    Special::Pictures,
                    Special::Music,
                    Special::Videos,
                ]
                .map(Art::Special),
            )
            .chain(
                [
                    Kind::Generic,
                    Kind::Text,
                    Kind::Code,
                    Kind::Image,
                    Kind::Video,
                    Kind::Audio,
                    Kind::Archive,
                    Kind::Pdf,
                    Kind::Doc,
                    Kind::Sheet,
                    Kind::Slides,
                    Kind::App,
                    Kind::Font,
                    Kind::Disk,
                    Kind::Web,
                ]
                .map(Art::File),
            )
            .collect();
        let sizes = [96, 48, 20];
        let (per_row, cell, row_h) = (13usize, 104usize, 96 + 48 + 20 + 34);
        let rows = arts.len().div_ceil(per_row);
        let band = rows * row_h;
        let (w, h) = (per_row * cell, band * 2);
        let mut px = vec![[0u8; 3]; w * h];
        let t = Theme::dark();
        for half in 0..2 {
            let bg = if half == 0 { [0x14, 0x14, 0x14] } else { [0xFF, 0xFF, 0xFF] };
            for y in half * band..(half + 1) * band {
                for x in 0..w {
                    px[y * w + x] = bg;
                }
            }
            for (i, &art) in arts.iter().enumerate() {
                let (col, row) = (i % per_row, i / per_row);
                let mut y0 = half * band + row * row_h + 4;
                for &s in &sizes {
                    let img = render(art, s, &t);
                    let x0 = col * cell + (cell - s as usize) / 2;
                    for yy in 0..s as usize {
                        for xx in 0..s as usize {
                            let p = img.px[yy * s as usize + xx];
                            let a = (p >> 24) as f32 / 255.0;
                            let d = &mut px[(y0 + yy) * w + x0 + xx];
                            for (k, sh) in [16u32, 8, 0].iter().enumerate() {
                                d[k] = ((p >> sh & 0xFF) as f32 + d[k] as f32 * (1.0 - a)).round().min(255.0) as u8;
                            }
                        }
                    }
                    y0 += s as usize + 10;
                }
            }
        }
        std::fs::write(out, png(w, h, &px)).unwrap();
    }

    /// DVFILES_TOOLSHEET=<file.png>: the toolbar's icons at the sizes they are drawn, normal, lit and off, and the marks
    #[test]
    fn tool_sheet() {
        let Some(out) = std::env::var_os("DVFILES_TOOLSHEET") else { return };
        let tools = [Tool::Search, Tool::Copy, Tool::Props, Tool::Warning, Tool::Stop, Tool::Tick];
        let marks = [
            Mark::Back,
            Mark::Forward,
            Mark::Rise,
            Mark::Refresh,
            Mark::Sort,
            Mark::Grid,
            Mark::List,
            Mark::Down,
            Mark::Up,
            Mark::Right,
            Mark::Close,
            Mark::Add,
            Mark::Check,
            Mark::More,
            Mark::Pause,
            Mark::Play,
            Mark::Cut,
            Mark::Copy,
            Mark::Paste,
            Mark::Rename,
            Mark::Delete,
            Mark::Info,
        ];
        let sizes = [64, 39, 26, 20, 18, 16];
        let cell = 72usize;
        let row_h: usize = sizes.iter().map(|s| *s as usize + 8).sum::<usize>() + 3 * 30 + 16;
        let (w, band) = (tools.len().max(marks.len()) * cell, row_h);
        let h = band * 2;
        let mut px = vec![[0u8; 3]; w * h];
        let t = Theme::dark();
        let put = |px: &mut Vec<[u8; 3]>, img: &Image, x0: usize, y0: usize| {
            for yy in 0..img.h as usize {
                for xx in 0..img.w as usize {
                    let p = img.px[yy * img.w as usize + xx];
                    let a = (p >> 24) as f32 / 255.0;
                    let d = &mut px[(y0 + yy) * w + x0 + xx];
                    for (k, sh) in [16u32, 8, 0].iter().enumerate() {
                        d[k] = ((p >> sh & 0xFF) as f32 + d[k] as f32 * (1.0 - a)).round().min(255.0) as u8;
                    }
                }
            }
        };
        for half in 0..2 {
            let bg = if half == 0 { [0x20, 0x20, 0x20] } else { [0xF3, 0xF3, 0xF3] };
            for y in half * band..(half + 1) * band {
                for x in 0..w {
                    px[y * w + x] = bg;
                }
            }
            for (i, &tl) in tools.iter().enumerate() {
                let mut y0 = half * band + 4;
                for &s in &sizes {
                    let img = render(Art::Tool(tl), s, &t);
                    put(&mut px, &img, i * cell + (cell - s as usize) / 2, y0);
                    y0 += s as usize + 8;
                }
                for look in [Look::Hot] {
                    let mut img = render(Art::Tool(tl), 26, &t);
                    restyle(&mut img, look);
                    put(&mut px, &img, i * cell + (cell - 26) / 2, y0);
                    y0 += 30;
                }
            }
            let y0 = half * band + band - 30;
            let ink = if half == 0 { crate::ui::gfx::Rgb::hex(0xE0E0E0) } else { crate::ui::gfx::Rgb::hex(0x202020) };
            for (i, &m) in marks.iter().enumerate() {
                for (k, s) in [12, 16, 24].iter().enumerate() {
                    let img = mark(m, *s, ink);
                    put(&mut px, &img, i * cell + 4 + k * 22, y0);
                }
            }
        }
        std::fs::write(out, png(w, h, &px)).unwrap();
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut c = 0xFFFF_FFFFu32;
        for &b in data {
            c ^= b as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
        }
        !c
    }

    /// a PNG with stored (uncompressed) deflate blocks: enough for a look
    pub(crate) fn png(w: usize, h: usize, px: &[[u8; 3]]) -> Vec<u8> {
        let mut raw = Vec::with_capacity(h * (w * 3 + 1));
        for y in 0..h {
            raw.push(0);
            for x in 0..w {
                raw.extend_from_slice(&px[y * w + x]);
            }
        }
        let mut z = vec![0x78, 0x01];
        let chunks: Vec<&[u8]> = raw.chunks(65535).collect();
        for (k, chunk) in chunks.iter().enumerate() {
            z.push((k + 1 == chunks.len()) as u8);
            z.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
            z.extend_from_slice(&(!(chunk.len() as u16)).to_le_bytes());
            z.extend_from_slice(chunk);
        }
        let (mut a, mut b) = (1u32, 0u32);
        for &x in &raw {
            a = (a + x as u32) % 65521;
            b = (b + a) % 65521;
        }
        z.extend_from_slice(&(b << 16 | a).to_be_bytes());
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut chunk = |kind: &[u8], data: &[u8]| {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            let mut c = kind.to_vec();
            c.extend_from_slice(data);
            out.extend_from_slice(&c);
            out.extend_from_slice(&crc32(&c).to_be_bytes());
        };
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&(w as u32).to_be_bytes());
        ihdr.extend_from_slice(&(h as u32).to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        chunk(b"IHDR", &ihdr);
        chunk(b"IDAT", &z);
        chunk(b"IEND", &[]);
        out
    }
}
