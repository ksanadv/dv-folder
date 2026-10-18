// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! dvfiles' own photo viewer: opening a picture (JPEG, PNG, GIF, BMP) shows it here, decoded by dvfiles' own
//! decoders on a thread, fitted to the window on a dark ground. Left and Right (or the wheel) go through the folder's
//! other pictures in the list's order, the next one decoded ahead so stepping on is instant. Ctrl+wheel, + and -
//! zoom (1 is actual size, 0 fits), dragging moves a zoomed picture, R and Shift+R turn it, Delete recycles it,
//! F11 or a double click fills the screen, Esc closes. Turned off in Settings, pictures open in their own program.
use crate::ui::gfx::{self, Rgb, Surface, R};
use crate::ui::thumbs::Image;
use crate::ui::{invalidate, key_down, post_box, take_box, with_app, wstr, x_of, y_of};
use dvfiles::img::Decoded;
use std::cell::RefCell;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_PICTURE: u32 = WM_APP + 41;
const GROUND: Rgb = Rgb(0x14, 0x14, 0x14);
const ZOOM_STEP: f32 = 1.25;
const ZOOM_MOST: f32 = 32.0;

struct Picture {
    path: String,
    img: Option<Decoded>,
}

#[derive(Default)]
struct View {
    hwnd: HWND,
    paths: Vec<String>,
    at: usize,
    // the path being decoded for the screen, so a slow decode of a picture already left behind is dropped
    wanted: Option<String>,
    full: Option<Decoded>,
    // the next picture, decoded while this one is looked at
    ahead: Option<(String, Option<Decoded>)>,
    // the picture fitted to the window, made again when the window's size changes
    fitted: Option<(i32, i32, Image)>,
    // screen pixels per picture pixel while zoomed, and the picture point at the window's middle
    zoom: Option<f32>,
    centre: (f32, f32),
    drag: Option<(i32, i32, (f32, f32))>,
    surface: Option<Surface>,
    // the window's place before it filled the screen
    restore: Option<WINDOWPLACEMENT>,
}

thread_local! {
    static VIEW: RefCell<Option<View>> = const { RefCell::new(None) };
}

fn with_view<T>(f: impl FnOnce(&mut View) -> T) -> Option<T> {
    VIEW.with(|v| v.try_borrow_mut().ok()?.as_mut().map(f))
}

pub fn register() {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let icon = crate::ui::app_icon();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(proc_),
            hInstance: hinst.into(),
            hIcon: icon,
            hIconSm: icon,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("dvfilesViewer"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
    }
}

/// Shows `paths[at]`, with the others a key away. The viewer window is made the first time and kept while open.
pub fn open(paths: Vec<String>, at: usize) {
    let hwnd = match with_view(|v| v.hwnd) {
        Some(h) => h,
        None => {
            let (mw, mh) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
            let (w, h) = (mw * 3 / 4, mh * 3 / 4);
            let hwnd = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("dvfilesViewer"),
                    w!("dvfiles"),
                    WS_OVERLAPPEDWINDOW,
                    (mw - w) / 2,
                    (mh - h) / 2,
                    w,
                    h,
                    None,
                    None,
                    Some(GetModuleHandleW(None).unwrap_or_default().into()),
                    None,
                )
                .unwrap_or_default()
            };
            let dark = 1i32;
            unsafe {
                let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const i32 as *const _, 4);
            }
            crate::ui::tint_frame(hwnd, GROUND, Rgb::hex(0xEDEDED), Rgb::hex(0x333333));
            VIEW.with(|v| *v.borrow_mut() = Some(View { hwnd, ..Default::default() }));
            unsafe {
                let _ = ShowWindow(hwnd, if crate::ui::quiet() { SW_SHOWNOACTIVATE } else { SW_SHOW });
            }
            hwnd
        }
    };
    with_view(|v| {
        v.paths = paths;
        v.at = at.min(v.paths.len().saturating_sub(1));
    });
    load(hwnd);
    if !crate::ui::quiet() {
        unsafe {
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

/// reads and decodes a picture on a thread, at most as large as the screen
fn decode(hwnd: HWND, path: String) {
    let (mw, mh) = unsafe { (GetSystemMetrics(SM_CXSCREEN) as u32, GetSystemMetrics(SM_CYSCREEN) as u32) };
    let h = hwnd.0 as isize;
    std::thread::spawn(move || {
        let img =
            std::fs::read(&path).ok().filter(|d| d.len() < 256 << 20).and_then(|d| dvfiles::img::thumbnail(&d, mw, mh));
        post_box(HWND(h as *mut _), WM_PICTURE, Picture { path, img });
    });
}

/// shows the picture at `at`: at once when it was decoded ahead, else after its decode
fn load(hwnd: HWND) {
    let Some(Some((path, ready))) = with_view(|v| {
        let path = v.paths.get(v.at).cloned()?;
        v.zoom = None;
        v.fitted = None;
        let ready = match v.ahead.take() {
            Some((p, img)) if p == path => {
                v.full = img;
                v.wanted = None;
                true
            }
            _ => {
                v.full = None;
                v.wanted = Some(path.clone());
                false
            }
        };
        Some((path, ready))
    }) else {
        return;
    };
    title(hwnd);
    invalidate(hwnd);
    if ready {
        look_ahead(hwnd);
    } else {
        decode(hwnd, path);
    }
}

/// decodes the next picture while this one is looked at, so Right shows it at once
fn look_ahead(hwnd: HWND) {
    let next = with_view(|v| {
        let n = v.paths.len();
        (n > 1).then(|| v.paths[(v.at + 1) % n].clone()).filter(|p| v.ahead.as_ref().is_none_or(|(a, _)| a != p))
    })
    .flatten();
    if let Some(p) = next {
        decode(hwnd, p);
    }
}

fn title(hwnd: HWND) {
    let Some(t) = with_view(|v| {
        let name = v.paths.get(v.at).map(|p| dvfiles::pathx::file_name(p).to_string()).unwrap_or_default();
        let zoom = v.zoom.map(|z| format!(" - {:.0}%", z * 100.0)).unwrap_or_default();
        format!("{name}{zoom} - {} of {} - dvfiles", v.at + 1, v.paths.len())
    }) else {
        return;
    };
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(wstr(&t).as_ptr()));
    }
}

