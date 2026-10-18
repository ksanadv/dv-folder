// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! dvfiles' glass: the frame around the list (the toolbar, the side pane, the status bar) shows what is really behind
//! the window (a browser, a video, the desktop), blurred and darkened by dvfiles itself. Windows is asked to leave
//! the window out of screen captures (SetWindowDisplayAffinity), so a copy of the screen under the window's place is
//! what lies behind it; it is taken small, blurred, and kept (about 100 KB). It is taken again when the window moves,
//! comes forward or is resized, and on a timer while the window shows, so a video behind it moves in the glass too:
//! twice a second after a change, slowing to once every 4 s while nothing behind changes. The panes are repainted
//! only when it changed. Every pixel stays opaque, so Windows' text boxes and the list
//! are untouched. Where Windows cannot leave the window out of captures (before Windows 10 2004), and with the setting
//! off, the frame is drawn solid.
use crate::ui::gfx::{Rgb, Surface, R};
use crate::ui::App;
use std::cell::Cell;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetWindowRect, IsIconic, IsWindowVisible, KillTimer, SetTimer, SetWindowDisplayAffinity,
    WindowFromPoint, GA_ROOTOWNER, WDA_EXCLUDEFROMCAPTURE, WDA_NONE, WM_APP,
};

/// the main window's timer that looks behind the window again
pub const T_GLASS: usize = 6;
/// a look behind the window, taken on a thread, has come back (a boxed `Option<Backdrop>`)
pub const WM_GLASS: u32 = WM_APP + 43;
// the timer's quickest and slowest pace
const EVERY_MS: u32 = 500;
const SLOWEST_MS: u32 = 4000;

/// screen pixels per pixel of the copy
const STEP: i32 = 4;
/// how far past the window's edges the copy reaches, so the blur at the edges has something to draw on
const PAD: i32 = 48;

/// what is behind the window, small and blurred: 0x00RRGGBB pixels, its top-left corner at (`x`, `y`) on the screen
pub struct Backdrop {
    w: i32,
    h: i32,
    x: i32,
    y: i32,
    px: Vec<u32>,
    // to tell whether a new look behind changed anything
    sum: u64,
}

/// for tests and screenshots (DVFILES_GLASS_TEST): one look behind, then the window may be captured again and the
/// glass keeps that look, so a shot of the window shows it
fn test_mode() -> bool {
    std::env::var_os("DVFILES_GLASS_TEST").is_some()
}

/// Turns the glass on or off for the window: on, the window is left out of screen captures and the timer that looks
/// behind it runs. False when Windows cannot leave it out (the frame is drawn solid then).
pub fn start(main: HWND, on: bool) -> bool {
    unsafe {
        if !on {
            let _ = KillTimer(Some(main), T_GLASS);
            let _ = SetWindowDisplayAffinity(main, WDA_NONE);
            return false;
        }
        if SetWindowDisplayAffinity(main, WDA_EXCLUDEFROMCAPTURE).is_err() {
            return false;
        }
        if !test_mode() {
            SetTimer(Some(main), T_GLASS, EVERY_MS, None);
        }
    }
    true
}

/// three box blurs, across and down, which together come close to a Gaussian one
fn blur(px: &mut [u32], w: usize, h: usize, radius: usize) {
    let mut tmp = vec![0u32; px.len()];
    for _ in 0..3 {
        pass(px, &mut tmp, w, h, radius, true);
        pass(&tmp, px, w, h, radius, false);
    }
}

