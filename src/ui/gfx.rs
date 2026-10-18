// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Drawing: each window paints into its own 32-bit DIB and copies it to the screen in one BitBlt. Rectangles, rounded
//! shapes and charts are written straight into the pixels; text goes through GDI's DrawTextW with ClearType Segoe UI,
//! which is what Explorer's own list uses. Every icon, the toolbar's too, is dvfiles' own drawing (art.rs), so
//! there are no image files and no icon font.
use crate::settings::ThemeMode;
use crate::ui::wstr;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HWND, RECT, SIZE};
use windows::Win32::Graphics::Gdi::*;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const fn hex(v: u32) -> Rgb {
        Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
    }

    // a 32-bit DIB pixel: blue in the low byte
    fn pixel(self) -> u32 {
        (self.0 as u32) << 16 | (self.1 as u32) << 8 | self.2 as u32
    }

    pub fn colorref(self) -> COLORREF {
        COLORREF(self.0 as u32 | (self.1 as u32) << 8 | (self.2 as u32) << 16)
    }

    pub fn mix(self, o: Rgb, t: f32) -> Rgb {
        let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round().clamp(0.0, 255.0) as u8;
        Rgb(m(self.0, o.0), m(self.1, o.1), m(self.2, o.2))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct R {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl R {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> R {
        R { x, y, w, h }
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    pub fn inset(&self, dx: i32, dy: i32) -> R {
        R::new(self.x + dx, self.y + dy, (self.w - 2 * dx).max(0), (self.h - 2 * dy).max(0))
    }

    pub fn rect(&self) -> RECT {
        RECT { left: self.x, top: self.y, right: self.right(), bottom: self.bottom() }
    }

    pub fn intersects(&self, o: &R) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }
}

/// dvfiles' look: a frame (title bar, toolbar, sidebar, status bar), glass when that is on, and on it the content as
/// a raised card with rounded corners. Controls are rounded too; hover is a quiet neutral, and the user's Windows
/// accent colour marks what is chosen: the selection's tint, the primary button, focus. In dark the accent is a lighter
/// shade of it, as Windows 11 uses it on dark surfaces.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub dark: bool,
    // title bar, toolbar, sidebar, status bar
    pub chrome: Rgb,
    // the card the list sits on, and dialogs' body
    pub bg: Rgb,
    pub card_border: Rgb,
    // the card's top edge, a touch lighter: light falls on it from above
    pub card_edge: Rgb,
    pub fg: Rgb,
    pub muted: Rgb,
    pub faint: Rgb,
    pub hover: Rgb,
    // hover on the chrome surface
    pub chrome_hover: Rgb,
    pub sel: Rgb,
    pub sel_border: Rgb,
    pub sel_inactive: Rgb,
    pub header_fg: Rgb,
    pub line: Rgb,
    pub bar_bg: Rgb,
    pub input_bg: Rgb,
    pub input_border: Rgb,
    pub input_hot: Rgb,
    pub btn_hover: Rgb,
    pub btn_press: Rgb,
    pub accent: Rgb,
    // the one filled button (New, a dialog's default button): the accent in light, a soft gray in dark, where a
    // near-white fill would glare against the dark surfaces
    pub primary: Rgb,
    pub primary_hot: Rgb,
    pub primary_fg: Rgb,
    pub danger: Rgb,
    pub thumb: Rgb,
    // the window's 1px outer border
    pub frame: Rgb,
    // Windows 10's progress green (address bar while loading, the copy graph), whatever the accent
    pub progress: Rgb,
    pub progress_fill: Rgb,
}

impl Theme {
    /// Windows' accent colour (Settings > Personalization > Colors), or Windows 10's blue
    pub fn system_accent() -> Rgb {
        use winreg::enums::HKEY_CURRENT_USER;
        let v: Option<u32> = winreg::RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(r"Software\Microsoft\Windows\DWM")
            .and_then(|k| k.get_value("AccentColor"))
            .ok();
        // stored as 0xAABBGGRR
        v.map_or(Rgb::hex(0x0078D7), |c| Rgb(c as u8, (c >> 8) as u8, (c >> 16) as u8))
    }

    /// the accent made light enough to read on dark surfaces and dark enough to read on light ones: Windows 11's
    /// "light 2" and "dark 1" shades, near enough
    fn accent_for(accent: Rgb, dark: bool) -> Rgb {
        let l = (accent.0 as u32 * 77 + accent.1 as u32 * 150 + accent.2 as u32 * 29) >> 8;
        if dark {
            // lifted towards white until it stands out on #1C1C1C, however dark the user's colour
            accent.mix(
                Rgb::hex(0xFFFFFF),
                if l < 90 {
                    0.50
                } else if l < 150 {
                    0.35
                } else {
                    0.10
                },
            )
        } else {
            accent.mix(Rgb(0, 0, 0), if l > 170 { 0.35 } else { 0.0 })
        }
    }

    /// Light: a soft grey frame, the content a white card on it, the accent for what is chosen.
    pub fn light() -> Theme {
        let accent = Theme::accent_for(Theme::system_accent(), false);
        let white = Rgb::hex(0xFFFFFF);
        let chrome = Rgb::hex(0xEDEEF0);
        Theme {
            dark: false,
            chrome,
            bg: white,
            card_border: Rgb::hex(0xDDDEE1),
            card_edge: Rgb::hex(0xE4E5E8),
            fg: Rgb::hex(0x1B1B1B),
            muted: Rgb::hex(0x5F6368),
            faint: Rgb::hex(0xA2A5AA),
            hover: Rgb::hex(0xF2F3F5),
            chrome_hover: chrome.mix(Rgb(0, 0, 0), 0.055),
            sel: white.mix(accent, 0.15),
            sel_border: white.mix(accent, 0.55),
            sel_inactive: white.mix(accent, 0.08).mix(Rgb::hex(0xEBECEE), 0.5),
            header_fg: Rgb::hex(0x6B7078),
            line: Rgb::hex(0xEDEEF0),
            bar_bg: Rgb::hex(0xF6F7F8),
            input_bg: white,
            input_border: Rgb::hex(0xD6D8DC),
            input_hot: Rgb::hex(0xB9BCC2),
            btn_hover: chrome.mix(Rgb(0, 0, 0), 0.055),
            btn_press: chrome.mix(Rgb(0, 0, 0), 0.10),
            accent,
            primary: accent,
            primary_hot: accent.mix(Rgb(0, 0, 0), 0.12),
            primary_fg: Rgb::hex(0xFFFFFF),
            danger: Rgb::hex(0xC42B1C),
            thumb: Rgb::hex(0xC2C5CA),
            frame: Rgb::hex(0xB4B6BA),
            progress: Rgb::hex(0x06B025),
            progress_fill: Rgb::hex(0xC6EFCB),
        }
    }

    /// Dark: a charcoal frame, the content a deeper card on it, the accent lifted for dark.
    pub fn dark() -> Theme {
        let accent = Theme::accent_for(Theme::system_accent(), true);
        let bg = Rgb::hex(0x191919);
        let chrome = Rgb::hex(0x222222);
        // black or white letters on the accent, whichever reads
        let l = (accent.0 as u32 * 77 + accent.1 as u32 * 150 + accent.2 as u32 * 29) >> 8;
        Theme {
            dark: true,
            chrome,
            bg,
            card_border: Rgb::hex(0x303030),
            card_edge: Rgb::hex(0x3A3A3A),
            fg: Rgb::hex(0xEDEDED),
            muted: Rgb::hex(0xA3A3A3),
            faint: Rgb::hex(0x666666),
            hover: Rgb::hex(0x232323),
            chrome_hover: chrome.mix(Rgb::hex(0xFFFFFF), 0.07),
            sel: bg.mix(accent, 0.20),
            sel_border: bg.mix(accent, 0.55),
            sel_inactive: bg.mix(accent, 0.11),
            header_fg: Rgb::hex(0x999999),
            line: Rgb::hex(0x242424),
            bar_bg: Rgb::hex(0x1F1F1F),
            input_bg: Rgb::hex(0x1E1E1E),
            input_border: Rgb::hex(0x3A3A3A),
            input_hot: Rgb::hex(0x555555),
            btn_hover: chrome.mix(Rgb::hex(0xFFFFFF), 0.07),
            btn_press: chrome.mix(Rgb::hex(0xFFFFFF), 0.12),
            accent,
            primary: accent,
            primary_hot: accent.mix(Rgb::hex(0xFFFFFF), 0.12),
            primary_fg: if l > 140 { Rgb::hex(0x111111) } else { Rgb::hex(0xFFFFFF) },
            danger: Rgb::hex(0xE8705F),
            thumb: Rgb::hex(0x4A4A4A),
            frame: Rgb::hex(0x3E3E3E),
            progress: Rgb::hex(0x3CC75A),
            progress_fill: Rgb::hex(0x1D3B24),
        }
    }

    pub fn system_is_dark() -> bool {
        use winreg::enums::HKEY_CURRENT_USER;
        let light: u32 = winreg::RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
            .and_then(|k| k.get_value("AppsUseLightTheme"))
            .unwrap_or(1);
        light == 0
    }

    pub fn for_mode(mode: ThemeMode) -> Theme {
        match mode {
            ThemeMode::Light => Theme::light(),
            ThemeMode::Dark => Theme::dark(),
            ThemeMode::System if Theme::system_is_dark() => Theme::dark(),
            ThemeMode::System => Theme::light(),
        }
    }
}

pub struct Fonts {
    pub ui: HFONT,
    pub bold: HFONT,
    pub small: HFONT,
    pub title: HFONT,
    pub big: HFONT,
}

fn font(px: i32, weight: i32, face: &str) -> HFONT {
    font_q(px, weight, face, CLEARTYPE_QUALITY)
}

fn font_q(px: i32, weight: i32, face: &str, quality: FONT_QUALITY) -> HFONT {
    unsafe {
        CreateFontW(
            -px,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            quality,
            0,
            PCWSTR(wstr(face).as_ptr()),
        )
    }
}

impl Fonts {
    pub fn new(scale: f32) -> Fonts {
        let px = |pt: f32| (pt * 96.0 / 72.0 * scale).round() as i32;
        // Windows 10's type: Segoe UI, Semibold for emphasis, Light for large titles
        let (text, semi, display) = ("Segoe UI", "Segoe UI Semibold", "Segoe UI Semibold");
        Fonts {
            ui: font(px(9.0), 400, text),
            bold: font(px(9.0), 600, semi),
            small: font(px(8.5), 600, semi),
            title: font(px(11.5), 600, display),
            big: font(px(15.0), 300, "Segoe UI Light"),
        }
    }
}

/// Loads the fonts the first frame uses on a spare thread while the window is being made: GDI's first use of a face
/// maps and parses the font file, and the first paint should not wait for that.
pub fn warm_up() {
    let Some(surf) = Surface::new(64, 32) else { return };
    for (face, weight) in [("Segoe UI", 400), ("Segoe UI Semibold", 600)] {
        let f = font(16, weight, face);
        surf.text(R::new(0, 0, 64, 32), "Ag", f, Rgb(0, 0, 0), LEFT);
        unsafe {
            let _ = DeleteObject(f.into());
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        unsafe {
            for f in [self.ui, self.bold, self.small, self.title, self.big] {
                let _ = DeleteObject(f.into());
            }
        }
    }
}

pub const LEFT: DRAW_TEXT_FORMAT =
    DRAW_TEXT_FORMAT(DT_LEFT.0 | DT_VCENTER.0 | DT_SINGLELINE.0 | DT_END_ELLIPSIS.0 | DT_NOPREFIX.0);
pub const RIGHT: DRAW_TEXT_FORMAT =
    DRAW_TEXT_FORMAT(DT_RIGHT.0 | DT_VCENTER.0 | DT_SINGLELINE.0 | DT_END_ELLIPSIS.0 | DT_NOPREFIX.0);
pub const CENTER: DRAW_TEXT_FORMAT = DRAW_TEXT_FORMAT(DT_CENTER.0 | DT_VCENTER.0 | DT_SINGLELINE.0 | DT_NOPREFIX.0);
pub const PATH_LEFT: DRAW_TEXT_FORMAT =
    DRAW_TEXT_FORMAT(DT_LEFT.0 | DT_VCENTER.0 | DT_SINGLELINE.0 | DT_PATH_ELLIPSIS.0 | DT_NOPREFIX.0);
// two centered lines under an icon, ellipsis on the second
pub const WRAP_CENTER: DRAW_TEXT_FORMAT =
    DRAW_TEXT_FORMAT(DT_CENTER.0 | DT_WORDBREAK.0 | DT_END_ELLIPSIS.0 | DT_EDITCONTROL.0 | DT_NOPREFIX.0);

/// A window's back buffer: a top-down 32-bit DIB selected into its own memory DC.
pub struct Surface {
    pub dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    pub w: i32,
    pub h: i32,
    // drawing is limited to this rectangle while it is set
    clip: std::cell::Cell<Option<R>>,
    // the window point this buffer's first pixel shows: a buffer can hold just a band of a window (the main window's
    // toolbar, status bar and the margin beside the card), and everything is drawn in window coordinates all the same
    ox: std::cell::Cell<i32>,
    oy: std::cell::Cell<i32>,
}

impl Surface {
    pub fn new(w: i32, h: i32) -> Option<Surface> {
        let (w, h) = (w.max(1), h.max(1));
        unsafe {
            let bi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bmp = CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            let dc = CreateCompatibleDC(None);
            let old = SelectObject(dc, bmp.into());
            SetBkMode(dc, TRANSPARENT);
            Some(Surface {
                dc,
                bmp,
                old,
                bits: bits as *mut u32,
                w,
                h,
                clip: std::cell::Cell::new(None),
                ox: std::cell::Cell::new(0),
                oy: std::cell::Cell::new(0),
            })
        }
    }

    /// the surface in `slot`, made or remade for this size
    pub fn ensure(slot: &mut Option<Surface>, w: i32, h: i32) -> Option<&Surface> {
        let (w, h) = (w.max(1), h.max(1));
        if slot.as_ref().is_none_or(|s| s.w != w || s.h != h) {
            *slot = None;
            *slot = Surface::new(w, h);
        }
        slot.as_ref()
    }

    /// makes the buffer show the window from row `oy` down
    pub fn set_origin(&self, oy: i32) {
        self.ox.set(0);
        self.oy.set(oy);
    }

    /// makes the buffer show the window from the point (`ox`, `oy`) right and down
    pub fn set_origin_xy(&self, ox: i32, oy: i32) {
        self.ox.set(ox);
        self.oy.set(oy);
    }

    // window coordinates to buffer coordinates
    fn tr(&self, r: R) -> R {
        R::new(r.x - self.ox.get(), r.y - self.oy.get(), r.w, r.h)
    }

    /// limits all drawing (pixels, text and icons) to `r`, or lifts the limit
    pub fn set_clip(&self, r: Option<R>) {
        let r = r.map(|r| self.tr(r));
        self.clip.set(r);
        unsafe {
            let _ = SelectClipRgn(self.dc, None);
            if let Some(r) = r {
                let _ = IntersectClipRect(self.dc, r.x, r.y, r.right(), r.bottom());
            }
        }
    }

    fn clip(&self, r: R) -> Option<(usize, usize, usize, usize)> {
        let c = self.clip.get().unwrap_or(R::new(0, 0, self.w, self.h));
        let x0 = r.x.max(0).max(c.x);
        let y0 = r.y.max(0).max(c.y);
        let x1 = r.right().min(self.w).min(c.right());
        let y1 = r.bottom().min(self.h).min(c.bottom());
        (x1 > x0 && y1 > y0).then_some((x0 as usize, y0 as usize, x1 as usize, y1 as usize))
    }

    fn row(&self, y: usize) -> &mut [u32] {
        // SAFETY: y < h; the DIB holds h rows of w pixels and lives as long as self; one thread draws
        unsafe { std::slice::from_raw_parts_mut(self.bits.add(y * self.w as usize), self.w as usize) }
    }

    /// a copy of the pixels drawn so far
    pub fn pixels(&self) -> Vec<u32> {
        self.flush();
        (0..self.h as usize).flat_map(|y| self.row(y).to_vec()).collect()
    }

    fn flush(&self) {
        unsafe {
            let _ = GdiFlush();
        }
    }

    pub fn fill(&self, r: R, c: Rgb) {
        let Some((x0, y0, x1, y1)) = self.clip(self.tr(r)) else { return };
        self.flush();
        let p = c.pixel();
        for y in y0..y1 {
            self.row(y)[x0..x1].fill(p);
        }
    }

    /// sets the pixels of `r` (window coordinates) row by row: `f(x, y, row)` with the window x and y of the row's
    /// first pixel
    pub fn put_rows(&self, r: R, mut f: impl FnMut(i32, i32, &mut [u32])) {
        let Some((x0, y0, x1, y1)) = self.clip(self.tr(r)) else { return };
        self.flush();
        let (ox, oy) = (self.ox.get(), self.oy.get());
        for y in y0..y1 {
            f(x0 as i32 + ox, y as i32 + oy, &mut self.row(y)[x0..x1]);
        }
    }

    /// Rounds the corners of `r` (window coordinates) named by `corners` (1 top left, 2 top right, 4 bottom right,
    /// 8 bottom left) over whatever is drawn there already: outside the curve `under` paints what lies behind (the
    /// glass, or the frame's colour), the curve's edge is smoothed, and a 1-pixel `border` follows it when given.
    /// Only the small squares at the corners are touched, so it is cheap to do after every paint.
    pub fn round_corners(&self, r: R, radius: f32, corners: u8, border: Option<Rgb>, under: &dyn Fn(&Surface, R)) {
        let n = radius.ceil() as i32 + 1;
        if n <= 1 || r.w < 2 * n || r.h < 2 * n {
            return;
        }
        let rad = radius;
        let squares = [
            (1u8, R::new(r.x, r.y, n, n), r.x as f32 + rad, r.y as f32 + rad),
            (2, R::new(r.right() - n, r.y, n, n), r.right() as f32 - rad, r.y as f32 + rad),
            (4, R::new(r.right() - n, r.bottom() - n, n, n), r.right() as f32 - rad, r.bottom() as f32 - rad),
            (8, R::new(r.x, r.bottom() - n, n, n), r.x as f32 + rad, r.bottom() as f32 - rad),
        ];
        let bp = border.map(|c| c.pixel());
        for (bit, sq, cx, cy) in squares {
            if corners & bit == 0 {
                continue;
            }
            // what is drawn there now, then what is behind, then the two joined along the curve
            let mut kept = Vec::with_capacity((n * n) as usize);
            self.put_rows(sq, |_, _, row| kept.extend_from_slice(row));
            under(self, sq);
            let mut k = 0;
            self.put_rows(sq, |x0, y, row| {
                let py = y as f32 + 0.5;
                for (i, px) in row.iter_mut().enumerate() {
                    let content = kept.get(k).copied().unwrap_or(*px);
                    k += 1;
                    let qx = (x0 + i as i32) as f32 + 0.5;
                    let (dx, dy) = (
                        (qx - cx) * if bit & 6 != 0 { 1.0 } else { -1.0 },
                        (py - cy) * if bit & 12 != 0 { 1.0 } else { -1.0 },
                    );
                    if dx <= 0.0 || dy <= 0.0 {
                        // beside the curve: the edge itself is straight here
                        *px = content;
                        continue;
                    }
                    let d = (dx * dx + dy * dy).sqrt();
                    let cov = (rad + 0.5 - d).clamp(0.0, 1.0);
                    let mut out = mix_px(*px, content, (cov * 256.0) as u32);
                    if let Some(b) = bp {
                        let ring = cov - (rad - 0.5 - d).clamp(0.0, 1.0);
                        if ring > 0.0 {
                            out = mix_px(out, b, (ring * 256.0) as u32);
                        }
                    }
                    *px = out;
                }
            });
        }
    }

    /// a rounded outline one pixel wide, inside `r`, leaving what is inside it
    pub fn round_ring(&self, r: R, radius: f32, c: Rgb) {
        let r = self.tr(r);
        let Some((x0, y0, x1, y1)) = self.clip(r) else { return };
        self.flush();
        let p = c.pixel();
        let rad = radius.min(r.w as f32 / 2.0).min(r.h as f32 / 2.0).max(0.0);
        let (l, t, rr, b) = (r.x as f32, r.y as f32, r.right() as f32, r.bottom() as f32);
        for y in y0..y1 {
            let row = self.row(y);
            let cy = y as f32 + 0.5;
            let edge_row = cy < t + 1.0 || cy > b - 1.0;
            for (x, px) in row.iter_mut().enumerate().take(x1).skip(x0) {
                let cx = x as f32 + 0.5;
                let qx = if cx < l + rad {
                    l + rad - cx
                } else if cx > rr - rad {
                    cx - (rr - rad)
                } else {
                    0.0
                };
                let qy = if cy < t + rad {
                    t + rad - cy
                } else if cy > b - rad {
                    cy - (b - rad)
                } else {
                    0.0
                };
                let cov = if qx > 0.0 && qy > 0.0 {
                    let d = (qx * qx + qy * qy).sqrt();
                    (rad + 0.5 - d).clamp(0.0, 1.0) - (rad - 0.5 - d).clamp(0.0, 1.0)
                } else if edge_row || cx < l + 1.0 || cx > rr - 1.0 {
                    1.0
                } else {
                    0.0
                };
                if cov > 0.0 {
                    *px = mix_px(*px, p, (cov * 256.0) as u32);
                }
            }
        }
    }

    /// a rounded shape laid over what is there at `a` strength (a soft shadow, a lit button on glass)
    pub fn round_blend(&self, r: R, radius: f32, c: Rgb, a: f32) {
        let r = self.tr(r);
        let Some((x0, y0, x1, y1)) = self.clip(r) else { return };
        self.flush();
        let p = c.pixel();
        let rad = radius.min(r.w as f32 / 2.0).min(r.h as f32 / 2.0).max(0.0);
        let (l, t, rr, b) = (r.x as f32, r.y as f32, r.right() as f32, r.bottom() as f32);
        for y in y0..y1 {
            let row = self.row(y);
            let cy = y as f32 + 0.5;
            for (x, px) in row.iter_mut().enumerate().take(x1).skip(x0) {
                let cx = x as f32 + 0.5;
                let qx = if cx < l + rad {
                    l + rad - cx
                } else if cx > rr - rad {
                    cx - (rr - rad)
                } else {
                    0.0
                };
                let qy = if cy < t + rad {
                    t + rad - cy
                } else if cy > b - rad {
                    cy - (b - rad)
                } else {
                    0.0
                };
                let cov =
                    if qx > 0.0 && qy > 0.0 { (rad + 0.5 - (qx * qx + qy * qy).sqrt()).clamp(0.0, 1.0) } else { 1.0 };
                if cov > 0.0 {
                    *px = mix_px(*px, p, (cov * a * 256.0) as u32);
                }
            }
        }
    }

    pub fn blend(&self, r: R, c: Rgb, a: f32) {
        let Some((x0, y0, x1, y1)) = self.clip(self.tr(r)) else { return };
        self.flush();
        let a = (a.clamp(0.0, 1.0) * 256.0) as u32;
        let s = c.pixel();
        for y in y0..y1 {
            for px in &mut self.row(y)[x0..x1] {
                *px = mix_px(*px, s, a);
            }
        }
    }

    pub fn frame(&self, r: R, c: Rgb) {
        self.fill(R::new(r.x, r.y, r.w, 1), c);
        self.fill(R::new(r.x, r.bottom() - 1, r.w, 1), c);
        self.fill(R::new(r.x, r.y, 1, r.h), c);
        self.fill(R::new(r.right() - 1, r.y, 1, r.h), c);
    }

    /// filled rectangle with antialiased round corners
    pub fn round(&self, r: R, radius: f32, c: Rgb) {
        let r = self.tr(r);
        let Some((x0, y0, x1, y1)) = self.clip(r) else { return };
        self.flush();
        let p = c.pixel();
        let rad = radius.min(r.w as f32 / 2.0).min(r.h as f32 / 2.0).max(0.0);
        let (l, t, rr, b) = (r.x as f32, r.y as f32, r.right() as f32, r.bottom() as f32);
        for y in y0..y1 {
            let row = self.row(y);
            let cy = y as f32 + 0.5;
            for (x, px) in row.iter_mut().enumerate().take(x1).skip(x0) {
                let cx = x as f32 + 0.5;
                let qx = if cx < l + rad {
                    l + rad - cx
                } else if cx > rr - rad {
                    cx - (rr - rad)
                } else {
                    0.0
                };
                let qy = if cy < t + rad {
                    t + rad - cy
                } else if cy > b - rad {
                    cy - (b - rad)
                } else {
                    0.0
                };
                if qx > 0.0 && qy > 0.0 {
                    let cov = (rad + 0.5 - (qx * qx + qy * qy).sqrt()).clamp(0.0, 1.0);
                    if cov > 0.0 {
                        *px = mix_px(*px, p, (cov * 256.0) as u32);
                    }
                } else {
                    *px = p;
                }
            }
        }
    }

    pub fn round_frame(&self, r: R, radius: f32, border: Rgb, fill: Rgb) {
        self.round(r, radius, border);
        self.round(r.inset(1, 1), (radius - 1.0).max(0.0), fill);
    }

    /// a ring chart: `frac` of it (clockwise from the top) in `used`, the rest in `free`
    pub fn donut(&self, cx: f32, cy: f32, outer: f32, inner: f32, frac: f32, used: Rgb, free: Rgb) {
        let cx = cx - self.ox.get() as f32;
        let cy = cy - self.oy.get() as f32;
        let r = R::new(
            (cx - outer - 1.0) as i32,
            (cy - outer - 1.0) as i32,
            (outer * 2.0 + 3.0) as i32,
            (outer * 2.0 + 3.0) as i32,
        );
        let Some((x0, y0, x1, y1)) = self.clip(r) else { return };
        self.flush();
        let limit = frac.clamp(0.0, 1.0) * std::f32::consts::TAU;
        for y in y0..y1 {
            let row = self.row(y);
            for (x, px) in row.iter_mut().enumerate().take(x1).skip(x0) {
                let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                let d = (dx * dx + dy * dy).sqrt();
                let cov = (outer + 0.5 - d).clamp(0.0, 1.0) * (d - inner + 0.5).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                let mut ang = dx.atan2(-dy);
                if ang < 0.0 {
                    ang += std::f32::consts::TAU;
                }
                let c = if ang <= limit { used } else { free };
                *px = mix_px(*px, c.pixel(), (cov * 256.0) as u32);
            }
        }
    }

    /// fills `r` from its left edge to `extent` pixels across, the last pixel by how much of it is covered, so a
    /// growing bar moves smoothly rather than a pixel at a time
    pub fn fill_frac(&self, r: R, extent: f32, c: Rgb) {
        let extent = extent.clamp(0.0, r.w as f32);
        let whole = extent.floor() as i32;
        self.fill(R::new(r.x, r.y, whole, r.h), c);
        let part = extent - whole as f32;
        if part > 0.0 {
            self.blend(R::new(r.x + whole, r.y, 1, r.h), c, part);
        }
    }

    /// a 1-pixel horizontal line at a fractional height, spread over the two rows it touches
    pub fn hline(&self, x: i32, y: f32, w: i32, c: Rgb, strength: f32) {
        let top = y.floor() as i32;
        let f = y - top as f32;
        self.blend(R::new(x, top, w, 1), c, (1.0 - f) * strength);
        self.blend(R::new(x, top + 1, w, 1), c, f * strength);
    }

    /// An area chart of values in 0..=1 spread across the first `extent` pixels of `r`: a smooth curve (Catmull-Rom
    /// through the samples) with an antialiased fill edge, a `thick`-pixel antialiased line on top, and a soft end at
    /// the fractional `extent`.
    pub fn area(&self, r: R, extent: f32, values: &[f32], fill: Rgb, line: Rgb, thick: f32) {
        if values.len() < 2 || extent < 1.0 || r.h < 2 {
            return;
        }
        let r = self.tr(r);
        let extent = extent.min(r.w as f32);
        let span = R::new(r.x, r.y, extent.ceil() as i32, r.h);
        let Some((x0, y0, x1, y1)) = self.clip(span) else { return };
        self.flush();
        let n = values.len() - 1;
        let v = |i: isize| values[i.clamp(0, n as isize) as usize];
        // the curve's top edge (pixels, fractional) at a horizontal position
        let top_at = |x: f32| -> f32 {
            let t = ((x - r.x as f32) / (extent - 1.0).max(1.0) * n as f32).clamp(0.0, n as f32);
            let i = (t.floor() as isize).min(n as isize - 1);
            let f = t - i as f32;
            let (p0, p1, p2, p3) = (v(i - 1), v(i), v(i + 1), v(i + 2));
            let f2 = f * f;
            let y = 0.5
                * (2.0 * p1
                    + (p2 - p0) * f
                    + (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3) * f2
                    + (-p0 + 3.0 * p1 - 3.0 * p2 + p3) * f2 * f);
            r.bottom() as f32 - y.clamp(0.0, 1.0) * (r.h - 1) as f32
        };
        let (fp, lp) = (fill.pixel(), line.pixel());
        let half = thick / 2.0;
        for x in x0..x1 {
            let cx = x as f32 + 0.5;
            // the last column only as far as `extent` reaches into it
            let edge = (r.x as f32 + extent - x as f32).clamp(0.0, 1.0);
            let top = top_at(cx.min(r.x as f32 + extent - 0.5));
            let slope = (top_at(cx + 1.0) - top_at(cx - 1.0)) / 2.0;
            // the line's vertical half-height grows with the slope, so steep parts keep their thickness
            let hh = half * (1.0 + slope * slope).sqrt();
            for y in y0..y1 {
                let cy = y as f32 + 0.5;
                let px = &mut self.row(y)[x];
                // fill below the curve, its top pixel by coverage
                let under = (cy + 0.5 - top).clamp(0.0, 1.0) * edge;
                if under > 0.0 {
                    *px = mix_px(*px, fp, (under * 256.0) as u32);
                }
                let d = (cy - top).abs();
                let cov = (hh + 0.5 - d).clamp(0.0, 1.0) * edge;
                if cov > 0.0 {
                    *px = mix_px(*px, lp, (cov * 256.0) as u32);
                }
            }
        }
    }

    pub fn text(&self, r: R, s: &str, f: HFONT, c: Rgb, flags: DRAW_TEXT_FORMAT) {
        if s.is_empty() || r.w <= 0 {
            return;
        }
        let mut w: Vec<u16> = s.encode_utf16().collect();
        let mut rc = self.tr(r).rect();
        unsafe {
            SelectObject(self.dc, f.into());
            SetTextColor(self.dc, c.colorref());
            // One line that fits, in scripts drawn letter by letter (Latin, Greek, Cyrillic): measured and drawn
            // with ExtTextOutW, about three times quicker than DrawTextW, which lays out every string in full.
            // Anything that needs shortening with "...", wrapping or shaping goes to DrawTextW as before.
            let one_line =
                flags.0 & DT_SINGLELINE.0 != 0 && flags.0 & DT_VCENTER.0 != 0 && flags.0 & DT_NOPREFIX.0 != 0;
            if one_line && w.iter().all(|&u| u < 0x0590) {
                let mut sz = SIZE::default();
                if GetTextExtentPoint32W(self.dc, &w, &mut sz).as_bool() && sz.cx <= rc.right - rc.left {
                    let x = if flags.0 & DT_RIGHT.0 != 0 {
                        rc.right - sz.cx
                    } else if flags.0 & DT_CENTER.0 != 0 {
                        rc.left + (rc.right - rc.left - sz.cx) / 2
                    } else {
                        rc.left
                    };
                    let y = rc.top + (rc.bottom - rc.top - sz.cy) / 2;
                    let _ =
                        ExtTextOutW(self.dc, x, y, ETO_CLIPPED, Some(&rc), PCWSTR(w.as_ptr()), w.len() as u32, None);
                    return;
                }
            }
            DrawTextW(self.dc, &mut w, &mut rc, flags);
        }
    }

    /// draws an icon (premultiplied pixels) centred in `r`
    pub fn image_in(&self, r: R, img: &crate::ui::thumbs::Image) {
        self.image(r.x + (r.w - img.w) / 2, r.y + (r.h - img.h) / 2, img.w, img.h, &img.px);
    }

    /// draws premultiplied BGRA pixels (see `thumbs::Image`) with their transparency, top-left at (x, y)
    pub fn image(&self, x: i32, y: i32, w: i32, h: i32, px: &[u32]) {
        let x = x - self.ox.get();
        let y = y - self.oy.get();
        let Some((x0, y0, x1, y1)) = self.clip(R::new(x, y, w, h)) else { return };
        self.flush();
        for yy in y0..y1 {
            let row = self.row(yy);
            let src = &px[((yy as i32 - y) * w) as usize..][..w as usize];
            for xx in x0..x1 {
                let s = src[(xx as i32 - x) as usize];
                let a = s >> 24;
                if a == 0 {
                    continue;
                }
                let d = &mut row[xx];
                if a == 255 {
                    *d = s & 0xFF_FFFF;
                    continue;
                }
                let inv = 255 - a;
                let ch = |sh: u32| ((s >> sh & 0xFF) + ((*d >> sh & 0xFF) * inv + 127) / 255).min(255) << sh;
                *d = ch(16) | ch(8) | ch(0);
            }
        }
    }

    pub fn present(&self, hwnd: HWND) {
        unsafe {
            let dc = GetDC(Some(hwnd));
            let _ = BitBlt(dc, self.ox.get(), self.oy.get(), self.w, self.h, Some(self.dc), 0, 0, SRCCOPY);
            ReleaseDC(Some(hwnd), dc);
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

fn mix_px(d: u32, s: u32, a: u32) -> u32 {
    if a >= 256 {
        return s;
    }
    let mix = |shift: u32| {
        let (dv, sv) = ((d >> shift) & 0xFF, (s >> shift) & 0xFF);
        ((dv * (256 - a) + sv * a) >> 8) << shift
    };
    mix(16) | mix(8) | mix(0)
}

/// the height of a line in `f`
pub fn line_height(dc: HDC, f: HFONT) -> i32 {
    let mut tm = TEXTMETRICW::default();
    unsafe {
        SelectObject(dc, f.into());
        let _ = GetTextMetricsW(dc, &mut tm);
    }
    tm.tmHeight
}

pub fn text_width(dc: HDC, s: &str, f: HFONT) -> i32 {
    let w: Vec<u16> = s.encode_utf16().collect();
    let mut sz = SIZE::default();
    unsafe {
        SelectObject(dc, f.into());
        let _ = GetTextExtentPoint32W(dc, &w, &mut sz);
    }
    sz.cx
}

#[cfg(test)]
mod text_tests {
    use super::*;

    /// the quick path draws the same pixels DrawTextW draws, for each alignment and font
    #[test]
    fn quick_text_matches_drawtext() {
        let fonts = Fonts::new(1.25);
        let words = [
            "advapi32.dll",
            "10/15/2026 4:54 PM",
            "Application extension",
            "1,234 KB",
            "Привет мир",
            "Ωmega ",
            "a&b",
            "Cafe\u{0301}.txt",
            "Բարեւ.txt",
        ];
        for f in [fonts.ui, fonts.bold, fonts.small] {
            for flags in [LEFT, RIGHT, CENTER, PATH_LEFT] {
                for word in words {
                    let (a, b) = (Surface::new(300, 30).unwrap(), Surface::new(300, 30).unwrap());
                    let r = R::new(7, 3, 280, 24);
                    a.text(r, word, f, Rgb(220, 220, 220), flags);
                    let mut w: Vec<u16> = word.encode_utf16().collect();
                    let mut rc = r.rect();
                    unsafe {
                        SelectObject(b.dc, f.into());
                        SetTextColor(b.dc, Rgb(220, 220, 220).colorref());
                        DrawTextW(b.dc, &mut w, &mut rc, flags);
                    }
                    assert!(a.pixels() == b.pixels(), "{word:?} differs with flags {:#x}", flags.0);
                }
            }
        }
    }
}