/// to the picture `by` steps on, wrapping round
fn go(hwnd: HWND, by: isize) {
    let moved = with_view(|v| {
        let n = v.paths.len() as isize;
        if n < 2 {
            return false;
        }
        v.at = (v.at as isize + by).rem_euclid(n) as usize;
        true
    });
    if moved == Some(true) {
        load(hwnd);
    }
}

fn go_to(hwnd: HWND, last: bool) {
    with_view(|v| v.at = if last { v.paths.len().saturating_sub(1) } else { 0 });
    load(hwnd);
}

/// the scale that fits the picture in the window, never above actual size
fn fit_scale(v: &View, cw: i32, ch: i32) -> f32 {
    v.full.as_ref().map_or(1.0, |d| (cw as f32 / d.w.max(1) as f32).min(ch as f32 / d.h.max(1) as f32).min(1.0))
}

/// zooms to what `to` gives from the current and the fitting scale (None fits), keeping the picture point under
/// (x, y) in the window where it is
fn zoom(hwnd: HWND, to: impl FnOnce(f32, f32) -> Option<f32>, at: Option<(i32, i32)>) {
    let (cw, ch) = crate::ui::client_size(hwnd);
    with_view(|v| {
        let Some(d) = &v.full else { return };
        let (dw, dh) = (d.w as f32, d.h as f32);
        let fit = fit_scale(v, cw, ch);
        let now = v.zoom.unwrap_or(fit);
        let new = to(now, fit).map(|z| z.clamp(fit, ZOOM_MOST)).filter(|&z| z > fit);
        let (px, py) = at.map_or((cw as f32 / 2.0, ch as f32 / 2.0), |(x, y)| (x as f32, y as f32));
        let centre = if v.zoom.is_some() { v.centre } else { (dw / 2.0, dh / 2.0) };
        let under = (centre.0 + (px - cw as f32 / 2.0) / now, centre.1 + (py - ch as f32 / 2.0) / now);
        v.zoom = new;
        if let Some(z) = new {
            v.centre = (under.0 - (px - cw as f32 / 2.0) / z, under.1 - (py - ch as f32 / 2.0) / z);
            clamp_centre(v, cw, ch);
        }
    });
    title(hwnd);
    invalidate(hwnd);
}

/// keeps a zoomed picture from being moved off the window
fn clamp_centre(v: &mut View, cw: i32, ch: i32) {
    let (Some(z), Some(d)) = (v.zoom, &v.full) else { return };
    let keep = |c: f32, size: f32, win: f32| {
        let half = win / 2.0 / z;
        if size <= 2.0 * half {
            size / 2.0
        } else {
            c.clamp(half, size - half)
        }
    };
    v.centre = (keep(v.centre.0, d.w as f32, cw as f32), keep(v.centre.1, d.h as f32, ch as f32));
}