/// one box blur along rows (`across`) or columns, edges held
fn pass(src: &[u32], dst: &mut [u32], w: usize, h: usize, r: usize, across: bool) {
    let (n, lines) = if across { (w, h) } else { (h, w) };
    let at = |line: usize, i: usize| if across { line * w + i } else { i * w + line };
    let span = (2 * r + 1) as u32;
    for line in 0..lines {
        let get = |i: isize| src[at(line, i.clamp(0, n as isize - 1) as usize)];
        let mut sum = [0u32; 3];
        for i in -(r as isize)..=(r as isize) {
            let p = get(i);
            sum[0] += p >> 16 & 0xFF;
            sum[1] += p >> 8 & 0xFF;
            sum[2] += p & 0xFF;
        }
        for i in 0..n {
            dst[at(line, i)] = (sum[0] / span) << 16 | (sum[1] / span) << 8 | sum[2] / span;
            let (out, inn) = (get(i as isize - r as isize), get(i as isize + r as isize + 1));
            sum[0] = sum[0] + (inn >> 16 & 0xFF) - (out >> 16 & 0xFF);
            sum[1] = sum[1] + (inn >> 8 & 0xFF) - (out >> 8 & 0xFF);
            sum[2] = sum[2] + (inn & 0xFF) - (out & 0xFF);
        }
    }
}

/// whether other windows cover the frame at its corners and the side pane's middle, as when a browser is in front:
/// no glass shows then, so nothing is copied. Asked only once the window shows, as before that every point is another's (copying the screen is most of the glass's cost)
fn covered(main: HWND, wr: &RECT) -> bool {
    let i = 12;
    let points = [
        (wr.left + i, wr.top + i),
        (wr.right - i, wr.top + i),
        (wr.left + i, wr.bottom - i),
        (wr.right - i, wr.bottom - i),
        (wr.left + i, (wr.top + wr.bottom) / 2),
    ];
    points.iter().all(|&(x, y)| unsafe { GetAncestor(WindowFromPoint(POINT { x, y }), GA_ROOTOWNER) != main })
}

/// where to look: the window's place, none while it is minimized or covered
fn place(main: HWND) -> Option<RECT> {
    let mut wr = RECT::default();
    unsafe {
        let shown = IsWindowVisible(main).as_bool();
        if IsIconic(main).as_bool() || GetWindowRect(main, &mut wr).is_err() || (shown && covered(main, &wr)) {
            return None;
        }
    }
    Some(wr)
}

/// A small copy of the screen at `wr` and around it (the window itself left out by Windows), blurred. Taken on a
/// thread: the copy waits on the compositor (5-20 ms here), which would stall the window, a drag of it most of all.
fn look_behind(wr: RECT) -> Option<Backdrop> {
    let (x, y) = (wr.left - PAD, wr.top - PAD);
    let (w, h) = ((wr.right - wr.left + 2 * PAD) / STEP, (wr.bottom - wr.top + 2 * PAD) / STEP);
    if w <= 0 || h <= 0 {
        return None;
    }
    let small = Surface::new(w, h)?;
    unsafe {
        let screen = GetDC(None);
        // every fourth pixel is enough: the blur that follows smooths what it skips
        SetStretchBltMode(small.dc, COLORONCOLOR);
        let ok = StretchBlt(small.dc, 0, 0, w, h, Some(screen), x, y, w * STEP, h * STEP, SRCCOPY).as_bool();
        ReleaseDC(None, screen);
        if !ok {
            return None;
        }
    }
    let mut px: Vec<u32> = small.pixels().iter().map(|p| p & 0x00FF_FFFF).collect();
    blur(&mut px, w as usize, h as usize, 6);
    let sum = px.iter().enumerate().fold(0u64, |s, (i, &p)| s.wrapping_mul(31).wrapping_add(p as u64 ^ i as u64));
    Some(Backdrop { w, h, x, y, px, sum })
}

thread_local! {
    // a look is on its way, and whether another was asked for meanwhile (the window moved again)
    static LOOKING: Cell<(bool, bool)> = const { Cell::new((false, false)) };
}

/// Looks behind the window again, on a thread; `took` gets what it saw.
pub fn look(a: &mut App) {
    if !(a.settings.glass && a.glass_live) || (test_mode() && a.backdrop.is_some()) {
        return;
    }
    if LOOKING.get().0 {
        LOOKING.set((true, true));
        return;
    }
    let Some(wr) = place(a.main) else {
        // minimized or covered: looked at again slowly, and at once when the window comes forward
        if a.glass_pace != SLOWEST_MS && !test_mode() {
            a.glass_pace = SLOWEST_MS;
            unsafe {
                SetTimer(Some(a.main), T_GLASS, SLOWEST_MS, None);
            }
        }
        return;
    };
    LOOKING.set((true, false));
    let main = a.main.0 as isize;
    std::thread::spawn(move || crate::ui::post_box(HWND(main as *mut _), WM_GLASS, look_behind(wr)));
}

