// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! The progress window of a copy, move or delete, in the style of Windows 10's: a speed graph, what is being copied,
//! time and items left, Pause and Cancel. Name conflicts are answered inside the same window ("Replace", "Skip",
//! "Keep both", optionally for all) instead of a second dialog. Quick jobs never show it: the window appears only
//! when a job is still running after 400 ms, or when it has a question.
use crate::ui::art::{Art, Mark, Tool};
use crate::ui::gfx::{self, Surface, R};
use crate::ui::{invalidate, post_box, take_box, with_app, wstr, x_of, y_of};
use dvfiles::fmt;
use dvfiles::ops::{Asker, ConflictInfo, Kind, Progress, Resolution};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::time::Instant;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::WindowsAndMessaging::*;

const WM_ASK: u32 = WM_APP + 30;
const T_TICK: usize = 1;

struct AskMsg {
    info: ConflictInfo,
    reply: Sender<(Resolution, bool)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Btn {
    Pause,
    Cancel,
    Details,
    Replace,
    Skip,
    KeepBoth,
    ApplyAll,
    Close,
}

struct Op {
    kind: Kind,
    title: String,
    progress: Arc<Progress>,
    started: Instant,
    shown: bool,
    details: bool,
    samples: Vec<f32>,
    last_sample: (Instant, u64),
    speed: f64,
    peak: f64,
    ask: Option<AskMsg>,
    apply_all: bool,
    finished: bool,
    errors: Vec<(String, String)>,
    hot: Option<Btn>,
    hits: Vec<(Btn, R)>,
    surface: Option<Surface>,
    tracking: bool,
    // what the window shows glides toward the real values, so nothing jumps between samples
    eased: Eased,
}

#[derive(Default, Clone, Copy)]
struct Eased {
    frac: f64,
    speed: f64,
    peak: f64,
    at: Option<Instant>,
}

thread_local! {
    static OPS: RefCell<HashMap<isize, Op>> = RefCell::new(HashMap::new());
}

fn with_op<R>(h: HWND, f: impl FnOnce(&mut Op) -> R) -> Option<R> {
    OPS.with(|m| m.try_borrow_mut().ok()?.get_mut(&(h.0 as isize)).map(f))
}

pub struct UiAsker {
    pub dialog: isize,
}

impl Asker for UiAsker {
    fn conflict(&self, info: &ConflictInfo) -> (Resolution, bool) {
        let (tx, rx) = channel();
        let msg = AskMsg {
            info: ConflictInfo {
                src: info.src.clone(),
                dst: info.dst.clone(),
                src_size: info.src_size,
                dst_size: info.dst_size,
                src_modified: info.src_modified,
                dst_modified: info.dst_modified,
            },
            reply: tx,
        };
        post_box(HWND(self.dialog as *mut _), WM_ASK, msg);
        rx.recv().unwrap_or((Resolution::Skip, true))
    }
}

pub fn register() {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let icon = crate::ui::app_icon();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc_),
            hInstance: hinst.into(),
            hIcon: icon,
            hIconSm: icon,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("dvfilesOps"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
    }
}

fn short_name(p: &str) -> String {
    let n = dvfiles::pathx::file_name(p);
    if n.is_empty() {
        p.to_string()
    } else {
        n.to_string()
    }
}

fn title_for(kind: Kind, sources: &[String], dest: Option<&str>) -> String {
    let n = sources.len();
    let what = if n == 1 { format!("\"{}\"", short_name(&sources[0])) } else { format!("{n} items") };
    let from = sources.first().and_then(|s| dvfiles::pathx::parent(s)).map(|p| short_name(&p)).unwrap_or_default();
    let to = dest.map(short_name).unwrap_or_default();
    match kind {
        Kind::Copy => format!("Copying {what} from {from} to {to}"),
        Kind::Move => format!("Moving {what} from {from} to {to}"),
        Kind::Delete => format!("Deleting {what}"),
        Kind::Recycle => format!("Moving {what} to the Recycle Bin"),
    }
}

