// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! The window: plain Win32, one main window with the toolbar and status bar painted into it, and two child windows
//! (navigation pane and file list) that paint themselves. Text boxes are the system EDIT control.
pub mod art;
pub mod bar;
pub mod bubble;
pub mod clip;
pub mod cmds;
pub mod details;
pub mod dialog;
pub mod dnd;
pub mod drag;
pub mod gfx;
pub mod glass;
pub mod icons;
mod keys;
pub mod list;
pub mod locale;
mod look;
pub mod menu;
pub mod model;
pub mod nav;
pub mod openwith;
pub mod opsui;
pub mod prefs;
pub mod preview;
pub mod props;
pub mod scroll;
mod search;
pub mod shell;
pub mod tabs;
pub mod thumbs;
pub mod viewer;
pub mod watch;
use keys::*;
pub use look::*;
pub use search::*;

use crate::settings::{Settings, ThemeMode, ViewMode};
use gfx::{Fonts, Rgb, Surface, Theme, R};
use model::{Loc, Model};
use std::cell::RefCell;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Controls::{SetWindowTheme, WM_MOUSELEAVE};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const T_SEARCH: usize = 1;
pub const T_DIR: usize = 2;
pub const T_IDLE: usize = 3;
pub const T_LOADING: usize = 4;
pub const WM_JOB_DONE: u32 = WM_APP + 15;
// the address bar's progress fill, ticking only while something loads
pub const T_BUSY: usize = 5;
// posted after the first frame: things that need not hold up the window's appearance
const WM_AFTER_SHOW: u32 = WM_APP + 22;

pub fn wstr(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub fn from_wide(p: &[u16]) -> String {
    let end = p.iter().position(|&c| c == 0).unwrap_or(p.len());
    String::from_utf16_lossy(&p[..end])
}

/// hands a value to the window thread; the receiver takes it back with `take_box`
pub fn post_box<T>(hwnd: HWND, msg: u32, v: T) {
    let p = Box::into_raw(Box::new(v));
    unsafe {
        if PostMessageW(Some(hwnd), msg, WPARAM(0), LPARAM(p as isize)).is_err() {
            drop(Box::from_raw(p));
        }
    }
}

/// # Safety
/// `lp` must come from `post_box::<T>` with the same `T`
pub unsafe fn take_box<T>(lp: LPARAM) -> Box<T> {
    Box::from_raw(lp.0 as *mut T)
}

pub fn x_of(lp: LPARAM) -> i32 {
    (lp.0 & 0xFFFF) as u16 as i16 as i32
}

pub fn y_of(lp: LPARAM) -> i32 {
    ((lp.0 >> 16) & 0xFFFF) as u16 as i16 as i32
}

pub fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetKeyState(vk.0 as i32) < 0 }
}

pub fn client_size(hwnd: HWND) -> (i32, i32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut r);
    }
    (r.right - r.left, r.bottom - r.top)
}

/// lets a window take files dropped with the old WM_DROPFILES message: the style bit DragAcceptFiles sets, without
/// loading the shell for it
pub fn accept_files(hwnd: HWND, on: bool) {
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let bit = WS_EX_ACCEPTFILES.0 as isize;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, if on { ex | bit } else { ex & !bit });
    }
}