/// What a look behind saw; true when the glass shows something new (the panes are to be repainted).
pub fn took(a: &mut App, b: Option<Backdrop>) -> bool {
    let again = LOOKING.get().1;
    LOOKING.set((false, false));
    if again {
        look(a);
    }
    let Some(b) = b.filter(|_| a.settings.glass && a.glass_live) else { return false };
    if test_mode() {
        unsafe {
            let _ = SetWindowDisplayAffinity(a.main, WDA_NONE);
        }
    }
    let changed = a
        .backdrop
        .as_ref()
        .is_none_or(|old| old.sum != b.sum || old.x != b.x || old.y != b.y || old.w != b.w || old.h != b.h);
    a.backdrop = Some(b);
    // quick again after a change, slower each time nothing changed
    let pace = if changed { EVERY_MS } else { (a.glass_pace * 2).clamp(EVERY_MS, SLOWEST_MS) };
    if pace != a.glass_pace && !test_mode() {
        a.glass_pace = pace;
        unsafe {
            SetTimer(Some(a.main), T_GLASS, pace, None);
        }
    }
    changed
}

/// whether the panes are drawn as glass now
pub fn on(a: &App) -> bool {
    a.settings.glass && a.glass_live && a.backdrop.is_some()
}

/// how much of the theme's frame colour lies over what is behind, out of 256: a smoked pane, dark in the dark theme
/// and pale in the light one, through which what is behind shows as soft shapes of light and dark
fn veil(dark: bool) -> u32 {
    if dark {
        200
    } else {
        190
    }
}

/// a colour behind as the glass shows it: nearly all its colour washed out (an eighth kept, so a hint of it remains
/// but never a tint), then the theme's veil over it
fn tinted(c: [u32; 3], base: Rgb, dark: bool, grain: u32) -> u32 {
    let k = veil(dark);
    let l = (c[0] * 77 + c[1] * 150 + c[2] * 29) >> 8;
    let t = [base.0 as u32, base.1 as u32, base.2 as u32];
    let mix = |i: usize| (((c[i] + l * 7) / 8 * (256 - k) + t[i] * k) / 256 + grain).min(255);
    0xFF00_0000 | mix(0) << 16 | mix(1) << 8 | mix(2)
}

impl Backdrop {
    /// the colour behind a screen point (nearest pixel: for averages)
    fn at(&self, sx: i32, sy: i32) -> [u32; 3] {
        let u = ((sx - self.x) / STEP).clamp(0, self.w - 1);
        let v = ((sy - self.y) / STEP).clamp(0, self.h - 1);
        let p = self.px[(v * self.w + u) as usize];
        [p >> 16 & 0xFF, p >> 8 & 0xFF, p & 0xFF]
    }
}

/// The glass's colour along the top of the window, for Windows to draw the title bar in, so the title bar and the
/// toolbar under it read as one pane of glass.
pub fn caption_colour(a: &App) -> Option<Rgb> {
    if !on(a) {
        return None;
    }
    let b = a.backdrop.as_ref()?;
    let mut o = POINT::default();
    unsafe {
        let _ = ClientToScreen(a.main, &mut o);
    }
    let (w, _) = crate::ui::client_size(a.main);
    let mut sum = [0u32; 3];
    for k in 0..16 {
        let c = b.at(o.x + w * (2 * k + 1) / 32, o.y);
        for i in 0..3 {
            sum[i] += c[i];
        }
    }
    let px = tinted([sum[0] / 16, sum[1] / 16, sum[2] / 16], a.theme.chrome, a.theme.dark, 0);
    Some(Rgb((px >> 16) as u8, (px >> 8) as u8, px as u8))
}

