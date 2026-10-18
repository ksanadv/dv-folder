// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Open with, drawn by dvfiles: the programs Windows lists for the file's type (the same list its own "How do you
//! want to open this file?" box shows, from `SHAssocEnumHandlers`, Store apps included), recommended ones first with
//! the current default marked, another program by its path, and a link to Windows' Default apps for "always": only
//! Windows' Settings can change a default, which Windows guards with a hash no program may write.
//!
//! The chosen program is started by Windows (`IAssocHandler::Invoke`, or `ShellExecute` for a typed path), so Store
//! apps, the downloaded-file warning and SmartScreen work as they do from Explorer.
use crate::ui::gfx::{self, Surface, Theme, R};
use crate::ui::thumbs::Image;
use crate::ui::{wstr, x_of, y_of};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

struct Choice {
    name: String,
    icon: Rc<Image>,
    recommended: bool,
    default: bool,
    handler: IAssocHandler,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Row(usize),
    Another,
    Settings,
    Open,
    Cancel,
}

enum Answer {
    App(usize),
    Path(String),
    Settings,
    Cancel,
}

struct Ow {
    file: String,
    file_icon: Rc<Image>,
    apps: Vec<Choice>,
    sel: Option<usize>,
    hot: Option<Hit>,
    pressed: Option<Hit>,
    scroll: i32,
    // the box for another program's path, once asked for
    path_edit: Option<HWND>,
    answer: Option<Answer>,
    theme: Theme,
    ui: HFONT,
    bold: HFONT,
    title: HFONT,
    small: HFONT,
    scale: f32,
    surface: Option<Surface>,
}

thread_local! {
    static OW: RefCell<Option<Ow>> = const { RefCell::new(None) };
    static CLASS: Cell<bool> = const { Cell::new(false) };
}

// the parts of the window, in its own pixels
struct Layout {
    list: R,
    rows: Vec<(usize, R)>,
    labels: Vec<(&'static str, R)>,
    content_h: i32,
    another: R,
    path: R,
    settings: R,
    open: R,
    cancel: R,
}

impl Ow {
    fn s(&self, v: f32) -> i32 {
        (v * self.scale).round() as i32
    }

    fn footer_h(&self) -> i32 {
        self.s(60.0)
    }

    fn layout(&self, w: i32, h: i32) -> Layout {
        let s = |v: f32| self.s(v);
        let pad = s(20.0);
        let top = s(92.0);
        let foot = h - self.footer_h();
        let path_h = if self.path_edit.is_some() { s(40.0) } else { 0 };
        let another = R::new(pad, foot - s(40.0) - path_h, w - 2 * pad, s(32.0));
        let path = R::new(pad, foot - s(44.0), w - 2 * pad, s(30.0));
        let list = R::new(0, top, w, another.y - s(8.0) - top);
        // recommended apps, then the others under a label
        let (row_h, label_h) = (s(44.0), s(30.0));
        let mut y = list.y - self.scroll;
        let (mut rows, mut labels) = (Vec::new(), Vec::new());
        let order: Vec<usize> = (0..self.apps.len())
            .filter(|&i| self.apps[i].recommended)
            .chain((0..self.apps.len()).filter(|&i| !self.apps[i].recommended))
            .collect();
        let mut others_started = false;
        for (k, &i) in order.iter().enumerate() {
            if k == 0 && self.apps[i].recommended {
                labels.push(("Recommended", R::new(pad, y, w - 2 * pad, label_h)));
                y += label_h;
            }
            if !self.apps[i].recommended && !others_started {
                others_started = true;
                labels.push((if k == 0 { "Apps" } else { "Other apps" }, R::new(pad, y, w - 2 * pad, label_h)));
                y += label_h;
            }
            rows.push((i, R::new(s(10.0), y, w - s(20.0), row_h)));
            y += row_h;
        }
        let content_h = y + self.scroll - list.y;
        let bh = s(32.0);
        let by = foot + (self.footer_h() - bh) / 2;
        let cancel = R::new(w - pad - s(92.0), by, s(92.0), bh);
        let open = R::new(cancel.x - s(8.0) - s(92.0), by, s(92.0), bh);
        let settings = R::new(pad - s(6.0), by, s(190.0), bh);
        Layout { list, rows, labels, content_h, another, path, settings, open, cancel }
    }

