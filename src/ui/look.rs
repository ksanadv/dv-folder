// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! The theme, the glass and the frame: colours applied to the windows, and what is painted behind the card.
use super::*;

// undocumented but long-stable uxtheme entry points: 0 default, 1 allow dark, 2 force dark, 3 force light
pub(super) unsafe fn set_app_mode(mode: i32) {
    let Ok(m) = LoadLibraryW(w!("uxtheme.dll")) else { return };
    if let Some(f) = GetProcAddress(m, windows::core::PCSTR(135 as *const u8)) {
        let set_mode: extern "system" fn(i32) -> i32 = std::mem::transmute(f);
        set_mode(mode);
    }
    if let Some(f) = GetProcAddress(m, windows::core::PCSTR(136 as *const u8)) {
        let flush: extern "system" fn() = std::mem::transmute(f);
        flush();
    }
}

pub fn app_mode(mode: ThemeMode) -> i32 {
    match mode {
        ThemeMode::System => 1,
        ThemeMode::Light => 3,
        ThemeMode::Dark => 2,
    }
}

pub fn apply_theme(hwnds: &[HWND], main: HWND, theme: &Theme, mode: ThemeMode) {
    unsafe {
        set_app_mode(app_mode(mode));
        let on: i32 = theme.dark as i32;
        let _ = DwmSetWindowAttribute(main, DWMWA_USE_IMMERSIVE_DARK_MODE, &on as *const i32 as *const _, 4);
        tint_frame(main, theme.chrome, theme.fg, theme.frame);
        let name = if theme.dark { w!("DarkMode_Explorer") } else { w!("Explorer") };
        for &h in hwnds {
            let _ = SetWindowTheme(h, name, PCWSTR::null());
        }
    }
}

/// The glass was turned on or off (in Settings, or as the window first shows): on, Windows is asked to leave the
/// window out of captures and dvfiles looks behind it; off, or where Windows cannot, the frame is solid.
pub fn glass_setting_changed() {
    with_app(|a| {
        a.glass_live = glass::start(a.main, a.settings.glass);
        a.backdrop = None;
        a.caption = None;
        if a.glass_live {
            glass::look(a);
        }
        if !glass::on(a) {
            tint_frame(a.main, a.theme.chrome, a.theme.fg, a.theme.frame);
        }
        glass_moved(a);
    });
}

/// Looks behind the window again (it moved, was resized or came forward, or the timer ticked) and redraws the glass
/// when what is behind changed.
pub(super) fn glass_look(a: &mut App) {
    glass::look(a);
}

/// The glass shows something new: the title bar takes the glass's colour and the glass panes are drawn again.
pub(super) fn glass_moved(a: &mut App) {
    let c = glass::caption_colour(a);
    if c != a.caption {
        a.caption = c;
        tint_frame(a.main, c.unwrap_or(a.theme.chrome), a.theme.fg, a.theme.frame);
    }
    invalidate(a.nav);
    invalidate(a.main);
    // the card's rounded corners show the glass too
    invalidate(a.list);
    if !a.preview.0.is_null() {
        invalidate(a.preview);
    }
}

/// What lies behind the card at `r` of the pane `hwnd`: the glass, or the frame's colour.
pub fn backdrop(a: &App, surf: &Surface, hwnd: HWND, r: R) {
    if !glass::paint(a, surf, hwnd, r) {
        surf.fill(r, a.theme.chrome);
    }
}

/// Rounds the card's corners on a pane's surface (the list, or the preview), `corners` as `Surface::round_corners`
/// takes them, with the card's border along the curve.
pub fn card_corners(a: &App, surf: &Surface, hwnd: HWND, w: i32, h: i32, corners: u8) {
    surf.set_clip(None);
    surf.round_corners(R::new(0, 0, w, h), a.rad_card(), corners, Some(a.theme.card_border), &|s, r| {
        backdrop(a, s, hwnd, r)
    });
}

/// Colours the title bar like the toolbar under it, so the top of the window is one surface (Windows 11; older
/// versions ignore it and keep their own title bar).
pub fn tint_frame(hwnd: HWND, caption: Rgb, text: Rgb, border: Rgb) {
    use windows::Win32::Graphics::Dwm::{DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR};
    unsafe {
        let (c, t, b) = (caption.colorref(), text.colorref(), border.colorref());
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, &c as *const COLORREF as *const _, 4);
        // a visible edge, so the window stands apart from what is behind it
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &b as *const COLORREF as *const _, 4);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_TEXT_COLOR, &t as *const COLORREF as *const _, 4);
    }
}

pub fn set_theme(mode: ThemeMode) {
    let hw = with_app(|a| {
        a.settings.theme = mode;
        a.theme = Theme::for_mode(mode);
        a.art.clear();
        unsafe {
            let _ = DeleteObject(a.input_brush.into());
            a.input_brush = CreateSolidBrush(a.theme.input_bg.colorref());
        }
        a.bar.dirty = true;
        (vec![a.list, a.nav, a.addr, a.search], a.main, a.theme)
    });
    crate::trace::log(|| format!("theme {mode:?}: {}", hw.is_some()));
    if let Some((children, main, theme)) = hw {
        apply_theme(&children, main, &theme, mode);
        unsafe {
            let _ = SetWindowPos(main, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED);
            let _ = RedrawWindow(Some(main), None, None, RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_FRAME);
        }
        opsui::retheme();
        props::retheme();
        prefs::retheme();
        // the glass is veiled with the new theme's colours
        with_app(|a| {
            if glass::on(a) {
                a.caption = None;
                glass_moved(a);
            }
        });
    }
}