/// R: a quarter turn of what is shown (the file is left alone)
fn turn(hwnd: HWND, clockwise: bool) {
    with_view(|v| {
        let Some(d) = &mut v.full else { return };
        let (w, h) = (d.w as usize, d.h as usize);
        let mut px = vec![0u32; w * h];
        for y in 0..h {
            for x in 0..w {
                let (nx, ny) = if clockwise { (h - 1 - y, x) } else { (y, w - 1 - x) };
                px[ny * h + nx] = d.px[y * w + x];
            }
        }
        *d = Decoded { w: h as u32, h: w as u32, px };
        v.fitted = None;
        v.zoom = None;
    });
    title(hwnd);
    invalidate(hwnd);
}

/// Delete: the picture goes to the Recycle Bin (Ctrl+Z in the folder puts it back), the next one shows
fn recycle(hwnd: HWND) {
    let Some(Some((path, left))) = with_view(|v| {
        if v.at >= v.paths.len() {
            return None;
        }
        let path = v.paths.remove(v.at);
        if v.at >= v.paths.len() {
            v.at = 0;
        }
        Some((path, v.paths.len()))
    }) else {
        return;
    };
    crate::ui::cmds::start_job(dvfiles::ops::Kind::Recycle, vec![path], None, None);
    if left == 0 {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    } else {
        load(hwnd);
    }
}

/// F11, a double click: the picture fills the screen, or back
fn toggle_full(hwnd: HWND) {
    unsafe {
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        if style & WS_OVERLAPPEDWINDOW.0 != 0 {
            let mut wp =
                WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
            let _ = GetWindowPlacement(hwnd, &mut wp);
            with_view(|v| v.restore = Some(wp));
            let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let _ = GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut mi);
            let m = mi.rcMonitor;
            SetWindowLongW(hwnd, GWL_STYLE, (style & !WS_OVERLAPPEDWINDOW.0) as i32);
            let _ =
                SetWindowPos(hwnd, Some(HWND_TOP), m.left, m.top, m.right - m.left, m.bottom - m.top, SWP_FRAMECHANGED);
        } else {
            SetWindowLongW(hwnd, GWL_STYLE, (style | WS_OVERLAPPEDWINDOW.0) as i32);
            if let Some(Some(wp)) = with_view(|v| v.restore.take()) {
                let _ = SetWindowPlacement(hwnd, &wp);
            }
            let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED);
        }
    }
}

/// the zoomed picture's part that shows, picked pixel by pixel (sharp, as zoom is for looking closely)
fn zoomed(d: &Decoded, z: f32, centre: (f32, f32), cw: i32, ch: i32) -> Vec<u32> {
    let mut out = vec![0u32; (cw * ch) as usize];
    let (x0, y0) = (centre.0 - cw as f32 / 2.0 / z, centre.1 - ch as f32 / 2.0 / z);
    let cols: Vec<Option<usize>> = (0..cw)
        .map(|x| {
            let sx = (x0 + (x as f32 + 0.5) / z).floor();
            (sx >= 0.0 && (sx as u32) < d.w).then_some(sx as usize)
        })
        .collect();
    for (y, row) in out.chunks_exact_mut(cw as usize).enumerate() {
        let sy = (y0 + (y as f32 + 0.5) / z).floor();
        if sy < 0.0 || sy as u32 >= d.h {
            continue;
        }
        let src = &d.px[sy as usize * d.w as usize..][..d.w as usize];
        for (o, c) in row.iter_mut().zip(&cols) {
            if let Some(c) = c {
                *o = src[*c];
            }
        }
    }
    out
}

fn paint(hwnd: HWND) {
    let (cw, ch) = crate::ui::client_size(hwnd);
    let Some(ui) = with_app(|a| a.fonts.ui) else { return };
    with_view(|v| {
        if Surface::ensure(&mut v.surface, cw, ch).is_none() {
            return;
        }
        // fitted to the window once for each size, never enlarged past the picture's own size
        if let (Some(full), None) = (&v.full, v.zoom) {
            if v.fitted.as_ref().is_none_or(|(w, h, _)| (*w, *h) != (cw, ch)) {
                let d = dvfiles::img::fit_ref(full, cw.max(1) as u32, ch.max(1) as u32);
                v.fitted = Some((cw, ch, Image { w: d.w as i32, h: d.h as i32, px: d.px.into_boxed_slice() }));
            }
        }
        let surf = v.surface.as_ref().expect("ensured above");
        surf.fill(R::new(0, 0, cw, ch), GROUND);
        match (&v.full, v.zoom, &v.fitted) {
            (Some(d), Some(z), _) => surf.image(0, 0, cw, ch, &zoomed(d, z, v.centre, cw, ch)),
            (Some(_), None, Some((_, _, img))) => surf.image_in(R::new(0, 0, cw, ch), img),
            (None, _, _) if v.wanted.is_some() => {}
            _ => {
                surf.text(R::new(0, 0, cw, ch), "dvfiles cannot show this picture", ui, Rgb::hex(0x9A9A9A), gfx::CENTER)
            }
        }
        surf.present(hwnd);
    });
}

extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
                paint(hwnd);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_SIZE => {
                let (cw, ch) = crate::ui::client_size(hwnd);
                with_view(|v| clamp_centre(v, cw, ch));
                invalidate(hwnd);
                LRESULT(0)
            }
            WM_PICTURE => {
                let p = take_box::<Picture>(lp);
                let shown = with_view(|v| {
                    if v.wanted.as_deref() == Some(p.path.as_str()) {
                        v.wanted = None;
                        v.full = p.img;
                        v.fitted = None;
                        true
                    } else {
                        v.ahead = Some((p.path, p.img));
                        false
                    }
                });
                if shown == Some(true) {
                    invalidate(hwnd);
                    look_ahead(hwnd);
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                let shift = key_down(VK_SHIFT);
                match VIRTUAL_KEY(wp.0 as u16) {
                    VK_RIGHT | VK_DOWN | VK_NEXT | VK_SPACE => go(hwnd, 1),
                    VK_LEFT | VK_UP | VK_PRIOR | VK_BACK => go(hwnd, -1),
                    VK_HOME => go_to(hwnd, false),
                    VK_END => go_to(hwnd, true),
                    VK_ADD | VK_OEM_PLUS => zoom(hwnd, |now, _| Some(now * ZOOM_STEP), None),
                    VK_SUBTRACT | VK_OEM_MINUS => zoom(hwnd, |now, _| Some(now / ZOOM_STEP), None),
                    VK_1 | VK_NUMPAD1 => zoom(hwnd, |_, _| Some(1.0), None),
                    VK_0 | VK_NUMPAD0 => zoom(hwnd, |_, _| None, None),
                    VK_R => turn(hwnd, !shift),
                    VK_DELETE => recycle(hwnd),
                    VK_F11 => toggle_full(hwnd),
                    VK_ESCAPE => {
                        if GetWindowLongW(hwnd, GWL_STYLE) as u32 & WS_OVERLAPPEDWINDOW.0 == 0 {
                            toggle_full(hwnd);
                        } else {
                            let _ = DestroyWindow(hwnd);
                        }
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = ((wp.0 >> 16) & 0xFFFF) as u16 as i16;
                if key_down(VK_CONTROL) {
                    let mut pt = POINT { x: x_of(lp), y: y_of(lp) };
                    let _ = ScreenToClient(hwnd, &mut pt);
                    let k = if delta > 0 { ZOOM_STEP } else { 1.0 / ZOOM_STEP };
                    zoom(hwnd, |now, _| Some(now * k), Some((pt.x, pt.y)));
                } else {
                    go(hwnd, if delta < 0 { 1 } else { -1 });
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let (x, y) = (x_of(lp), y_of(lp));
                if with_view(|v| v.zoom.map(|_| v.drag = Some((x, y, v.centre)))).flatten().is_some() {
                    SetCapture(hwnd);
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let (x, y) = (x_of(lp), y_of(lp));
                let (cw, ch) = crate::ui::client_size(hwnd);
                let moved = with_view(|v| {
                    let (Some((x0, y0, c)), Some(z)) = (v.drag, v.zoom) else { return false };
                    v.centre = (c.0 - (x - x0) as f32 / z, c.1 - (y - y0) as f32 / z);
                    clamp_centre(v, cw, ch);
                    true
                });
                if moved == Some(true) {
                    invalidate(hwnd);
                }
                LRESULT(0)
            }
            WM_LBUTTONUP | WM_CAPTURECHANGED => {
                if with_view(|v| v.drag.take()).flatten().is_some() && msg == WM_LBUTTONUP {
                    let _ = ReleaseCapture();
                }
                LRESULT(0)
            }
            WM_LBUTTONDBLCLK => {
                toggle_full(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                VIEW.with(|v| *v.borrow_mut() = None);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