/// creates the (hidden) window for a job and returns it
pub fn open(owner: HWND, kind: Kind, sources: &[String], dest: Option<&str>, progress: Arc<Progress>) -> isize {
    let scale = with_app(|a| a.scale).unwrap_or(1.0);
    let title = title_for(kind, sources, dest);
    let (w, h) = ((480.0 * scale) as i32, (230.0 * scale) as i32);
    let hwnd = unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("dvfilesOps"),
            PCWSTR(wstr(kind.verb()).as_ptr()),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            w,
            h,
            Some(owner),
            None,
            Some(hinst.into()),
            None,
        )
        .unwrap_or_default()
    };
    let op = Op {
        kind,
        title,
        progress,
        started: Instant::now(),
        shown: false,
        details: true,
        samples: Vec::new(),
        last_sample: (Instant::now(), 0),
        speed: 0.0,
        peak: 1.0,
        ask: None,
        apply_all: false,
        finished: false,
        errors: Vec::new(),
        hot: None,
        hits: Vec::new(),
        surface: None,
        tracking: false,
        eased: Eased::default(),
    };
    OPS.with(|m| m.borrow_mut().insert(hwnd.0 as isize, op));
    dark_frame(hwnd);
    unsafe {
        // about 60 frames a second while the job runs, for the smooth progress; the window goes when the job ends
        SetTimer(Some(hwnd), T_TICK, 16, None);
    }
    hwnd.0 as isize
}

fn dark_frame(hwnd: HWND) {
    let Some(t) = with_app(|a| a.theme) else { return };
    let dark = t.dark as i32;
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const i32 as *const _, 4);
    }
    crate::ui::tint_frame(hwnd, t.bg, t.fg, t.frame);
}

pub fn retheme() {
    let all: Vec<isize> = OPS.with(|m| m.borrow().keys().copied().collect());
    for h in all {
        let h = HWND(h as *mut _);
        dark_frame(h);
        invalidate(h);
    }
}

/// the job thread has ended
pub fn finished(dialog: isize) {
    let h = HWND(dialog as *mut _);
    let keep = with_op(h, |op| {
        op.finished = true;
        op.errors = std::mem::take(&mut *op.progress.errors.lock().unwrap());
        if let Some(a) = op.ask.take() {
            let _ = a.reply.send((Resolution::Skip, true));
        }
        !op.errors.is_empty() && !op.progress.cancelled()
    });
    match keep {
        Some(true) => {
            show(h, true);
            fit(h);
            invalidate(h);
        }
        _ => unsafe {
            let _ = DestroyWindow(h);
        },
    }
}

pub fn dialog_message(_msg: &MSG) -> bool {
    false
}