    fn hit(&self, w: i32, h: i32, x: i32, y: i32) -> Option<Hit> {
        let l = self.layout(w, h);
        if l.open.contains(x, y) {
            return Some(Hit::Open);
        }
        if l.cancel.contains(x, y) {
            return Some(Hit::Cancel);
        }
        if l.settings.contains(x, y) {
            return Some(Hit::Settings);
        }
        if l.another.contains(x, y) && self.path_edit.is_none() {
            return Some(Hit::Another);
        }
        if l.list.contains(x, y) {
            return l.rows.iter().find(|(_, r)| r.contains(x, y)).map(|(i, _)| Hit::Row(*i));
        }
        None
    }

    /// the order the rows are shown in, for the arrow keys
    fn shown(&self) -> Vec<usize> {
        (0..self.apps.len())
            .filter(|&i| self.apps[i].recommended)
            .chain((0..self.apps.len()).filter(|&i| !self.apps[i].recommended))
            .collect()
    }
}

fn paint(hwnd: HWND) {
    OW.with(|o| {
        let Ok(mut o) = o.try_borrow_mut() else { return };
        let Some(o) = o.as_mut() else { return };
        let (w, h) = crate::ui::client_size(hwnd);
        if Surface::ensure(&mut o.surface, w, h).is_none() {
            return;
        }
        let l = o.layout(w, h);
        let t = o.theme;
        let s = |v: f32| (v * o.scale).round() as i32;
        let surf = o.surface.as_ref().unwrap();
        let pad = s(20.0);
        surf.fill(R::new(0, 0, w, h), t.bg);
        // the file: its picture, the question, its name
        surf.image_in(R::new(pad, s(18.0), s(48.0), s(48.0)), &o.file_icon);
        surf.text(
            R::new(pad + s(62.0), s(16.0), w - pad * 2 - s(62.0), s(30.0)),
            "Open with",
            o.title,
            t.fg,
            gfx::LEFT,
        );
        surf.text(R::new(pad + s(62.0), s(46.0), w - pad * 2 - s(62.0), s(22.0)), &o.file, o.ui, t.muted, gfx::LEFT);
        // the programs
        surf.set_clip(Some(l.list));
        for (label, r) in &l.labels {
            surf.text(*r, label, o.small, t.muted, gfx::LEFT);
        }
        for (i, r) in &l.rows {
            let app = &o.apps[*i];
            let selected = o.sel == Some(*i);
            if selected {
                surf.fill(*r, t.sel);
                surf.frame(*r, t.sel_border);
            } else if o.hot == Some(Hit::Row(*i)) {
                surf.fill(*r, t.hover);
            }
            surf.image_in(R::new(r.x + s(10.0), r.y, s(32.0), r.h), &app.icon);
            let tx = r.x + s(54.0);
            let tag_w = if app.default { gfx::text_width(surf.dc, "Default", o.small) + s(16.0) } else { 0 };
            surf.text(R::new(tx, r.y, r.right() - tx - tag_w - s(10.0), r.h), &app.name, o.ui, t.fg, gfx::LEFT);
            if app.default {
                surf.text(
                    R::new(r.right() - tag_w - s(10.0), r.y, tag_w, r.h),
                    "Default",
                    o.small,
                    t.muted,
                    gfx::RIGHT,
                );
            }
        }
        surf.set_clip(None);
        if l.content_h > l.list.h {
            // a hairline under the list says it scrolls
            surf.fill(R::new(0, l.list.bottom(), w, 1), t.line);
        }
        // another program
        if o.path_edit.is_none() {
            let hot = o.hot == Some(Hit::Another);
            surf.text(
                l.another,
                "Look for another app on this PC...",
                o.ui,
                if hot { t.fg } else { t.accent },
                gfx::LEFT,
            );
        } else {
            surf.text(
                R::new(pad, l.another.y, w - 2 * pad, l.another.h),
                "The program's path:",
                o.ui,
                t.muted,
                gfx::LEFT,
            );
            surf.round_frame(l.path, s(5.0) as f32, t.input_border, t.input_bg);
        }
        // the footer: Default apps, Open, Cancel
        let fy = h - o.footer_h();
        surf.fill(R::new(0, fy, w, o.footer_h()), t.chrome);
        surf.fill(R::new(0, fy, w, 1), t.card_border);
        let hot_settings = o.hot == Some(Hit::Settings);
        surf.text(
            l.settings.inset(s(6.0), 0),
            "Always use an app: Settings",
            o.ui,
            if hot_settings { t.fg } else { t.accent },
            gfx::LEFT,
        );
        let can_open = o.sel.is_some() || o.path_edit.is_some();
        for (hit, r, label, primary) in [(Hit::Open, l.open, "Open", true), (Hit::Cancel, l.cancel, "Cancel", false)] {
            let lit = o.hot == Some(hit) || o.pressed == Some(hit);
            if primary {
                let bg = if !can_open {
                    t.sel_inactive
                } else if lit {
                    t.primary_hot
                } else {
                    t.primary
                };
                surf.round(r, s(5.0) as f32, bg);
                surf.text(r, label, o.bold, if can_open { t.primary_fg } else { t.faint }, gfx::CENTER);
            } else {
                surf.round_frame(
                    r,
                    s(5.0) as f32,
                    if lit { t.input_hot } else { t.input_border },
                    if lit { t.btn_hover } else { t.bg },
                );
                surf.text(r, label, o.ui, t.fg, gfx::CENTER);
            }
        }
        surf.present(hwnd);
    });
}

fn with<R>(f: impl FnOnce(&mut Ow) -> R) -> Option<R> {
    OW.with(|o| o.try_borrow_mut().ok()?.as_mut().map(f))
}

fn hit_at(hwnd: HWND, x: i32, y: i32) -> Option<Hit> {
    let (w, h) = crate::ui::client_size(hwnd);
    with(|o| o.hit(w, h, x, y)).flatten()
}

/// what a click on `hit` does
fn act(hwnd: HWND, hit: Hit, double: bool) {
    match hit {
        Hit::Row(i) => {
            with(|o| {
                o.sel = Some(i);
                if double {
                    o.answer = Some(Answer::App(i));
                }
            });
        }
        Hit::Another => show_path_box(hwnd),
        Hit::Settings => {
            with(|o| o.answer = Some(Answer::Settings));
        }
        Hit::Open => confirm(),
        Hit::Cancel => {
            with(|o| o.answer = Some(Answer::Cancel));
        }
    }
    paint(hwnd);
}

/// Open: the typed path if there is one, else the selected program
fn confirm() {
    let path = with(|o| o.path_edit.map(crate::ui::window_text))
        .flatten()
        .map(|p| p.trim().trim_matches('"').to_string())
        .filter(|p| !p.is_empty());
    with(|o| {
        o.answer = match (path, o.sel) {
            (Some(p), _) => Some(Answer::Path(p)),
            (None, Some(i)) => Some(Answer::App(i)),
            (None, None) => None,
        }
    });
}

fn show_path_box(hwnd: HWND) {
    if with(|o| o.path_edit.is_some()).unwrap_or(true) {
        return;
    }
    with(|o| o.path_edit = Some(HWND::default()));
    let (w, h) = crate::ui::client_size(hwnd);
    let Some((r, font, margin)) = with(|o| (o.layout(w, h).path, o.ui, o.s(6.0))) else { return };
    unsafe {
        let style = WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32);
        let inner = r.inset(1, (r.h - with(|o| o.s(20.0)).unwrap_or(20)) / 2);
        if let Ok(e) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("EDIT"),
            PCWSTR::null(),
            style,
            inner.x + margin,
            inner.y,
            inner.w - 2 * margin,
            inner.h,
            Some(hwnd),
            None,
            None,
            None,
        ) {
            SendMessageW(e, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
            // a grey hint inside the empty box (EM_SETCUEBANNER)
            let hint = wstr(r"C:\Program Files\...\program.exe");
            SendMessageW(e, 0x1501, Some(WPARAM(1)), Some(LPARAM(hint.as_ptr() as isize)));
            with(|o| o.path_edit = Some(e));
            crate::ui::focus(e);
        }
    }
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
            WM_MOUSEMOVE => {
                let h = hit_at(hwnd, x_of(lp), y_of(lp));
                if with(|o| std::mem::replace(&mut o.hot, h) != h) == Some(true) {
                    paint(hwnd);
                }
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut tme);
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                if with(|o| o.hot.take().is_some()) == Some(true) {
                    paint(hwnd);
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let h = hit_at(hwnd, x_of(lp), y_of(lp));
                with(|o| o.pressed = h);
                if let Some(Hit::Row(i)) = h {
                    with(|o| o.sel = Some(i));
                }
                paint(hwnd);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let h = hit_at(hwnd, x_of(lp), y_of(lp));
                let p = with(|o| o.pressed.take()).flatten();
                if let Some(h) = h.filter(|h| Some(*h) == p) {
                    act(hwnd, h, false);
                }
                paint(hwnd);
                LRESULT(0)
            }
            WM_LBUTTONDBLCLK => {
                if let Some(h @ Hit::Row(_)) = hit_at(hwnd, x_of(lp), y_of(lp)) {
                    act(hwnd, h, true);
                }
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = (wp.0 >> 16) as u16 as i16 as i32;
                let (w, h) = crate::ui::client_size(hwnd);
                with(|o| {
                    let l = o.layout(w, h);
                    let max = (l.content_h - l.list.h).max(0);
                    o.scroll = (o.scroll - delta.signum() * o.s(44.0)).clamp(0, max);
                });
                paint(hwnd);
                LRESULT(0)
            }
            WM_CTLCOLOREDIT => {
                if let Some((fg, bg)) = with(|o| (o.theme.fg, o.theme.input_bg)) {
                    let dc = HDC(wp.0 as *mut _);
                    SetTextColor(dc, fg.colorref());
                    SetBkColor(dc, bg.colorref());
                    SetDCBrushColor(dc, bg.colorref());
                    return LRESULT(GetStockObject(DC_BRUSH).0 as isize);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_CLOSE => {
                with(|o| o.answer = Some(Answer::Cancel));
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn register() {
    if CLASS.get() {
        return;
    }
    unsafe {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(proc_),
            hInstance: GetModuleHandleW(None).unwrap_or_default().into(),
            hIcon: crate::ui::app_icon(),
            hIconSm: crate::ui::app_icon(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("dvfilesOpenWith"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
    }
    CLASS.set(true);
}

unsafe fn take(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    let s = p.to_string().unwrap_or_default();
    CoTaskMemFree(Some(p.0 as *const _));
    s
}

/// A Store app's icon: Windows names it as a resource of the app's package ("@{Package?ms-resource://...png}"), which
/// it resolves to the PNG for this screen's scale; dvfiles decodes that itself.
pub fn store_icon(resource: &str, size: i32) -> Option<Image> {
    let mut buf = vec![0u16; 1024];
    unsafe { SHLoadIndirectString(PCWSTR(wstr(resource).as_ptr()), &mut buf, None).ok()? };
    let path = crate::ui::from_wide(&buf);
    let data = std::fs::read(&path).ok().filter(|d| d.len() < 8 << 20)?;
    let d = dvfiles::img::fit(dvfiles::img::decode(&data, size as u32)?, size as u32, size as u32);
    Some(Image { w: d.w as i32, h: d.h as i32, px: d.px.into_boxed_slice() })
}

/// the programs Windows offers for a file type, recommended ones first, with the current default marked
fn programs(ext: &str, size: i32) -> Vec<(String, String, Option<Image>, bool, IAssocHandler)> {
    crate::ui::shell::ole_init();
    let dotted = wstr(&format!(".{ext}"));
    let mut out = Vec::new();
    unsafe {
        let Ok(list) = SHAssocEnumHandlers(PCWSTR(dotted.as_ptr()), ASSOC_FILTER_NONE) else { return out };
        loop {
            let mut one = [None];
            let mut got = 0u32;
            if list.Next(&mut one, Some(&mut got)).is_err() || got == 0 {
                break;
            }
            let Some(h) = one[0].take() else { break };
            let name = h.GetUIName().map(|p| take(p)).unwrap_or_default();
            let exe = h.GetName().map(|p| take(p)).unwrap_or_default();
            if name.trim().is_empty()
                || out.iter().any(|(n, _, _, _, _): &(String, String, Option<Image>, bool, IAssocHandler)| {
                    n.eq_ignore_ascii_case(&name)
                })
            {
                continue;
            }
            let mut ip = PWSTR::null();
            let mut index = 0;
            let icon = if h.GetIconLocation(&mut ip, &mut index).is_ok() {
                let file = take(ip);
                if file.starts_with("@{") {
                    store_icon(&file, size)
                } else {
                    crate::ui::icons::extract(&dvfiles::fsx::expand_env(&file), index, size)
                }
            } else {
                None
            };
            let recommended = h.IsRecommended() == S_OK;
            out.push((name, exe, icon, recommended, h));
        }
    }
    out
}

/// Shows Open with for `path` over `owner`, and opens the file with what is chosen.
pub fn show(owner: HWND, path: &str) {
    let ext = dvfiles::naming::ext_lower(path);
    let t0 = std::time::Instant::now();
    let Some((theme, ui, bold, title, small, scale, file_icon, fallback, size)) = crate::ui::with_app(|a| {
        use crate::ui::art::{kind_of, Art, Kind};
        let size = a.s(32.0);
        let file_icon = a.art.get(Art::File(kind_of(&ext)), a.s(48.0), &a.theme);
        let fallback = a.art.get(Art::File(Kind::App), size, &a.theme);
        (a.theme, a.fonts.ui, a.fonts.bold, a.fonts.title, a.fonts.small, a.scale, file_icon, fallback, size)
    }) else {
        return;
    };
    let default_exe = {
        let dotted = wstr(&format!(".{ext}"));
        let mut buf = [0u16; 1024];
        let mut n = buf.len() as u32;
        unsafe {
            AssocQueryStringW(
                ASSOCF_NOTRUNCATE,
                ASSOCSTR_EXECUTABLE,
                PCWSTR(dotted.as_ptr()),
                PCWSTR::null(),
                Some(PWSTR(buf.as_mut_ptr())),
                &mut n,
            )
        }
        .ok()
        .map(|_| crate::ui::from_wide(&buf).to_lowercase())
        .ok()
        .filter(|d| !d.ends_with("openwith.exe"))
    };
    let apps: Vec<Choice> = programs(&ext, size)
        .into_iter()
        .map(|(name, exe, icon, recommended, handler)| Choice {
            default: default_exe.as_deref().is_some_and(|d| d == exe.to_lowercase()),
            name,
            icon: icon.map(Rc::new).unwrap_or_else(|| fallback.clone()),
            recommended,
            handler,
        })
        .collect();
    crate::trace::log(|| {
        format!("open with: {} programs for .{ext} in {:.1} ms", apps.len(), t0.elapsed().as_secs_f64() * 1000.0)
    });
    let sel = apps
        .iter()
        .position(|a| a.default)
        .or_else(|| apps.iter().position(|a| a.recommended))
        .or(if apps.is_empty() { None } else { Some(0) });
    register();
    let s = |v: f32| (v * scale).round() as i32;
    let (cw, ch) = (s(460.0), s(560.0));
    let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
    let mut wr = RECT { left: 0, top: 0, right: cw, bottom: ch };
    unsafe {
        let _ = AdjustWindowRectEx(&mut wr, style, false, WS_EX_DLGMODALFRAME);
    }
    let (ww, wh) = (wr.right - wr.left, wr.bottom - wr.top);
    let mut or = RECT::default();
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe {
        let _ = GetWindowRect(owner, &mut or);
        let _ = GetMonitorInfoW(MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST), &mut mi);
    }
    let work = mi.rcWork;
    let x = ((or.left + or.right - ww) / 2).clamp(work.left, (work.right - ww).max(work.left));
    let y = ((or.top + or.bottom - wh) / 2).clamp(work.top, (work.bottom - wh).max(work.top));
    let Ok(hwnd) = (unsafe {
        CreateWindowExW(
            WS_EX_DLGMODALFRAME,
            w!("dvfilesOpenWith"),
            w!("Open with"),
            style,
            x,
            y,
            ww,
            wh,
            Some(owner),
            None,
            Some(GetModuleHandleW(None).unwrap_or_default().into()),
            None,
        )
    }) else {
        crate::trace::log(|| "open with: the window could not be made".to_string());
        return;
    };
    let file = dvfiles::pathx::file_name(path).to_string();
    OW.with(|c| {
        *c.borrow_mut() = Some(Ow {
            file,
            file_icon,
            apps,
            sel,
            hot: None,
            pressed: None,
            scroll: 0,
            path_edit: None,
            answer: None,
            theme,
            ui,
            bold,
            title,
            small,
            scale,
            surface: None,
        })
    });
    unsafe {
        let dark = theme.dark as i32;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const i32 as *const _, 4);
    }
    crate::ui::tint_frame(hwnd, theme.bg, theme.fg, theme.frame);
    unsafe {
        let _ = EnableWindow(owner, false);
        let _ = ShowWindow(hwnd, if crate::ui::quiet() { SW_SHOWNA } else { SW_SHOW });
    }
    let mut msg = MSG::default();
    let answer = loop {
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r.0 == 0 {
            unsafe { PostQuitMessage(msg.wParam.0 as i32) };
            break Answer::Cancel;
        }
        if r.0 == -1 {
            break Answer::Cancel;
        }
        let edit = with(|o| o.path_edit).flatten();
        let ours = msg.hwnd == hwnd
            || msg.hwnd == owner
            || unsafe { IsChild(owner, msg.hwnd).as_bool() }
            || Some(msg.hwnd) == edit;
        let handled = match msg.message {
            WM_KEYDOWN | WM_SYSKEYDOWN if ours => {
                let vk = VIRTUAL_KEY(msg.wParam.0 as u16);
                let in_edit = Some(msg.hwnd) == edit;
                match vk {
                    VK_ESCAPE => {
                        with(|o| o.answer = Some(Answer::Cancel));
                        true
                    }
                    VK_RETURN => {
                        confirm();
                        true
                    }
                    VK_UP | VK_DOWN | VK_HOME | VK_END if !in_edit => {
                        with(|o| {
                            let shown = o.shown();
                            if shown.is_empty() {
                                return;
                            }
                            let at = o.sel.and_then(|s| shown.iter().position(|&i| i == s));
                            let k = match (vk, at) {
                                (VK_HOME, _) | (VK_DOWN, None) => 0,
                                (VK_END, _) | (_, None) => shown.len() - 1,
                                (VK_DOWN, Some(k)) => (k + 1).min(shown.len() - 1),
                                (_, Some(k)) => k.saturating_sub(1),
                            };
                            o.sel = Some(shown[k]);
                        });
                        paint(hwnd);
                        true
                    }
                    _ if in_edit => false,
                    _ => true,
                }
            }
            WM_CHAR | WM_SYSCHAR | WM_KEYUP | WM_SYSKEYUP if ours && Some(msg.hwnd) != edit => true,
            _ => false,
        };
        if !handled {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if let Some(a) = with(|o| o.answer.take()).flatten() {
            break a;
        }
    };
    crate::ui::menu::swallow_chars();
    let chosen = with(|o| match &answer {
        Answer::App(i) => o.apps.get(*i).map(|c| (c.handler.clone(), c.name.clone())),
        _ => None,
    })
    .flatten();
    unsafe {
        let _ = EnableWindow(owner, true);
        let _ = DestroyWindow(hwnd);
    }
    OW.with(|c| *c.borrow_mut() = None);
    match answer {
        Answer::App(_) => {
            if let Some((handler, name)) = chosen {
                let ok = crate::ui::shell::data_object(path).is_some_and(|d| unsafe { handler.Invoke(&d) }.is_ok());
                if !ok {
                    crate::ui::shell::message(
                        owner,
                        &format!("{name} could not open \"{}\".", dvfiles::pathx::file_name(path)),
                        true,
                    );
                }
            }
        }
        Answer::Path(exe) => crate::ui::shell::open_in(owner, &exe, path),
        Answer::Settings => crate::ui::shell::default_apps(owner),
        Answer::Cancel => {}
    }
}