/// Draws the glass behind `r` of the pane `hwnd` (in the surface's window coordinates): what is behind it on the
/// screen, blurred, washed out and veiled with the theme's frame colour. False when there is no glass to draw, so
/// the caller fills solid.
pub fn paint(a: &App, surf: &Surface, hwnd: HWND, r: R) -> bool {
    if !on(a) {
        return false;
    }
    let Some(b) = a.backdrop.as_ref() else { return false };
    let mut o = POINT::default();
    unsafe {
        let _ = ClientToScreen(hwnd, &mut o);
    }
    let s = STEP as f32;
    // backdrop coordinates in 16.16 fixed point
    let step = (65536.0 / s) as i64;
    let (base, dark) = (a.theme.chrome, a.theme.dark);
    let (bw, bh) = (b.w as i64, b.h as i64);
    surf.put_rows(r, |x0, y, row| {
        let v = (((o.y + y - b.y) as f32 + 0.5) / s - 0.5).max(0.0);
        let (v0, fv) = ((v as i64).min(bh - 1), ((v.fract()) * 256.0) as u32);
        let v1 = (v0 + 1).min(bh - 1);
        let (r0, r1) = (&b.px[(v0 * bw) as usize..][..bw as usize], &b.px[(v1 * bw) as usize..][..bw as usize]);
        let mut u = ((((o.x + x0 - b.x) as f32 + 0.5) / s - 0.5).max(0.0) * 65536.0) as i64;
        for (i, out) in row.iter_mut().enumerate() {
            let ui = (u >> 16).min(bw - 1) as usize;
            let u1 = (ui + 1).min(bw as usize - 1);
            let fu = ((u >> 8) & 0xFF) as u32;
            let (a0, a1, c0, c1) = (r0[ui], r0[u1], r1[ui], r1[u1]);
            let ch = |sh: u32| {
                let top = (a0 >> sh & 0xFF) * (256 - fu) + (a1 >> sh & 0xFF) * fu;
                let bot = (c0 >> sh & 0xFF) * (256 - fu) + (c1 >> sh & 0xFF) * fu;
                (top * (256 - fv) + bot * fv) >> 16
            };
            // a faint grain, as frosted glass has, which also keeps the gradient from banding
            let n = ((x0 as u32 + i as u32).wrapping_mul(0x9E37_79B9) ^ (y as u32).wrapping_mul(0x85EB_CA6B)) >> 30;
            *out = tinted([ch(16), ch(8), ch(0)], base, dark, n);
            u += step;
        }
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blur_keeps_flat_colour_and_softens_an_edge() {
        let (w, h) = (40usize, 10usize);
        let mut flat = vec![0x0040_8020u32; w * h];
        blur(&mut flat, w, h, 3);
        assert!(flat.iter().all(|&p| p == 0x0040_8020));
        // black left half, white right half: the middle becomes grey, the far ends stay
        let mut edge: Vec<u32> = (0..w * h).map(|i| if i % w < w / 2 { 0 } else { 0x00FF_FFFF }).collect();
        blur(&mut edge, w, h, 3);
        let mid = edge[5 * w + w / 2] & 0xFF;
        assert!(mid > 60 && mid < 200, "{mid}");
        assert_eq!(edge[5 * w] & 0xFF, 0);
        assert_eq!(edge[5 * w + w - 1] & 0xFF, 255);
    }

    /// whatever the colour behind, the glass is near grey: at most a hint of it
    #[test]
    fn the_glass_is_never_a_tint() {
        let base = Rgb::hex(0x222222);
        for c in [[255, 0, 80], [200, 40, 120], [0, 0, 255]] {
            let p = tinted(c, base, true, 0);
            let (r, g, b) = (p >> 16 & 0xFF, p >> 8 & 0xFF, p & 0xFF);
            assert!(r.max(g).max(b) - r.min(g).min(b) <= 8, "{c:?} -> {r},{g},{b}");
        }
    }
}