fn show(h: HWND, activate: bool) {
    let first = with_op(h, |op| {
        let first = !op.shown;
        op.shown = true;
        first
    })
    .unwrap_or(false);
    unsafe {
        if first {
            // centered over the main window
            if let Some(main) = with_app(|a| a.main) {
                let (mut mr, mut wr) = (RECT::default(), RECT::default());
                let _ = GetWindowRect(main, &mut mr);
                let _ = GetWindowRect(h, &mut wr);
                let (w, ht) = (wr.right - wr.left, wr.bottom - wr.top);
                let x = mr.left + (mr.right - mr.left - w) / 2;
                let y = mr.top + (mr.bottom - mr.top - ht) / 3;
                let _ = SetWindowPos(h, None, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
        let _ = ShowWindow(h, if activate { SW_SHOW } else { SW_SHOWNOACTIVATE });
        if activate {
            let _ = SetForegroundWindow(h);
        }
    }
}

/// resizes the window to what it shows now
fn fit(h: HWND) {
    let scale = with_app(|a| a.scale).unwrap_or(1.0);
    let height = with_op(h, |op| {
        let base = if op.ask.is_some() {
            330.0
        } else if op.finished && !op.errors.is_empty() {
            150.0 + 20.0 * op.errors.len().min(8) as f32 + 50.0
        } else if op.details && matches!(op.kind, Kind::Copy | Kind::Move) {
            252.0
        } else {
            170.0
        };
        (base * scale) as i32
    });
    let Some(client_h) = height else { return };
    unsafe {
        let mut r = RECT { left: 0, top: 0, right: (480.0 * scale) as i32, bottom: client_h };
        let _ = AdjustWindowRectEx(
            &mut r,
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            false,
            WINDOW_EX_STYLE(0),
        );
        let _ =
            SetWindowPos(h, None, 0, 0, r.right - r.left, r.bottom - r.top, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
}

fn tick(h: HWND) {
    let show_now = with_op(h, |op| {
        let p = &op.progress;
        let now = Instant::now();
        let done = p.done_bytes.load(Ordering::Relaxed);
        let dt = now.duration_since(op.last_sample.0).as_secs_f64();
        if dt >= 0.125 {
            let raw = done.saturating_sub(op.last_sample.1) as f64 / dt;
            // each point is lightly averaged with the one before, so the curve follows the speed without jitter
            let rate = op.samples.last().map_or(raw, |&prev| prev as f64 * 0.4 + raw * 0.6);
            // the average so far until there are enough samples, then a moving average that follows changes
            op.speed = if op.samples.len() < 8 {
                done as f64 / op.started.elapsed().as_secs_f64().max(0.05)
            } else {
                op.speed * 0.75 + rate * 0.25
            };
            op.peak = op.peak.max(rate);
            op.samples.push(rate as f32);
            // keep the whole job in view: halve the resolution when it gets long
            if op.samples.len() > 800 {
                op.samples = op.samples.chunks(2).map(|c| c.iter().sum::<f32>() / c.len() as f32).collect();
            }
            op.last_sample = (now, done);
        }
        // ease toward the real values: a fifth of a second to settle, whatever the frame rate
        let (db, tb) = (p.done_bytes.load(Ordering::Relaxed), p.total_bytes.load(Ordering::Relaxed));
        let (di, ti) = (p.done_items.load(Ordering::Relaxed), p.total_items.load(Ordering::Relaxed));
        let frac = if matches!(op.kind, Kind::Copy | Kind::Move) && tb > 0 {
            db as f64 / tb as f64
        } else if ti > 0 {
            di as f64 / ti as f64
        } else {
            0.0
        };
        let dt = op.eased.at.map_or(1.0, |t| now.duration_since(t).as_secs_f64());
        let k = 1.0 - (-dt / 0.2).exp();
        let e = &mut op.eased;
        e.frac += (frac.clamp(0.0, 1.0) - e.frac) * k;
        e.speed += (op.speed - e.speed) * k;
        e.peak += (op.peak - e.peak) * k;
        e.at = Some(now);
        !op.shown && !op.finished && op.started.elapsed().as_millis() >= 400
    });
    if show_now == Some(true) {
        fit(h);
        show(h, false);
    }
    // a hidden window (a job quicker than 400 ms) is not redrawn
    if with_op(h, |op| op.shown).unwrap_or(false) {
        invalidate(h);
    }
}

/// the window's icons, fetched from the app's cache before drawing
struct Pics {
    pause: std::rc::Rc<crate::ui::thumbs::Image>,
    play: std::rc::Rc<crate::ui::thumbs::Image>,
    cancel: std::rc::Rc<crate::ui::thumbs::Image>,
    up: std::rc::Rc<crate::ui::thumbs::Image>,
    down: std::rc::Rc<crate::ui::thumbs::Image>,
    replace: std::rc::Rc<crate::ui::thumbs::Image>,
    skip: std::rc::Rc<crate::ui::thumbs::Image>,
    keep: std::rc::Rc<crate::ui::thumbs::Image>,
}

fn paint(h: HWND) {
    let Some((theme, fonts, scale, nf, pics)) = with_app(|a| {
        let t = a.theme;
        let (m, big) = (a.s(14.0), a.s(26.0));
        let pics = Pics {
            pause: a.art.mark(Mark::Pause, m, t.fg),
            play: a.art.mark(Mark::Play, m, t.fg),
            cancel: a.art.mark(Mark::Close, m, t.danger),
            up: a.art.mark(Mark::Up, a.s(12.0), t.muted),
            down: a.art.mark(Mark::Down, a.s(12.0), t.muted),
            replace: a.art.get(Art::Tool(Tool::Tick), big, &t),
            skip: a.art.mark(Mark::Forward, a.s(20.0), t.accent),
            keep: a.art.get(Art::Tool(Tool::Copy), big, &t),
        };
        (t, (a.fonts.ui, a.fonts.title, a.fonts.bold), a.scale, a.nf, pics)
    }) else {
        return;
    };
    let (ui, title_font, bold) = fonts;
    let (w, ht) = crate::ui::client_size(h);
    with_op(h, |op| {
        if Surface::ensure(&mut op.surface, w, ht).is_none() {
            return;
        }
        let s = |v: f32| (v * scale).round() as i32;
        let t = theme;
        let surf = op.surface.as_ref().unwrap();
        let mut hits = Vec::new();
        surf.fill(R::new(0, 0, w, ht), t.bg);
        let p = &op.progress;
        let (db, tb) = (p.done_bytes.load(Ordering::Relaxed), p.total_bytes.load(Ordering::Relaxed));
        let (di, ti) = (p.done_items.load(Ordering::Relaxed), p.total_items.load(Ordering::Relaxed));
        let by_bytes = matches!(op.kind, Kind::Copy | Kind::Move) && tb > 0;
        // the eased value (see tick), so the bar and graph glide instead of stepping
        let frac = op.eased.frac;
        let pad = s(18.0);
        surf.text(R::new(pad, s(10.0), w - 2 * pad, s(22.0)), &op.title, ui, t.fg, gfx::LEFT);

        if op.finished {
            surf.text(
                R::new(pad, s(36.0), w - 2 * pad, s(28.0)),
                "Some items could not be processed",
                title_font,
                t.danger,
                gfx::LEFT,
            );
            let mut y = s(72.0);
            for (path, e) in op.errors.iter().take(8) {
                let line = format!("{}  -  {e}", short_name(path));
                surf.text(R::new(pad, y, w - 2 * pad, s(20.0)), &line, ui, t.fg, gfx::LEFT);
                y += s(20.0);
            }
            if op.errors.len() > 8 {
                surf.text(
                    R::new(pad, y, w - 2 * pad, s(20.0)),
                    &format!("and {} more", op.errors.len() - 8),
                    ui,
                    t.muted,
                    gfx::LEFT,
                );
            }
            let r = R::new(w - pad - s(90.0), ht - s(42.0), s(90.0), s(30.0));
            button(surf, r, "Close", op.hot == Some(Btn::Close), true, &t, ui, scale);
            hits.push((Btn::Close, r));
            op.hits = hits;
            surf.present(h);
            return;
        }

        let pct = format!("{}% complete", (frac * 100.0).floor().clamp(0.0, 100.0) as u32);
        let status = if p.paused.load(Ordering::Relaxed) {
            "Paused".to_string()
        } else if op.ask.is_some() {
            "Waiting for your answer".to_string()
        } else {
            pct
        };
        surf.text(R::new(pad, s(34.0), w - 2 * pad - s(80.0), s(30.0)), &status, title_font, t.fg, gfx::LEFT);
        // Pause and Cancel
        let paused = p.paused.load(Ordering::Relaxed);
        let rp = R::new(w - pad - s(68.0), s(34.0), s(30.0), s(30.0));
        let rc = R::new(w - pad - s(32.0), s(34.0), s(30.0), s(30.0));
        for (b, r, g) in
            [(Btn::Pause, rp, if paused { &pics.play } else { &pics.pause }), (Btn::Cancel, rc, &pics.cancel)]
        {
            if op.hot == Some(b) {
                surf.round(r, s(5.0) as f32, t.btn_hover);
            }
            surf.image_in(r, g);
            hits.push((b, r));
        }

        let mut y = s(72.0);
        if let Some(ask) = &op.ask {
            let name = short_name(&ask.info.dst);
            surf.text(
                R::new(pad, y, w - 2 * pad, s(20.0)),
                &format!("The destination already has a file named \"{name}\""),
                bold,
                t.fg,
                gfx::LEFT,
            );
            y += s(28.0);
            let fmt_side = |size: u64, m: u64| format!("{}, modified {}", nf.bytes(size), crate::ui::locale::short(m));
            let folder_of = |p: &str| dvfiles::pathx::parent(p).map(|d| short_name(&d)).unwrap_or_default();
            let (from, to) = (folder_of(&ask.info.src), folder_of(&ask.info.dst));
            let options = [
                (
                    Btn::Replace,
                    &pics.replace,
                    "Replace the file in the destination",
                    format!("With the one from {from}: {}", fmt_side(ask.info.src_size, ask.info.src_modified)),
                ),
                (
                    Btn::Skip,
                    &pics.skip,
                    "Skip this file",
                    format!("Keep the one in {to}: {}", fmt_side(ask.info.dst_size, ask.info.dst_modified)),
                ),
                (
                    Btn::KeepBoth,
                    &pics.keep,
                    "Keep both files",
                    "The copy is renamed with a number, e.g. \"name (2)\"".to_string(),
                ),
            ];
            for (b, g, label, sub) in options {
                let r = R::new(pad, y, w - 2 * pad, s(46.0));
                let hot = op.hot == Some(b);
                surf.round(r, s(5.0) as f32, if hot { t.btn_hover } else { t.bar_bg });
                surf.image_in(R::new(r.x + s(8.0), r.y, s(26.0), r.h), g);
                surf.text(R::new(r.x + s(42.0), r.y + s(4.0), r.w - s(50.0), s(20.0)), label, ui, t.fg, gfx::LEFT);
                surf.text(R::new(r.x + s(42.0), r.y + s(22.0), r.w - s(50.0), s(20.0)), &sub, ui, t.muted, gfx::LEFT);
                hits.push((b, r));
                y += s(52.0);
            }
            let cb = R::new(pad, y + s(4.0), s(16.0), s(16.0));
            surf.frame(cb, if op.hot == Some(Btn::ApplyAll) { t.accent } else { t.muted });
            if op.apply_all {
                surf.fill(cb.inset(s(3.0), s(3.0)), t.accent);
            }
            surf.text(
                R::new(cb.right() + s(8.0), y, w - 2 * pad, s(24.0)),
                "Do this for all conflicts",
                ui,
                t.fg,
                gfx::LEFT,
            );
            hits.push((Btn::ApplyAll, R::new(pad, y, s(220.0), s(24.0))));
            op.hits = hits;
            surf.present(h);
            return;
        }

        let graph = op.details && by_bytes;
        if graph {
            let g = R::new(pad, y, w - 2 * pad, s(80.0));
            // Windows 10's copy graph: the done part tinted green, the speed history as a green area, and a line at
            // the current speed with its value
            surf.fill(g, t.bg);
            surf.frame(g, t.card_border);
            let peak = op.eased.peak.max(1.0) as f32 * 1.15;
            // the samples are smoothed (1-4-6-4-1) before the curve is drawn, so changes become gentle bends
            let n = op.samples.len() as isize;
            let at = |i: isize| op.samples[i.clamp(0, n - 1) as usize];
            let vals: Vec<f32> = (0..n)
                .map(|i| (at(i - 2) + 4.0 * at(i - 1) + 6.0 * at(i) + 4.0 * at(i + 1) + at(i + 2)) / 16.0 / peak)
                .collect();
            let extent = (g.w as f64 * frac.clamp(0.0, 1.0)) as f32;
            surf.set_clip(Some(g.inset(1, 1)));
            surf.fill_frac(g, extent, t.bg.mix(t.progress_fill, 0.45));
            surf.area(
                R::new(g.x, g.y + s(4.0), g.w, g.h - s(4.0)),
                extent,
                &vals,
                t.progress_fill,
                t.progress,
                1.6 * scale,
            );
            let settled = op.started.elapsed().as_secs_f64() > 1.2 && op.samples.len() >= 4;
            if settled {
                let lyf = g.bottom() as f32 - (op.eased.speed as f32 / peak).clamp(0.0, 1.0) * (g.h - s(4.0)) as f32;
                surf.hline(g.x, lyf, g.w, t.fg, 0.45);
                let ly = lyf as i32;
                let label = format!("Speed: {}", nf.speed(op.eased.speed));
                let ty = (ly - s(19.0)).max(g.y + s(2.0));
                surf.text(R::new(g.x + s(8.0), ty, g.w - s(16.0), s(18.0)), &label, ui, t.fg, gfx::RIGHT);
            }
            surf.set_clip(None);
            y += s(90.0);
        } else {
            let bar = R::new(pad, y, w - 2 * pad, s(8.0));
            surf.fill(bar, t.line);
            surf.fill_frac(bar, (bar.w as f64 * frac.clamp(0.0, 1.0)) as f32, t.progress);
            y += s(18.0);
        }
        let current = p.current.lock().map(|c| c.clone()).unwrap_or_default();
        let left_items = ti.saturating_sub(di);
        let remaining = if by_bytes {
            let left = tb.saturating_sub(db);
            // the first second's rate says little; Windows waits too
            if p.scanning.load(Ordering::Relaxed) || op.speed < 1.0 || op.started.elapsed().as_secs_f64() < 1.2 {
                "Calculating...".to_string()
            } else {
                fmt::time_left(left as f64 / op.speed)
            }
        } else {
            String::new()
        };
        let mut lines = vec![("Name:", current)];
        if by_bytes {
            lines.push(("Time remaining:", remaining));
            lines.push(("Items remaining:", format!("{} ({})", nf.group(left_items), nf.bytes(tb.saturating_sub(db)))));
        } else {
            lines.push(("Items remaining:", nf.group(left_items)));
        }
        for (k, v) in lines {
            surf.text(R::new(pad, y, s(110.0), s(20.0)), k, ui, t.muted, gfx::LEFT);
            surf.text(R::new(pad + s(112.0), y, w - 2 * pad - s(112.0), s(20.0)), &v, ui, t.fg, gfx::LEFT);
            y += s(20.0);
        }
        if matches!(op.kind, Kind::Copy | Kind::Move) {
            let r = R::new(pad - s(4.0), ht - s(30.0), s(130.0), s(24.0));
            let label = if op.details { "Fewer details" } else { "More details" };
            let g = if op.details { &pics.up } else { &pics.down };
            surf.image_in(R::new(r.x, r.y, s(20.0), r.h), g);
            surf.text(
                R::new(r.x + s(22.0), r.y, r.w, r.h),
                label,
                ui,
                if op.hot == Some(Btn::Details) { t.accent } else { t.fg },
                gfx::LEFT,
            );
            hits.push((Btn::Details, r));
        }
        op.hits = hits;
        surf.present(h);
    });
}

fn button(
    surf: &Surface,
    r: R,
    label: &str,
    hot: bool,
    primary: bool,
    t: &gfx::Theme,
    font: windows::Win32::Graphics::Gdi::HFONT,
    scale: f32,
) {
    let bg = if primary {
        if hot {
            t.primary_hot
        } else {
            t.primary
        }
    } else if hot {
        t.btn_hover
    } else {
        t.bar_bg
    };
    surf.round(r, (5.0 * scale).round(), bg);
    surf.text(r, label, font, if primary { t.primary_fg } else { t.fg }, gfx::CENTER);
}

fn answer(h: HWND, r: Resolution) {
    with_op(h, |op| {
        if let Some(a) = op.ask.take() {
            let _ = a.reply.send((r, op.apply_all));
        }
    });
    fit(h);
    invalidate(h);
}

fn click(h: HWND, x: i32, y: i32) {
    crate::trace::log(|| {
        format!(
            "ops click {x},{y} hits={:?}",
            with_op(h, |op| op.hits.iter().map(|(b, r)| (*b, *r)).collect::<Vec<_>>())
        )
    });
    let Some(b) = with_op(h, |op| op.hits.iter().find(|(_, r)| r.contains(x, y)).map(|(b, _)| *b)).flatten() else {
        return;
    };
    match b {
        Btn::Pause => {
            with_op(h, |op| {
                let p = !op.progress.paused.load(Ordering::Relaxed);
                op.progress.set_paused(p);
            });
        }
        Btn::Cancel => cancel(h),
        Btn::Details => {
            with_op(h, |op| op.details = !op.details);
            fit(h);
        }
        Btn::Replace => answer(h, Resolution::Replace),
        Btn::Skip => answer(h, Resolution::Skip),
        Btn::KeepBoth => answer(h, Resolution::KeepBoth),
        Btn::ApplyAll => {
            with_op(h, |op| op.apply_all = !op.apply_all);
        }
        Btn::Close => unsafe {
            let _ = DestroyWindow(h);
        },
    }
    invalidate(h);
}

fn cancel(h: HWND) {
    with_op(h, |op| {
        op.progress.cancel();
        if let Some(a) = op.ask.take() {
            let _ = a.reply.send((Resolution::Skip, true));
        }
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
            WM_TIMER => {
                tick(hwnd);
                LRESULT(0)
            }
            WM_ASK => {
                let m = take_box::<AskMsg>(lp);
                with_op(hwnd, |op| op.ask = Some(*m));
                fit(hwnd);
                show(hwnd, true);
                invalidate(hwnd);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let (x, y) = (x_of(lp), y_of(lp));
                with_op(hwnd, |op| {
                    if !op.tracking {
                        let mut tme = TRACKMOUSEEVENT {
                            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        let _ = TrackMouseEvent(&mut tme);
                        op.tracking = true;
                    }
                    let hot = op.hits.iter().find(|(_, r)| r.contains(x, y)).map(|(b, _)| *b);
                    if hot != op.hot {
                        op.hot = hot;
                        invalidate(hwnd);
                    }
                });
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                with_op(hwnd, |op| {
                    op.tracking = false;
                    op.hot = None;
                });
                invalidate(hwnd);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                click(hwnd, x_of(lp), y_of(lp));
                LRESULT(0)
            }
            WM_KEYDOWN => {
                if wp.0 as u16 == windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE.0 {
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                let done = with_op(hwnd, |op| op.finished).unwrap_or(true);
                if done {
                    let _ = DestroyWindow(hwnd);
                } else {
                    cancel(hwnd);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                let _ = KillTimer(Some(hwnd), T_TICK);
                OPS.with(|m| {
                    if let Some(op) = m.borrow_mut().remove(&(hwnd.0 as isize)) {
                        // a window closed mid-job cancels it rather than leaving a question unanswered
                        if !op.finished {
                            op.progress.cancel();
                        }
                        if let Some(a) = op.ask {
                            let _ = a.reply.send((Resolution::Skip, true));
                        }
                    }
                });
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