pub fn invalidate(hwnd: HWND) {
    // a window not made yet (the preview pane, the text boxes): no window at all would mean every window on screen
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

pub struct App {
    pub main: HWND,
    pub list: HWND,
    pub nav: HWND,
    pub preview: HWND,
    pub addr: HWND,
    pub search: HWND,
    pub scale: f32,
    pub fonts: Fonts,
    pub theme: Theme,
    pub input_brush: HBRUSH,
    pub settings: Settings,
    pub icons: icons::Icons,
    pub mdc: HDC,
    pub nf: dvfiles::fmt::NumFmt,
    pub model: Model,
    pub generation: usize,
    pub back: Vec<Loc>,
    pub fwd: Vec<Loc>,
    pub drives: Vec<dvfiles::fsx::Drive>,
    pub known: Vec<(&'static str, String)>,
    pub navs: nav::Nav,
    pub lv: list::ListState,
    pub bar: bar::BarState,
    pub surf_main: Option<Surface>,
    // the status bar's band of the main window (see bar::paint)
    pub surf_status: Option<Surface>,
    // the frame's margin beside the card
    pub surf_edge: Option<Surface>,
    pub surf_list: Option<Surface>,
    pub surf_nav: Option<Surface>,
    pub surf_preview: Option<Surface>,
    pub pv: preview::State,
    // the undo bubble at the bottom of the list, while it shows
    pub bubble: Option<bubble::Bubble>,
    // what is behind the window, blurred, for the glass; whether Windows left the window out of captures so the
    // glass can show it; and the title bar's colour over it
    pub backdrop: Option<glass::Backdrop>,
    pub glass_live: bool,
    // how often the glass looks behind the window now, in ms
    pub glass_pace: u32,
    pub caption: Option<Rgb>,
    watcher: Option<watch::Watcher>,
    // what was cut to the clipboard: its sequence number and the paths (lower case), drawn faded
    pub cut: Option<(u32, Vec<String>)>,
    pub addr_editing: bool,
    // the client size the last layout was made for
    laid_out: (i32, i32),
    // a folder is loading or a search runs: when it started, and when it ended (the fill then completes and fades)
    pub busy: Option<(std::time::Instant, Option<std::time::Instant>)>,
    // folder sizes measured this session, by lower-case path, and the running sizer's stop flag
    pub size_cache: std::collections::HashMap<String, u64>,
    sizer: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    // what Ctrl+Z puts back, newest last
    pub undo: Vec<dvfiles::undo::Action>,
    // the folders gone to, for jumping to one by a few letters of its name
    pub visits: dvfiles::visits::Visits,
    // the shelf: items gathered from anywhere, to copy or move somewhere together
    pub shelf: Vec<String>,
    // in Downloads: what arrived after this (FILETIME) is new since the last look
    pub new_since: Option<u64>,
    pub thumbs: thumbs::Thumbs,
    pub tabs: tabs::Tabs,
    pub art: art::Cache,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` with the app. Returns None if the app is already in use further up the stack (a message sent while
/// handling another one), so such a message is skipped instead of panicking.
/// DVFILES_NOACTIVATE=1, for tests run while someone works at the machine: dvfiles' windows never take the
/// keyboard from the window in front (posted keys and clicks still reach them)
pub fn quiet() -> bool {
    static Q: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *Q.get_or_init(|| std::env::var_os("DVFILES_NOACTIVATE").is_some())
}

/// gives `h` the keyboard; in the quiet mode, only while its window is in front, as SetFocus would bring it there
pub fn focus(h: HWND) {
    unsafe {
        if quiet() && GetForegroundWindow() != GetAncestor(h, GA_ROOT) {
            return;
        }
        let _ = SetFocus(Some(h));
    }
}

pub fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|c| {
        let mut b = c.try_borrow_mut().ok()?;
        b.as_mut().map(f)
    })
}

impl App {
    pub fn s(&self, v: f32) -> i32 {
        (v * self.scale).round() as i32
    }

    /// the corner radius of buttons, boxes and rows
    pub fn rad(&self) -> f32 {
        (5.0 * self.scale).round()
    }

    /// the corner radius of the card, the bubble and other panels
    pub fn rad_card(&self) -> f32 {
        (8.0 * self.scale).round()
    }

    pub fn view(&self) -> ViewMode {
        if self.model.loc == Loc::Pc {
            ViewMode::Details
        } else {
            self.settings.view
        }
    }

    pub fn sort_key(&self) -> (crate::settings::Col, bool) {
        (self.settings.sort, self.settings.sort_desc)
    }

    pub fn is_cut(&self, path: &str) -> bool {
        match &self.cut {
            Some((seq, paths)) if *seq == clip::sequence() => {
                let p = path.to_lowercase();
                paths.iter().any(|c| *c == p)
            }
            _ => false,
        }
    }

    pub fn drive(&self, root: &str) -> Option<&dvfiles::fsx::Drive> {
        self.drives.iter().find(|d| d.root.eq_ignore_ascii_case(root))
    }

    /// `Local Disk (C:)` for a drive root, the folder name otherwise
    pub fn display_name(&self, path: &str) -> String {
        if path.len() <= 3 && path.as_bytes().get(1) == Some(&b':') {
            let root = format!("{}\\", path.trim_end_matches('\\'));
            if let Some(d) = self.drive(&root) {
                return d.title();
            }
            return format!("Local Disk ({})", path.trim_end_matches('\\'));
        }
        for (label, p) in &self.known {
            if dvfiles::pathx::same(p, path) {
                return label.to_string();
            }
        }
        dvfiles::pathx::file_name(path).to_string()
    }

    pub fn title_text(&self) -> String {
        match (&self.model.loc, &self.model.search) {
            (_, Some(s)) => format!("\"{}\" - Search results", s.text),
            (Loc::Pc, _) => "This PC".into(),
            (Loc::Dir(d), _) => self.display_name(d),
        }
    }

    fn types(&mut self) -> impl FnMut(&dvfiles::fsx::Entry) -> std::rc::Rc<str> + '_ {
        |e| self.icons.type_name(e)
    }

    pub fn resort(&mut self) {
        let key = self.sort_key();
        let mut model = std::mem::replace(&mut self.model, Model::new(Loc::Pc, false));
        model.sort(key, &mut self.types());
        self.model = model;
        self.after_change();
    }

    /// after the list content changed: scroll range, status bar
    pub fn after_change(&mut self) {
        list::clamp_scroll(self);
        invalidate(self.list);
        invalidate(self.main);
    }

    pub fn ensure_visible_focus(&mut self) {
        if let Some(p) = self.model.sel.focus_pos(&self.model.order) {
            list::scroll_to(self, p);
        }
    }

    fn start_watch(&mut self) {
        self.watcher = None;
        if let Loc::Dir(d) = &self.model.loc {
            self.watcher = watch::Watcher::start(self.main, d, self.generation);
        }
    }

    /// Measures the shown folder's subfolders in the background when "Folder sizes" is on. Sizes already known this
    /// session are used at once; only the rest are measured.
    pub fn start_sizer(&mut self) {
        self.stop_sizer();
        self.model.sizes.clear();
        self.model.sizes_pending = 0;
        let Loc::Dir(folder) = self.model.loc.clone() else { return };
        if !self.settings.folder_sizes || self.model.search.is_some() {
            return;
        }
        let mut todo = Vec::new();
        for &i in &self.model.order {
            let e = &self.model.entries[i as usize];
            if !e.is_dir() || e.attrs & dvfiles::fsx::FA_REPARSE != 0 {
                continue;
            }
            let path = dvfiles::pathx::join(&folder, &e.name);
            match self.size_cache.get(&path.to_lowercase()) {
                Some(&b) => {
                    self.model.sizes.insert(e.name.to_lowercase(), b);
                }
                None => todo.push((e.name.to_string(), path)),
            }
        }
        self.model.sizes_pending = todo.len();
        if !todo.is_empty() {
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            model::spawn_sizer(self.main, self.generation, todo, stop.clone());
            self.sizer = Some(stop);
        }
    }

    pub fn push_undo(&mut self, a: dvfiles::undo::Action) {
        if let Some(text) = a.summary() {
            bubble::show(self, text);
        }
        self.undo.push(a);
        if self.undo.len() > 50 {
            self.undo.remove(0);
        }
    }

    pub fn stop_sizer(&mut self) {
        if let Some(s) = self.sizer.take() {
            s.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    pub fn stop_search(&mut self) {
        if let Some(s) = &mut self.model.search {
            s.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// navigation

/// Shows a location. The listing happens on a thread; the old content stays until it arrives.
pub fn navigate(loc: Loc, push: bool) {
    let loc = match loc {
        Loc::Dir(d) => {
            let full = dvfiles::fsx::full_path(&d);
            Loc::Dir(full)
        }
        other => other,
    };
    let clear_search = with_app(|a| {
        a.stop_search();
        a.stop_sizer();
        let had_search = a.model.search.is_some();
        if push && !a.model.loc.same(&loc) {
            let old = a.model.loc.clone();
            a.back.push(old);
            a.fwd.clear();
        }
        a.generation += 1;
        a.new_since = None;
        if let Loc::Dir(d) = &loc {
            let downloads = a.known.iter().any(|(k, p)| *k == "Downloads" && dvfiles::pathx::same(p, d));
            if downloads && a.settings.inbox {
                a.new_since = a.visits.last(d).map(|t| (t + 11_644_473_600) * 10_000_000);
            }
            a.visits.record(d, dvfiles::visits::now());
        }
        a.model.search = None;
        a.model.loc = loc.clone();
        a.model.loading = true;
        a.model.error = None;
        a.lv.scroll_y = 0;
        a.lv.glide = None;
        a.lv.hover = None;
        a.addr_editing = false;
        model::spawn_list(a.main, a.generation, loc.clone(), false);
        unsafe {
            SetTimer(Some(a.main), T_LOADING, 300, None);
        }
        a.bar.dirty = true;
        invalidate(a.main);
        a.navs.mark_current(&loc);
        invalidate(a.nav);
        set_busy(a, true);
        had_search
    });
    if clear_search == Some(true) {
        set_search_text("");
    }
    let addr = with_app(|a| a.addr);
    if let Some(h) = addr {
        unsafe {
            let _ = ShowWindow(h, SW_HIDE);
        }
    }
    update_title();
}

/// The address and search boxes are system EDIT controls made the first time one is needed: the first EDIT loads
/// the common controls and theme libraries, which would add about 14 ms to every start.
pub fn ensure_edits() -> (HWND, HWND) {
    let Some((main, addr, search, font, dark)) = with_app(|a| (a.main, a.addr, a.search, a.fonts.ui, a.theme.dark))
    else {
        return Default::default();
    };
    if !search.0.is_null() {
        return (addr, search);
    }
    let style = WINDOW_STYLE(ES_AUTOHSCROLL as u32) | WS_TABSTOP;
    let (addr, search) = (child(w!("EDIT"), style, main), child(w!("EDIT"), style, main));
    unsafe {
        let _ = ShowWindow(addr, SW_HIDE);
        let name = if dark { w!("DarkMode_Explorer") } else { w!("Explorer") };
        for h in [addr, search] {
            let _ = SetWindowTheme(h, name, PCWSTR::null());
            set_font(h, font);
        }
        // EM_SETCUEBANNER
        let cue = wstr("Search");
        SendMessageW(search, 0x1501, Some(WPARAM(1)), Some(LPARAM(cue.as_ptr() as isize)));
    }
    with_app(|a| {
        a.addr = addr;
        a.search = search;
    });
    layout();
    (addr, search)
}

/// Starts or ends the address bar's progress fill, Windows 10's sign that a folder is loading or a search runs.
pub fn set_busy(a: &mut App, on: bool) {
    let now = std::time::Instant::now();
    match (on, a.busy) {
        (true, None) | (true, Some((_, Some(_)))) => {
            a.busy = Some((now, None));
            unsafe {
                SetTimer(Some(a.main), T_BUSY, 33, None);
            }
        }
        (false, Some((start, None))) => a.busy = Some((start, Some(now))),
        _ => {}
    }
    invalidate(a.main);
}

/// how far the fill has run (0..1) and how strong it is (fading after the end); None when nothing shows
pub fn busy_fill(a: &App) -> Option<(f32, f32)> {
    let (start, end) = a.busy?;
    let t = start.elapsed().as_secs_f32();
    match end {
        // quick loads end before the fill would appear
        None if t < 0.15 => None,
        // it does not know how long, so it slows as it goes, like Windows' own
        None => Some((0.95 * (1.0 - (-(t - 0.15) / 2.5).exp()), 1.0)),
        Some(e) if e.duration_since(start).as_secs_f32() < 0.15 => None,
        Some(e) => {
            // from where it was, ease out to full in a quarter second, then fade
            let ran = e.duration_since(start).as_secs_f32();
            let reached = 0.95 * (1.0 - (-(ran - 0.15) / 2.5).exp());
            let since = e.elapsed().as_secs_f32();
            let k = (since / 0.25).min(1.0);
            let ease = 1.0 - (1.0 - k) * (1.0 - k) * (1.0 - k);
            Some((reached + (1.0 - reached) * ease, (1.0 - (since - 0.25).max(0.0) / 0.3).clamp(0.0, 1.0)))
        }
    }
}

fn busy_tick() {
    with_app(|a| {
        let finished = match a.busy {
            Some((_, Some(e))) => e.elapsed().as_secs_f32() > 0.55,
            None => true,
            _ => false,
        };
        if finished {
            a.busy = None;
            unsafe {
                let _ = KillTimer(Some(a.main), T_BUSY);
            }
        }
        invalidate(a.main);
    });
}

pub fn update_title() {
    if let Some((h, t)) = with_app(|a| {
        tabs::retitle(a);
        (a.main, a.title_text())
    }) {
        unsafe {
            let _ = SetWindowTextW(h, PCWSTR(wstr(&t).as_ptr()));
        }
    }
}

pub fn set_search_text(t: &str) {
    if let Some((h, m)) = with_app(|a| (a.search, a.main)) {
        unsafe {
            // setting the text reports a change, which would start a search
            let _ = SetWindowTextW(h, PCWSTR(wstr(t).as_ptr()));
            let _ = KillTimer(Some(m), T_SEARCH);
        }
    }
}

fn on_listed(l: Box<model::Listed>) {
    let rename = with_app(|a| {
        if l.generation != a.generation || !l.loc.same(&a.model.loc) {
            return None;
        }
        unsafe {
            let _ = KillTimer(Some(a.main), T_LOADING);
        }
        if !l.refresh {
            set_busy(a, false);
        }
        a.model.loading = false;
        a.model.list_ms = l.ms;
        let first_load = !l.refresh;
        match l.result {
            Ok(entries) => {
                a.model.error = None;
                let key = a.sort_key();
                let mut model = std::mem::replace(&mut a.model, Model::new(Loc::Pc, false));
                model.set_entries(entries, l.refresh, key, &mut |e| a.icons.type_name(e));
                a.model = model;
            }
            Err(e) => {
                if l.refresh {
                    // the folder went away while shown: go up to what still exists
                    let mut up = a.model.loc.path().and_then(dvfiles::pathx::parent);
                    while let Some(p) = up.clone() {
                        if dvfiles::fsx::is_dir(&p) {
                            break;
                        }
                        up = dvfiles::pathx::parent(&p);
                    }
                    let target = up.map(Loc::Dir).unwrap_or(Loc::Pc);
                    return Some((false, Some(target)));
                }
                a.model.entries.clear();
                a.model.order.clear();
                a.model.sel = dvfiles::selection::Selection::new(0);
                a.model.error = Some(e);
            }
        }
        if first_load {
            a.start_watch();
            if a.model.sel.focus.is_none() {
                if let Some(&first) = a.model.order.first() {
                    a.model.sel.focus = Some(first);
                }
            }
        }
        if a.model.error.is_none() {
            a.start_sizer();
        }
        a.ensure_visible_focus();
        // a tab brought back keeps where it was scrolled to
        if let Some(y) = a.lv.restore_scroll.take() {
            a.lv.scroll_y = y;
        }
        a.after_change();
        a.bar.dirty = true;
        let rename = a.model.pending_rename;
        a.model.pending_rename = false;
        Some((rename, None))
    })
    .flatten();
    match rename {
        Some((true, _)) => list::begin_rename(),
        Some((false, Some(target))) => navigate(target, false),
        _ => {}
    }
    update_title();
}

/// reloads the shown folder in place, keeping selection and scroll
pub fn refresh() {
    with_app(|a| {
        if a.model.search.is_some() {
            return;
        }
        if a.lv.rename_edit.is_some() {
            a.lv.refresh_after_rename = true;
            return;
        }
        a.generation += 1;
        a.start_watch();
        model::spawn_list(a.main, a.generation, a.model.loc.clone(), true);
        if a.model.loc == Loc::Pc {
            model::spawn_drives(a.main);
        }
    });
}

fn set_font(h: HWND, f: HFONT) {
    unsafe {
        SendMessageW(h, WM_SETFONT, Some(WPARAM(f.0 as usize)), Some(LPARAM(1)));
    }
}

pub fn layout() {
    let plan = with_app(|a| {
        let (w, h) = client_size(a.main);
        a.laid_out = (w, h);
        a.bar.dirty = true;
        let l = bar::layout(a, w, h);
        // the sidebar on the frame, and the card (the list, and the preview at its side) beside it
        let card = l.card;
        let nav = R::new(0, card.y, if a.settings.nav_width < 40 { 0 } else { card.x }, card.h);
        let pv_w = preview::width(a, card.w);
        let list = R::new(card.x, card.y, (card.w - pv_w).max(0), card.h);
        let pv = R::new(list.right(), card.y, pv_w, card.h);
        let text_h = a.s(18.0);
        let ab = l.addr_text;
        let addr = R::new(ab.x, ab.y + (ab.h - text_h) / 2, ab.w, text_h);
        let sb = l.search_text;
        let search = R::new(sb.x, sb.y + (sb.h - text_h) / 2, sb.w, text_h);
        a.bar.layout = Some(l);
        (a.nav, nav, a.list, list, a.addr, addr, a.addr_editing, a.search, search, a.preview, pv)
    });
    let Some((navh, nav, listh, list, addrh, addr, editing, searchh, search, mut pvh, pv)) = plan else { return };
    if pv.w > 0 && pvh.0.is_null() {
        if let Some(main) = with_app(|a| a.main) {
            pvh = child(w!("dvfilesPreview"), WINDOW_STYLE(0), main);
            with_app(|a| a.preview = pvh);
        }
    }
    unsafe {
        let _ = MoveWindow(pvh, pv.x, pv.y, pv.w, pv.h, true);
        let _ = ShowWindow(pvh, if pv.w > 0 { SW_SHOWNA } else { SW_HIDE });
        let _ = MoveWindow(navh, nav.x, nav.y, nav.w, nav.h, true);
        let _ = ShowWindow(navh, if nav.w > 0 { SW_SHOWNA } else { SW_HIDE });
        let _ = MoveWindow(listh, list.x, list.y, list.w, list.h, true);
        let _ = MoveWindow(addrh, addr.x, addr.y, addr.w, addr.h, true);
        let _ = ShowWindow(addrh, if editing { SW_SHOW } else { SW_HIDE });
        let _ = MoveWindow(searchh, search.x, search.y, search.w, search.h, true);
    }
    with_app(|a| {
        list::clamp_scroll(a);
        invalidate(a.main);
    });
}

fn on_dpi(new_dpi: u32, suggested: &RECT) {
    let hw = with_app(|a| {
        a.scale = new_dpi as f32 / 96.0;
        a.fonts = Fonts::new(a.scale);
        preview::reset_fonts(a);
        a.icons.small_px = a.s(16.0);
        a.icons.large_px = a.s(48.0);
        (a.main, a.addr, a.search, a.fonts.ui)
    });
    if let Some((main, addr, search, f)) = hw {
        set_font(addr, f);
        set_font(search, f);
        unsafe {
            let _ = SetWindowPos(
                main,
                None,
                suggested.left,
                suggested.top,
                suggested.right - suggested.left,
                suggested.bottom - suggested.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        layout();
    }
}

// ---------------------------------------------------------------------------------------------------------------
// idle: give memory back when nothing happens

pub fn poke_idle() {
    if let Some(m) = with_app(|a| a.main) {
        unsafe {
            SetTimer(Some(m), T_IDLE, 8000, None);
        }
    }
}

fn go_idle() {
    with_app(|a| {
        unsafe {
            let _ = KillTimer(Some(a.main), T_IDLE);
        }
        // the screen keeps what was painted; back buffers are rebuilt by the next paint, pictures asked for again
        a.thumbs.clear();
        a.art.clear();
        a.icons.clear_own();
        a.surf_main = None;
        a.surf_status = None;
        a.surf_edge = None;
        a.surf_list = None;
        a.surf_nav = None;
        a.surf_preview = None;
    });
    dvfiles::trim_memory();
}

// ---------------------------------------------------------------------------------------------------------------
// main window

extern "system" fn main_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if let Some(r) = shell::menu_message(msg, wp, lp) {
        return r;
    }
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
                with_app(bar::paint);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_SIZE => {
                // showing the window sends a size it already has; laying out again would only repaint
                let (w, h) = ((lp.0 & 0xFFFF) as i32, ((lp.0 >> 16) & 0xFFFF) as i32);
                if with_app(|a| a.laid_out != (w, h)).unwrap_or(true) {
                    with_app(glass_look);
                    layout();
                }
                LRESULT(0)
            }
            WM_GETMINMAXINFO => {
                let mmi = &mut *(lp.0 as *mut MINMAXINFO);
                let s = GetDpiForWindow(hwnd).max(96) as i32;
                mmi.ptMinTrackSize = POINT { x: 420 * s / 96, y: 300 * s / 96 };
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                bar::mouse_move(x_of(lp), y_of(lp));
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                bar::mouse_leave();
                LRESULT(0)
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                bar::mouse_down(x_of(lp), y_of(lp));
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                bar::mouse_up(x_of(lp), y_of(lp));
                LRESULT(0)
            }
            WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
                let mut p = POINT::default();
                let _ = GetCursorPos(&mut p);
                let _ = ScreenToClient(hwnd, &mut p);
                let split = with_app(|a| bar::over_splitter(a, p.x, p.y)).unwrap_or(false);
                SetCursor(Some(LoadCursorW(None, if split { IDC_SIZEWE } else { IDC_ARROW }).unwrap_or_default()));
                LRESULT(1)
            }
            WM_MBUTTONUP => {
                bar::middle_click(x_of(lp), y_of(lp));
                LRESULT(0)
            }
            WM_XBUTTONUP => {
                let button = (wp.0 >> 16) & 0xFFFF;
                cmds::run(if button == 1 { cmds::Cmd::Back } else { cmds::Cmd::Forward });
                LRESULT(1)
            }
            WM_COMMAND => {
                let code = ((wp.0 >> 16) & 0xFFFF) as u32;
                let from = HWND(lp.0 as *mut _);
                let (addr, search) = with_app(|a| (a.addr, a.search)).unwrap_or_default();
                if from == search && code == EN_CHANGE {
                    let _ = SetTimer(Some(hwnd), T_SEARCH, 250, None);
                    invalidate(hwnd);
                } else if from == addr && code == EN_KILLFOCUS {
                    bar::end_address_edit();
                } else if from == search && (code == EN_SETFOCUS || code == EN_KILLFOCUS) {
                    invalidate(hwnd);
                }
                LRESULT(0)
            }
            WM_CTLCOLOREDIT => {
                let colors = with_app(|a| (a.theme.fg, a.theme.input_bg, a.input_brush));
                if let Some((fg, bg, brush)) = colors {
                    let dc = HDC(wp.0 as *mut _);
                    SetTextColor(dc, fg.colorref());
                    SetBkColor(dc, bg.colorref());
                    return LRESULT(brush.0 as isize);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_TIMER => {
                match wp.0 {
                    T_SEARCH => {
                        let _ = KillTimer(Some(hwnd), T_SEARCH);
                        if let Some(h) = with_app(|a| a.search) {
                            // one letter matches nearly everything: that search waits for Enter
                            let text = window_text(h);
                            if text.trim().chars().count() != 1 {
                                start_search(text);
                            }
                        }
                    }
                    T_DIR => {
                        let _ = KillTimer(Some(hwnd), T_DIR);
                        refresh();
                    }
                    T_IDLE => go_idle(),
                    glass::T_GLASS => {
                        with_app(glass_look);
                    }
                    T_BUSY => busy_tick(),
                    T_LOADING => {
                        let _ = KillTimer(Some(hwnd), T_LOADING);
                        with_app(|a| invalidate(a.list));
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            thumbs::WM_THUMB => {
                let r = take_box::<thumbs::Reply>(lp);
                with_app(|a| {
                    a.thumbs.apply(*r);
                    invalidate(a.list);
                    invalidate(a.preview);
                });
                LRESULT(0)
            }
            preview::WM_PREVIEW_TEXT => {
                preview::on_text(take_box(lp));
                LRESULT(0)
            }
            icons::WM_ICONS => {
                let r = take_box::<icons::Reply>(lp);
                with_app(|a| {
                    a.icons.apply(*r);
                    invalidate(a.list);
                    invalidate(a.nav);
                    invalidate(a.main);
                    invalidate(a.preview);
                });
                props::retheme();
                LRESULT(0)
            }
            watch::WM_DIRCHANGED => {
                let current = with_app(|a| a.generation == wp.0 && a.model.search.is_none()).unwrap_or(false);
                if current {
                    // a burst of changes costs one reload
                    let _ = SetTimer(Some(hwnd), T_DIR, 150, None);
                }
                LRESULT(0)
            }
            model::WM_LISTED => {
                on_listed(take_box(lp));
                LRESULT(0)
            }
            model::WM_SEARCH => {
                on_search(take_box(lp));
                LRESULT(0)
            }
            model::WM_SIZES => {
                let r = take_box::<model::SizeResult>(lp);
                with_app(|a| {
                    if r.generation != a.generation {
                        return;
                    }
                    a.size_cache.insert(r.path.to_lowercase(), r.bytes);
                    a.model.sizes.insert(r.name.to_lowercase(), r.bytes);
                    a.model.sizes_pending = a.model.sizes_pending.saturating_sub(1);
                    if r.last {
                        a.sizer = None;
                        // sorting by size waits for the last folder, so rows do not jump while sizes arrive
                        if a.settings.sort == crate::settings::Col::Size {
                            a.resort();
                        }
                    }
                    invalidate(a.list);
                    invalidate(a.main);
                });
                LRESULT(0)
            }
            model::WM_DRIVES => {
                let d = take_box::<Vec<dvfiles::fsx::Drive>>(lp);
                with_app(|a| {
                    a.drives = *d;
                    a.navs.set_drives(&a.drives);
                    a.bar.dirty = true;
                    invalidate(a.nav);
                    invalidate(a.list);
                    invalidate(a.main);
                });
                update_title();
                LRESULT(0)
            }
            glass::WM_GLASS => {
                let b = take_box::<Option<glass::Backdrop>>(lp);
                with_app(|a| {
                    if glass::took(a, *b) {
                        glass_moved(a);
                    }
                });
                LRESULT(0)
            }
            WM_JOB_DONE => {
                cmds::job_done(take_box(lp));
                LRESULT(0)
            }
            dnd::WM_DROPPED => {
                let d = take_box::<dnd::Dropped>(lp);
                cmds::start_job(d.kind, d.paths, Some(d.target), None);
                LRESULT(0)
            }
            WM_AFTER_SHOW => {
                if let Some((l, n)) = with_app(|a| (a.list, a.nav)) {
                    dnd::register(l, n);
                    dnd::self_test(l);
                }
                glass_setting_changed();
                // for tests and screenshots: Settings open at the start
                if std::env::var_os("DVFILES_OPEN_SETTINGS").is_some() {
                    prefs::open();
                }
                LRESULT(0)
            }
            // the window moved, or came forward or went back: the glass shows what is behind it now
            WM_MOVE => {
                with_app(glass_look);
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            // F12 with DVFILES_DNDTEST set: let go of the test drag
            WM_KEYDOWN if wp.0 == 0x7B && std::env::var_os("DVFILES_DNDTEST").is_some() => {
                dnd::self_test_drop();
                LRESULT(0)
            }
            WM_DEVICECHANGE => {
                if let Some(m) = with_app(|a| a.main) {
                    model::spawn_drives(m);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_SETTINGCHANGE => {
                let what =
                    if lp.0 != 0 { PCWSTR(lp.0 as *const u16).to_string().unwrap_or_default() } else { String::new() };
                if what == "ImmersiveColorSet" {
                    if let Some(ThemeMode::System) = with_app(|a| a.settings.theme) {
                        set_theme(ThemeMode::System);
                    }
                }
                LRESULT(0)
            }
            // WM_DWMCOLORIZATIONCOLORCHANGED: the accent colour changed, which the light theme follows
            0x0320 => {
                if let Some(mode) = with_app(|a| (!a.theme.dark).then_some(a.settings.theme)).flatten() {
                    set_theme(mode);
                }
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let r = &*(lp.0 as *const RECT);
                on_dpi((wp.0 & 0xFFFF) as u32, r);
                LRESULT(0)
            }
            WM_ACTIVATE => {
                with_app(|a| {
                    invalidate(a.list);
                    // what is behind may have changed while the window was away
                    glass_look(a);
                });
                if (wp.0 & 0xFFFF) != 0 {
                    // focus the list when the window is activated, unless a text box had it
                    let focus = GetFocus();
                    let (l, ad, s) = with_app(|a| (a.list, a.addr, a.search)).unwrap_or_default();
                    if focus != ad && focus != s {
                        let _ = SetFocus(Some(l));
                    }
                    return LRESULT(0);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_CLOSE => {
                save_window_state();
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                dnd::revoke();
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

pub fn window_text(h: HWND) -> String {
    unsafe {
        let n = GetWindowTextLengthW(h);
        let mut buf = vec![0u16; n as usize + 1];
        GetWindowTextW(h, &mut buf);
        from_wide(&buf)
    }
}

fn save_window_state() {
    with_app(|a| {
        let mut wp = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        unsafe {
            if GetWindowPlacement(a.main, &mut wp).is_ok() {
                let r = wp.rcNormalPosition;
                a.settings.window = [r.left, r.top, r.right - r.left, r.bottom - r.top];
                a.settings.maximized = wp.showCmd == SW_SHOWMAXIMIZED.0 as u32;
            }
        }
        a.settings.save();
        if let Some(d) = crate::settings::dir() {
            let _ = std::fs::write(d.join("visits.txt"), a.visits.text());
        }
    });
}

fn register(class: PCWSTR, proc_: WNDPROC, cursor: PCWSTR, icon: HICON, dbl: bool) {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: if dbl { CS_DBLCLKS } else { WNDCLASS_STYLES(0) },
            lpfnWndProc: proc_,
            hInstance: hinst.into(),
            hIcon: icon,
            hIconSm: icon,
            hCursor: LoadCursorW(None, cursor).unwrap_or_default(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
    }
}

pub fn app_icon() -> HICON {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        LoadIconW(Some(hinst.into()), PCWSTR(1 as *const u16)).unwrap_or_default()
    }
}

fn child(class: PCWSTR, style: WINDOW_STYLE, parent: HWND) -> HWND {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!(""),
            WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS | style,
            0,
            0,
            10,
            10,
            Some(parent),
            None,
            Some(hinst.into()),
            None,
        )
        .unwrap_or_default()
    }
}

pub struct Start {
    pub loc: Loc,
    pub select: Option<String>,
    pub listing: Option<std::thread::JoinHandle<Result<Vec<dvfiles::fsx::Entry>, String>>>,
    pub job: Option<(dvfiles::ops::Kind, Vec<String>, String)>,
}

pub fn run(start: Start, settings: Settings) {
    unsafe {
        crate::trace::mark("set app mode");
        set_app_mode(app_mode(settings.theme));
        let icon = app_icon();
        register(w!("dvfilesMain"), Some(main_proc), IDC_ARROW, icon, true);
        register(w!("dvfilesList"), Some(list::proc_), IDC_ARROW, HICON::default(), true);
        register(w!("dvfilesNav"), Some(nav::proc_), IDC_ARROW, HICON::default(), true);
        register(w!("dvfilesPreview"), Some(preview::proc_), IDC_ARROW, HICON::default(), false);
        opsui::register();
        props::register();
        prefs::register();
        viewer::register();

        let [x, y, w, h] = settings.window;
        let (x, y) = if w > 0 && x == 0 && y == 0 { (CW_USEDEFAULT, CW_USEDEFAULT) } else { (x, y) };
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let main = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("dvfilesMain"),
            w!("dvfiles"),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .expect("main window");
        crate::trace::mark("main window created");
        dvfiles::recycle::OWNER.store(main.0 as isize, std::sync::atomic::Ordering::Relaxed);
        let scale = GetDpiForWindow(main).max(96) as f32 / 96.0;
        let theme = Theme::for_mode(settings.theme);
        // the first frame's icons, drawn on a spare thread while the rest of the window is made
        art::draw_ahead(scale, theme);
        // CW_USEDEFAULT sizes are in pixels; settings sizes are too, but scale the first-run default
        if settings.window[0] == 0 && settings.window[1] == 0 && scale > 1.0 {
            let _ = SetWindowPos(
                main,
                None,
                0,
                0,
                (w as f32 * scale) as i32,
                (h as f32 * scale) as i32,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        let list = child(w!("dvfilesList"), WS_TABSTOP, main);
        let nav = child(w!("dvfilesNav"), WINDOW_STYLE(0), main);
        // the preview pane's window is made when the pane first shows (see layout)
        let preview = HWND::default();
        // the address and search text boxes are made on first use (see ensure_edits)
        let (addr, search) = (HWND::default(), HWND::default());
        accept_files(list, true);
        accept_files(nav, true);

        crate::trace::mark("children created");
        apply_theme(&[list, nav], main, &theme, settings.theme);
        let fonts = Fonts::new(scale);

        crate::trace::mark("fonts and theme");
        let show_hidden = settings.show_hidden.unwrap_or_else(crate::settings::explorer_shows_hidden);
        let known = shell::known_folders();
        let mut model = Model::new(start.loc.clone(), show_hidden);
        if let Some(s) = &start.select {
            model.pending_select.push(s.clone());
        }
        let mut navs = nav::Nav::new(&known, &settings.pins);
        navs.mark_current(&start.loc);
        let app = App {
            main,
            list,
            nav,
            preview,
            addr,
            search,
            scale,
            fonts,
            input_brush: CreateSolidBrush(theme.input_bg.colorref()),
            theme,
            settings,
            icons: icons::Icons::start(main, scale),
            mdc: CreateCompatibleDC(None),
            nf: locale::numbers(),
            model,
            generation: 1,
            back: Vec::new(),
            fwd: Vec::new(),
            drives: Vec::new(),
            known,
            navs,
            lv: list::ListState::default(),
            bar: bar::BarState::default(),
            surf_main: None,
            surf_status: None,
            surf_edge: None,
            surf_list: None,
            surf_nav: None,
            surf_preview: None,
            pv: preview::State::default(),
            backdrop: None,
            glass_live: false,
            glass_pace: 0,
            bubble: None,
            caption: None,
            watcher: None,
            cut: None,
            addr_editing: false,
            laid_out: (0, 0),
            busy: None,
            size_cache: std::collections::HashMap::new(),
            sizer: None,
            undo: Vec::new(),
            shelf: Vec::new(),
            new_since: None,
            visits: crate::settings::dir()
                .and_then(|d| std::fs::read_to_string(d.join("visits.txt")).ok())
                .map_or_else(Default::default, |t| dvfiles::visits::Visits::parse(&t)),
            thumbs: thumbs::Thumbs::new(main),
            tabs: tabs::Tabs::default(),
            art: art::Cache::default(),
        };
        crate::trace::mark("app built");
        APP.with(|c| *c.borrow_mut() = Some(app));
        model::spawn_drives(main);
        // the start folder was read on a thread while the window was being made
        let first = start.listing.map(|t| t.join().unwrap_or_else(|_| Err("The folder could not be read.".into())));
        match first {
            Some(result) => on_listed(Box::new(model::Listed {
                generation: 1,
                loc: start.loc.clone(),
                result,
                refresh: false,
                ms: 0.0,
            })),
            None => model::spawn_list(main, 1, start.loc.clone(), false),
        }
        crate::trace::mark("first listing applied");
        layout();
        update_title();
        // draw the first frame before the window appears, so it never shows blank
        with_app(|a| {
            bar::paint(a);
            crate::trace::mark("bar painted");
            list::paint(a);
            crate::trace::mark("list painted");
            nav::paint(a);
        });
        crate::trace::mark("first frame painted");
        let maximized = with_app(|a| a.settings.maximized).unwrap_or(false);
        // shown first, activated right after: activation starts the input method and text services, which takes
        // longer than drawing the window and need not hold it back
        let size_before = with_app(|a| a.laid_out);
        let _ = ShowWindow(main, if maximized { SW_SHOWMAXIMIZED } else { SW_SHOWNA });
        crate::trace::mark("visible");
        if !quiet() {
            let _ = SetForegroundWindow(main);
        }
        crate::trace::mark("shown");
        // the frame drawn before showing is already on screen: no second paint, unless showing changed the size
        if with_app(|a| a.laid_out) == size_before {
            for h in [main, list, nav] {
                let _ = ValidateRect(Some(h), None);
            }
        }
        focus(list);
        poke_idle();
        let _ = PostMessageW(Some(main), WM_AFTER_SHOW, WPARAM(0), LPARAM(0));
        if let Some((kind, items, dest)) = start.job {
            cmds::start_job(kind, items, Some(dest), None);
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if matches!(msg.message, WM_KEYDOWN | WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MOUSEWHEEL) {
                poke_idle();
            }
            if pretranslate(&msg) {
                continue;
            }
            if opsui::dialog_message(&msg) || props::dialog_message(&msg) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        APP.with(|c| {
            if let Some(a) = c.borrow_mut().take() {
                let _ = DeleteObject(a.input_brush.into());
                let _ = DeleteDC(a.mdc);
            }
        });
    }
}
