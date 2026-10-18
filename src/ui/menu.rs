// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! dvfiles' own popup menus, drawn like the rest of the window: the theme's colours, square corners, a hairline edge
//! and Windows' drop shadow, dvfiles' marks for checks and submenus. Windows' menus keep Windows' look in a dark
//! window and load the theme engine's menu parts the first time one opens.
//!
//! `track` stands in for `TrackPopupMenuEx` with `TPM_RETURNCMD`: it shows the menu without taking activation from
//! the window, runs a message loop of its own until an item is chosen or the menu is dismissed, and returns the
//! item's id. Keys and the mouse go to the menu; everything else (listings, thumbnails, jobs) is dispatched as
//! usual. Moving through a menu (`step`, `by_letter`) and placing it on the screen (`place`) are plain functions,
//! unit-tested below. The shell's own menu (Show more options) stays Windows', as programs add to it.
use crate::ui::cmds::{F_CHECK, F_DEFAULT, F_OFF, M};
use crate::ui::gfx::{self, Rgb, Surface, Theme, R};
use crate::ui::thumbs::Image;
use crate::ui::{wstr, x_of, y_of};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::NotifyWinEvent;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// one line of a menu
#[derive(Clone, Debug)]
pub struct Row {
    pub label: String,
    // the key written at the right ("Ctrl+C")
    pub key: String,
    pub kind: Kind,
    pub check: bool,
    pub off: bool,
    pub default: bool,
    // a picture of its own (the shell's items: 7-Zip, Send to's places), drawn where a check would be
    pub icon: Option<Rc<Image>>,
    // the underlined letter of a label written with "&", which picks it from the keyboard
    pub mnemonic: Option<char>,
    // a submenu whose rows are only made when it opens (the shell's "Send to" and "Open with")
    pub fill: Option<Filler>,
}

/// makes a submenu's rows when it first opens
#[derive(Clone)]
pub struct Filler(pub Rc<dyn Fn() -> Vec<Row>>);

impl std::fmt::Debug for Filler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Filler")
    }
}

#[derive(Clone, Debug)]
pub enum Kind {
    Item(u32),
    Sep,
    Sub(Vec<Row>),
}

/// the menu builder's items as rows
pub fn rows(items: &[M]) -> Vec<Row> {
    items
        .iter()
        .map(|it| match it {
            M::Item(id, label, flags) => {
                let (label, key) = label.split_once('\t').unwrap_or((label, ""));
                Row {
                    label: label.to_string(),
                    key: key.to_string(),
                    kind: Kind::Item(*id),
                    check: flags & F_CHECK != 0,
                    off: flags & F_OFF != 0,
                    default: flags & F_DEFAULT != 0,
                    icon: None,
                    mnemonic: None,
                    fill: None,
                }
            }
            M::Sep => Row {
                label: String::new(),
                key: String::new(),
                kind: Kind::Sep,
                check: false,
                off: true,
                default: false,
                icon: None,
                mnemonic: None,
                fill: None,
            },
            M::Sub(label, sub) => Row {
                label: label.clone(),
                key: String::new(),
                kind: Kind::Sub(rows(sub)),
                check: false,
                off: sub.is_empty(),
                default: false,
                icon: None,
                mnemonic: None,
                fill: None,
            },
        })
        .collect()
}

fn usable(r: &Row) -> bool {
    !r.off && !matches!(r.kind, Kind::Sep)
}

/// the next usable row after `from`, going down or up and round, passing over separators and unavailable items
pub fn step(rows: &[Row], from: Option<usize>, down: bool) -> Option<usize> {
    let n = rows.len();
    let mut i = from.unwrap_or(if down { n.wrapping_sub(1) } else { 0 });
    for _ in 0..n {
        i = if down { (i + 1) % n } else { (i + n - 1) % n };
        if usable(&rows[i]) {
            return Some(i);
        }
    }
    None
}

/// The next usable row after `from` whose label starts with `c`, round to the first, and whether it is the only
/// one. The labels have no underlined letters, so this is how a letter picks an item.
pub fn by_letter(rows: &[Row], from: Option<usize>, c: char) -> Option<(usize, bool)> {
    let c = c.to_lowercase().next()?;
    // a label's underlined letter, else its first
    let letter = |r: &Row| r.mnemonic.or_else(|| r.label.chars().next()).and_then(|f| f.to_lowercase().next());
    let hits: Vec<usize> = (0..rows.len()).filter(|&i| usable(&rows[i]) && letter(&rows[i]) == Some(c)).collect();
    let first = *hits.first()?;
    let next = hits.iter().copied().find(|&i| from.is_some_and(|f| i > f)).unwrap_or(first);
    Some((next, hits.len() == 1))
}

/// Where a menu `w` by `h` goes: its top left at `at`, or beside `beside` (a submenu, next to the row that opened
/// it), turned left or up to stay inside the work area `work`.
pub fn place(w: i32, h: i32, at: (i32, i32), beside: Option<R>, work: R) -> (i32, i32) {
    let (mut x, mut y) = match beside {
        Some(b) => {
            let x = if b.right() + w <= work.right() { b.right() } else { b.x - w };
            (x, b.y)
        }
        None => {
            let x = if at.0 + w <= work.right() { at.0 } else { at.0 - w };
            let y = if at.1 + h <= work.bottom() { at.1 } else { at.1 - h };
            (x, y)
        }
    };
    if y + h > work.bottom() {
        y = work.bottom() - h;
    }
    x = x.clamp(work.x, (work.right() - w).max(work.x));
    y = y.clamp(work.y, (work.bottom() - h).max(work.y));
    (x, y)
}

// ---------------------------------------------------------------------------------------------------------------
// the look

/// what a menu is drawn with, taken from the app before the menu's loop starts
pub struct Style {
    theme: Theme,
    ui: HFONT,
    bold: HFONT,
    scale: f32,
    check: Rc<Image>,
    check_off: Rc<Image>,
    sub: Rc<Image>,
    sub_off: Rc<Image>,
    up: Rc<Image>,
    down: Rc<Image>,
}

impl Style {
    pub fn of(a: &mut crate::ui::App) -> Style {
        use crate::ui::art::Mark;
        let t = a.theme;
        let (m, k) = (a.s(12.0), a.s(14.0));
        Style {
            theme: t,
            ui: a.fonts.ui,
            bold: a.fonts.bold,
            scale: a.scale,
            check: a.art.mark(Mark::Check, k, t.fg),
            check_off: a.art.mark(Mark::Check, k, t.faint),
            sub: a.art.mark(Mark::Right, m, t.muted),
            sub_off: a.art.mark(Mark::Right, m, t.faint),
            up: a.art.mark(Mark::Up, m, t.muted),
            down: a.art.mark(Mark::Down, m, t.muted),
        }
    }

    fn s(&self, v: f32) -> i32 {
        (v * self.scale).round() as i32
    }

    // the menu's surface, the pointed row, its edge and its separators: a little above the window's own surfaces,
    // as Windows 10's menus sit
    fn colors(&self) -> (Rgb, Rgb, Rgb, Rgb) {
        let t = &self.theme;
        if t.dark {
            (Rgb::hex(0x2B2B2B), Rgb::hex(0x3A3A3A), Rgb::hex(0x454545), Rgb::hex(0x3D3D3D))
        } else {
            (Rgb::hex(0xFFFFFF), Rgb::hex(0xF0F1F3), Rgb::hex(0xD0D2D6), Rgb::hex(0xE6E7EA))
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// one open menu (the first, or a submenu)

struct Level {
    rows: Vec<Row>,
    hwnd: HWND,
    // on the screen
    rect: R,
    // each row's top and height in the list of rows
    ys: Vec<(i32, i32)>,
    content_h: i32,
    // a menu taller than the screen scrolls between two strips
    scrolls: bool,
    scroll: i32,
    hot: Option<usize>,
    // the row of the menu above that opened this one
    opener: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hit {
    Row(usize),
    Up,
    Down,
    Edge,
}

impl Level {
    fn top(&self, st: &Style) -> i32 {
        if self.scrolls {
            st.s(18.0)
        } else {
            st.s(4.0)
        }
    }

    fn view_h(&self, st: &Style) -> i32 {
        self.rect.h - 2 * self.top(st)
    }

    fn max_scroll(&self, st: &Style) -> i32 {
        (self.content_h - self.view_h(st)).max(0)
    }

    /// what is at (x, y) inside this menu, in its own coordinates
    fn hit(&self, st: &Style, x: i32, y: i32) -> Option<Hit> {
        if x < 0 || y < 0 || x >= self.rect.w || y >= self.rect.h {
            return None;
        }
        let top = self.top(st);
        if self.scrolls && y < top {
            return Some(Hit::Up);
        }
        if self.scrolls && y >= self.rect.h - top {
            return Some(Hit::Down);
        }
        let cy = y - top + self.scroll;
        Some(self.ys.iter().position(|&(ry, rh)| cy >= ry && cy < ry + rh).map_or(Hit::Edge, Hit::Row))
    }

    /// a row's place on the screen
    fn row_rect(&self, st: &Style, i: usize) -> R {
        let (ry, rh) = self.ys[i];
        R::new(self.rect.x, self.rect.y + self.top(st) + ry - self.scroll, self.rect.w, rh)
    }

    fn reveal(&mut self, st: &Style, i: usize) {
        let (ry, rh) = self.ys[i];
        let vh = self.view_h(st);
        if ry < self.scroll {
            self.scroll = ry;
        } else if ry + rh > self.scroll + vh {
            self.scroll = ry + rh - vh;
        }
        self.scroll = self.scroll.clamp(0, self.max_scroll(st));
    }
}

thread_local! {
    static CLASS: Cell<bool> = const { Cell::new(false) };
    // each open menu's picture, for WM_PAINT
    static SURFACES: RefCell<Vec<(isize, Surface)>> = const { RefCell::new(Vec::new()) };
    // set when the menu loses the mouse (another program came forward): the loop then closes it
    static LOST: Cell<bool> = const { Cell::new(false) };
    // closed menus' windows, hidden and kept for the next menu (with the window they belong to): making a window
    // and destroying it took about as long as drawing the whole menu
    static SPARE: RefCell<Vec<(isize, HWND)>> = const { RefCell::new(Vec::new()) };
}

/// a window for a menu of `owner`: a spare one, or a new one
fn window(owner: HWND, x: i32, y: i32, w: i32, h: i32) -> HWND {
    let spare = SPARE.with(|s| {
        let mut s = s.borrow_mut();
        let i = s.iter().position(|(o, _)| *o == owner.0 as isize)?;
        Some(s.swap_remove(i).1)
    });
    unsafe {
        if let Some(hwnd) = spare.filter(|h| IsWindow(Some(*h)).as_bool()) {
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE);
            return hwnd;
        }
        register();
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            w!("dvfilesMenu"),
            PCWSTR::null(),
            WS_POPUP,
            x,
            y,
            w,
            h,
            Some(owner),
            None,
            Some(GetModuleHandleW(None).unwrap_or_default().into()),
            None,
        )
        .unwrap_or_default();
        // rounded as Windows 11 rounds its own menus (older versions keep them square)
        let round = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &round as *const _ as *const _, 4);
        hwnd
    }
}

/// hides a closed menu's window and keeps it for the next menu
fn put_away(owner: HWND, hwnd: HWND) {
    unsafe {
        NotifyWinEvent(EVENT_SYSTEM_MENUPOPUPEND, hwnd, OBJID_CLIENT.0, CHILDID_SELF as i32);
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    SURFACES.with(|s| s.borrow_mut().retain(|(h, _)| *h != hwnd.0 as isize));
    let extra = SPARE.with(|s| {
        let mut s = s.borrow_mut();
        s.push((owner.0 as isize, hwnd));
        (s.len() > 3).then(|| s.remove(0).1)
    });
    if let Some(h) = extra {
        unsafe {
            let _ = DestroyWindow(h);
        }
    }
}

const T_HOVER: usize = 1;
const T_SCROLL: usize = 2;
// a click on another program does not reach the menu: it looks every so often whether the window is still in front
const T_FRONT: usize = 3;

extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
                SURFACES.with(|s| {
                    if let Ok(s) = s.try_borrow() {
                        if let Some((_, surf)) = s.iter().find(|(h, _)| *h == hwnd.0 as isize) {
                            surf.present(hwnd);
                        }
                    }
                });
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_CAPTURECHANGED => {
                LOST.set(true);
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
            style: CS_DROPSHADOW,
            lpfnWndProc: Some(proc_),
            hInstance: GetModuleHandleW(None).unwrap_or_default().into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("dvfilesMenu"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
    }
    CLASS.set(true);
}

fn work_area(x: i32, y: i32) -> R {
    unsafe {
        let mon = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        if GetMonitorInfoW(mon, &mut mi).as_bool() {
            let r = mi.rcWork;
            R::new(r.left, r.top, r.right - r.left, r.bottom - r.top)
        } else {
            R::new(0, 0, GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN))
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// a menu on the screen and its loop

enum Next {
    Stay,
    Done(Option<u32>),
}

struct Tracker {
    st: Style,
    owner: HWND,
    levels: Vec<Level>,
    // a menu opened by pressing a button gets that button's release: it counts only once the pointer has moved,
    // or after a press inside the menu, so it does not pick whatever row was under the pointer
    armed: bool,
    start: (i32, i32),
    // where the pointer was last: Windows also reports a still pointer when a window appears under it, and that
    // must not take the lit row from the keyboard
    last: (i32, i32),
    // a row pointed at, whose submenu opens (or whose sibling's closes) after Windows' menu delay
    pending: Option<(usize, Option<usize>)>,
    delay: u32,
    scrolling: Option<(usize, i32)>,
}

impl Tracker {
    fn open(&mut self, rows: Vec<Row>, at: (i32, i32), beside: Option<R>, opener: Option<usize>) {
        let st = &self.st;
        let (row_h, sep_h, pad) = (st.s(28.0), st.s(9.0), st.s(4.0));
        let probe = Surface::new(1, 1);
        let width = |s: &str, f: HFONT| probe.as_ref().map_or(s.len() as i32 * 7, |p| gfx::text_width(p.dc, s, f));
        let label_w = rows.iter().map(|r| width(&r.label, if r.default { st.bold } else { st.ui })).max().unwrap_or(0);
        let key_w = rows.iter().map(|r| width(&r.key, st.ui)).max().unwrap_or(0);
        let mut ys = Vec::with_capacity(rows.len());
        let mut y = 0;
        for r in &rows {
            let h = if matches!(r.kind, Kind::Sep) { sep_h } else { row_h };
            ys.push((y, h));
            y += h;
        }
        let content_h = y;
        let work = work_area(beside.map_or(at.0, |b| b.right()), beside.map_or(at.1, |b| b.y));
        let w = (st.s(36.0) + label_w + if key_w > 0 { st.s(40.0) + key_w } else { 0 } + st.s(32.0))
            .clamp(st.s(180.0), (work.w / 2).max(st.s(180.0)));
        let max_h = work.h - st.s(8.0);
        let scrolls = content_h + 2 * pad > max_h;
        let h = if scrolls { max_h } else { content_h + 2 * pad };
        // a submenu's first row sits level with the row that opened it
        let beside = beside.map(|b| R::new(b.x, b.y - pad - 1, b.w, b.h));
        let (x, y) = place(w, h, at, beside, work);
        let hwnd = window(self.owner, x, y, w, h);
        self.levels.push(Level {
            rows,
            hwnd,
            rect: R::new(x, y, w, h),
            ys,
            content_h,
            scrolls,
            scroll: 0,
            hot: None,
            opener,
        });
        let k = self.levels.len() - 1;
        // drawn first, put on the screen once the window shows
        self.draw(k);
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            NotifyWinEvent(EVENT_SYSTEM_MENUPOPUPSTART, hwnd, OBJID_CLIENT.0, CHILDID_SELF as i32);
        }
        self.present(k);
    }

    /// closes the menus below level `k`
    fn close_below(&mut self, k: usize) {
        let closing = self.levels.len() > k + 1;
        while self.levels.len() > k + 1 {
            let l = self.levels.pop().unwrap();
            put_away(self.owner, l.hwnd);
        }
        if closing {
            self.paint(k);
        }
    }

    /// opens the submenu of row `i` of level `k`, with its first row lit when the keyboard opened it
    fn open_sub(&mut self, k: usize, i: usize, keyboard: bool) {
        if self.levels.get(k + 1).is_some_and(|l| l.opener == Some(i)) {
            if keyboard {
                let sub = self.levels.len() - 1;
                let f = step(&self.levels[sub].rows, None, true);
                self.set_hot(sub, f);
            }
            return;
        }
        self.close_below(k);
        // a submenu made only now, once
        if let Some(fill) = self.levels[k].rows[i].fill.take() {
            self.levels[k].rows[i].kind = Kind::Sub((fill.0)());
        }
        let Kind::Sub(rows) = &self.levels[k].rows[i].kind else { return };
        let rows = rows.clone();
        let beside = self.levels[k].row_rect(&self.st, i);
        self.open(rows, (beside.right(), beside.y), Some(beside), Some(i));
        if keyboard {
            let sub = self.levels.len() - 1;
            let f = step(&self.levels[sub].rows, None, true);
            self.set_hot(sub, f);
        }
    }

    fn set_hot(&mut self, k: usize, hot: Option<usize>) {
        let l = &mut self.levels[k];
        if l.hot == hot {
            return;
        }
        l.hot = hot;
        if let Some(i) = hot {
            l.reveal(&self.st, i);
            // what a screen reader is told: the menu's name becomes the lit row's, and the focus moves to it
            let name = wstr(&l.rows[i].label);
            unsafe {
                let _ = SetWindowTextW(l.hwnd, PCWSTR(name.as_ptr()));
                NotifyWinEvent(EVENT_OBJECT_FOCUS, l.hwnd, OBJID_CLIENT.0, CHILDID_SELF as i32);
            }
        }
        self.paint(k);
    }

    /// the menu and what is at a point on the screen, the deepest menu first
    fn at(&self, x: i32, y: i32) -> Option<(usize, Hit)> {
        (0..self.levels.len()).rev().find_map(|k| {
            let l = &self.levels[k];
            l.hit(&self.st, x - l.rect.x, y - l.rect.y).map(|h| (k, h))
        })
    }

    fn timer(&self, id: usize, ms: u32) {
        unsafe {
            SetTimer(Some(self.levels[0].hwnd), id, ms, None);
        }
    }

    fn kill(&self, id: usize) {
        unsafe {
            let _ = KillTimer(Some(self.levels[0].hwnd), id);
        }
    }

    fn mouse_move(&mut self, x: i32, y: i32) {
        if (x, y) == self.last {
            return;
        }
        self.last = (x, y);
        if !self.armed && ((x - self.start.0).abs() > 2 || (y - self.start.1).abs() > 2) {
            self.armed = true;
        }
        let hit = self.at(x, y);
        if let Some((k, _)) = hit {
            // inside a submenu, the rows that lead to it stay lit, and one waiting to close stays open
            for j in (1..=k).rev() {
                let op = self.levels[j].opener;
                if self.levels[j - 1].hot != op {
                    self.set_hot(j - 1, op);
                }
            }
            if self.pending.is_some_and(|(pk, _)| pk < k) {
                self.pending = None;
                self.kill(T_HOVER);
            }
        }
        match hit {
            Some((k, Hit::Row(i))) => {
                self.stop_scroll();
                let hot = usable(&self.levels[k].rows[i]).then_some(i);
                if self.levels[k].hot != hot {
                    self.set_hot(k, hot);
                }
                // back on the row whose submenu is open: nothing to wait for
                let open = self.levels.get(k + 1).and_then(|l| l.opener);
                if open.is_some() && open == hot {
                    self.pending = None;
                    self.kill(T_HOVER);
                    return;
                }
                if self.pending != Some((k, hot))
                    && (open.is_some() || hot.is_some_and(|i| matches!(self.levels[k].rows[i].kind, Kind::Sub(_))))
                {
                    self.pending = Some((k, hot));
                    self.timer(T_HOVER, self.delay);
                }
            }
            Some((k, Hit::Up)) => self.start_scroll(k, -1),
            Some((k, Hit::Down)) => self.start_scroll(k, 1),
            Some((_, Hit::Edge)) => self.stop_scroll(),
            None => {
                self.stop_scroll();
                // off the menus, the deepest one lights nothing, unless one of its rows has a submenu open
                let k = self.levels.len() - 1;
                if self.levels[k].hot.is_some() {
                    self.set_hot(k, None);
                }
            }
        }
    }

    fn hover_due(&mut self) {
        self.kill(T_HOVER);
        let Some((k, hot)) = self.pending.take() else { return };
        if k >= self.levels.len() || self.levels[k].hot != hot {
            return;
        }
        match hot {
            Some(i) if matches!(self.levels[k].rows[i].kind, Kind::Sub(_)) => self.open_sub(k, i, false),
            _ => self.close_below(k),
        }
    }

    fn start_scroll(&mut self, k: usize, dir: i32) {
        if self.scrolling != Some((k, dir)) {
            self.scrolling = Some((k, dir));
            self.timer(T_SCROLL, 35);
        }
    }

    fn stop_scroll(&mut self) {
        if self.scrolling.take().is_some() {
            self.kill(T_SCROLL);
        }
    }

    fn scroll_by(&mut self, k: usize, dy: i32) {
        let l = &mut self.levels[k];
        let to = (l.scroll + dy).clamp(0, l.max_scroll(&self.st));
        if to != l.scroll {
            l.scroll = to;
            self.close_below(k);
            self.paint(k);
        }
    }

    fn press(&mut self, x: i32, y: i32) -> Next {
        match self.at(x, y) {
            None => Next::Done(None),
            Some((k, Hit::Up)) => {
                self.scroll_by(k, -self.st.s(28.0));
                Next::Stay
            }
            Some((k, Hit::Down)) => {
                self.scroll_by(k, self.st.s(28.0));
                Next::Stay
            }
            _ => {
                self.armed = true;
                Next::Stay
            }
        }
    }

    fn release(&mut self, x: i32, y: i32) -> Next {
        if !self.armed {
            self.armed = true;
            return Next::Stay;
        }
        match self.at(x, y) {
            Some((k, Hit::Row(i))) => self.choose(k, i, false),
            _ => Next::Stay,
        }
    }

    fn choose(&mut self, k: usize, i: usize, keyboard: bool) -> Next {
        let row = &self.levels[k].rows[i];
        if !usable(row) {
            return Next::Stay;
        }
        match row.kind {
            Kind::Item(id) => Next::Done(Some(id)),
            Kind::Sub(_) => {
                self.open_sub(k, i, keyboard);
                Next::Stay
            }
            Kind::Sep => Next::Stay,
        }
    }

    fn key(&mut self, vk: VIRTUAL_KEY) -> Next {
        let k = self.levels.len() - 1;
        let hot = self.levels[k].hot;
        match vk {
            VK_DOWN | VK_UP => {
                let n = step(&self.levels[k].rows, hot, vk == VK_DOWN);
                self.set_hot(k, n);
            }
            VK_HOME | VK_END => {
                let n = step(&self.levels[k].rows, None, vk == VK_HOME);
                self.set_hot(k, n);
            }
            VK_NEXT | VK_PRIOR => {
                // a page, for long menus
                let rows = (self.levels[k].view_h(&self.st) / self.st.s(28.0)).max(1);
                let mut n = hot;
                for _ in 0..rows {
                    let m = step(&self.levels[k].rows, n, vk == VK_NEXT);
                    if m.zip(n).is_some_and(|(m, n)| (vk == VK_NEXT) != (m > n)) {
                        break;
                    }
                    n = m;
                }
                self.set_hot(k, n);
            }
            VK_RIGHT => {
                if let Some(i) = hot.filter(|&i| matches!(self.levels[k].rows[i].kind, Kind::Sub(_))) {
                    self.open_sub(k, i, true);
                }
            }
            VK_LEFT | VK_ESCAPE if k > 0 => {
                let op = self.levels[k].opener;
                self.close_below(k - 1);
                self.set_hot(k - 1, op);
            }
            VK_ESCAPE => return Next::Done(None),
            VK_RETURN | VK_SPACE => {
                if let Some(i) = hot {
                    return self.choose(k, i, true);
                }
            }
            VK_MENU | VK_F10 | VK_LWIN | VK_RWIN => return Next::Done(None),
            _ => {}
        }
        Next::Stay
    }

    fn letter(&mut self, c: char) -> Next {
        if c.is_control() || c == ' ' {
            return Next::Stay;
        }
        let k = self.levels.len() - 1;
        match by_letter(&self.levels[k].rows, self.levels[k].hot, c) {
            Some((i, true)) => {
                self.set_hot(k, Some(i));
                self.choose(k, i, true)
            }
            Some((i, false)) => {
                self.set_hot(k, Some(i));
                Next::Stay
            }
            None => Next::Stay,
        }
    }

    fn paint(&self, k: usize) {
        self.draw(k);
        self.present(k);
    }

    fn present(&self, k: usize) {
        let hwnd = self.levels[k].hwnd;
        SURFACES.with(|s| {
            if let Some((_, surf)) = s.borrow().iter().find(|(h, _)| *h == hwnd.0 as isize) {
                surf.present(hwnd);
            }
        });
    }

    fn draw(&self, k: usize) {
        let st = &self.st;
        let l = &self.levels[k];
        let t = &st.theme;
        let (bg, hover, border, line) = st.colors();
        let (w, h) = (l.rect.w, l.rect.h);
        SURFACES.with(|s| {
            let mut s = s.borrow_mut();
            let key = l.hwnd.0 as isize;
            if !s.iter().any(|(k, surf)| *k == key && surf.w == w && surf.h == h) {
                s.retain(|(k, _)| *k != key);
                if let Some(surf) = Surface::new(w, h) {
                    s.push((key, surf));
                }
            }
            let Some((_, surf)) = s.iter().find(|(k, _)| *k == key) else { return };
            surf.fill(R::new(0, 0, w, h), bg);
            let top = l.top(st);
            let view = R::new(1, top, w - 2, l.view_h(st));
            surf.set_clip(Some(view));
            let (cx, key_right, sub_x) = (st.s(36.0), w - st.s(32.0), w - st.s(28.0));
            for (i, row) in l.rows.iter().enumerate() {
                let (ry, rh) = l.ys[i];
                let y = top + ry - l.scroll;
                if y + rh < top || y > top + view.h {
                    continue;
                }
                if let Kind::Sep = row.kind {
                    surf.fill(R::new(st.s(12.0), y + rh / 2, w - st.s(24.0), 1), line);
                    continue;
                }
                let open = self.levels.get(k + 1).is_some_and(|n| n.opener == Some(i));
                if l.hot == Some(i) || open {
                    surf.round(R::new(st.s(4.0), y, w - st.s(8.0), rh), st.s(5.0) as f32, hover);
                }
                let fg = if row.off { t.faint } else { t.fg };
                if row.check {
                    surf.image_in(
                        R::new(st.s(4.0), y, cx - st.s(4.0), rh),
                        if row.off { &st.check_off } else { &st.check },
                    );
                } else if let Some(icon) = &row.icon {
                    surf.image_in(R::new(st.s(4.0), y, cx - st.s(4.0), rh), icon);
                }
                let font = if row.default { st.bold } else { st.ui };
                surf.text(R::new(cx, y, key_right - cx, rh), &row.label, font, fg, gfx::LEFT);
                if !row.key.is_empty() {
                    surf.text(
                        R::new(cx, y, key_right - cx, rh),
                        &row.key,
                        st.ui,
                        if row.off { t.faint } else { t.muted },
                        gfx::RIGHT,
                    );
                }
                if let Kind::Sub(_) = row.kind {
                    surf.image_in(R::new(sub_x, y, st.s(20.0), rh), if row.off { &st.sub_off } else { &st.sub });
                }
            }
            surf.set_clip(None);
            if l.scrolls {
                let strip = top;
                if l.scroll > 0 {
                    surf.image_in(R::new(0, 0, w, strip), &st.up);
                }
                if l.scroll < l.max_scroll(st) {
                    surf.image_in(R::new(0, h - strip, w, strip), &st.down);
                }
            }
            surf.frame(R::new(0, 0, w, h), border);
        });
    }
}

/// Takes out the characters made from the key that closed a menu or a box (Enter, Space, Esc), which would otherwise
/// reach the window behind it once the loop ends: a space in a rename box, a beep. Windows' menus do the same.
pub fn swallow_chars() {
    let mut m = MSG::default();
    for kind in [WM_CHAR, WM_DEADCHAR, WM_SYSCHAR, WM_SYSDEADCHAR] {
        unsafe { while PeekMessageW(&mut m, None, kind, kind, PM_REMOVE).as_bool() {} }
    }
}

/// Shows the menu at (x, y) on the screen and returns the id of the item chosen, or None when it was dismissed.
pub fn track(owner: HWND, style: Style, items: &[M], x: i32, y: i32) -> Option<u32> {
    track_rows(owner, style, rows(items), x, y)
}

/// `track`, for rows made elsewhere (the shell's menu, read into rows)
pub fn track_rows(owner: HWND, style: Style, rows: Vec<Row>, x: i32, y: i32) -> Option<u32> {
    let mut delay = 400u32;
    let mut start = POINT { x, y };
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETMENUSHOWDELAY,
            0,
            Some(&mut delay as *mut u32 as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
        let _ = GetCursorPos(&mut start);
    }
    let mut t = Tracker {
        st: style,
        owner,
        levels: Vec::new(),
        armed: false,
        start: (start.x, start.y),
        last: (start.x, start.y),
        pending: None,
        delay: delay.max(1),
        scrolling: None,
    };
    t.open(rows, (x, y), None, None);
    let first = t.levels[0].hwnd;
    LOST.set(false);
    unsafe {
        SetCapture(first);
    }
    t.timer(T_FRONT, 100);
    let root = unsafe { GetAncestor(owner, GA_ROOT) };
    // the window in front when the menu opened (dvfiles', unless something kept it from coming forward)
    let front = unsafe { GetForegroundWindow() };
    let mut msg = MSG::default();
    let result = loop {
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r.0 == 0 {
            // WM_QUIT: closing down; put it back for the main loop
            unsafe { PostQuitMessage(msg.wParam.0 as i32) };
            break None;
        }
        if r.0 == -1 {
            break None;
        }
        let next = match msg.message {
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                crate::trace::log(|| format!("menu key {:#x}", msg.wParam.0));
                crate::ui::poke_idle();
                // letters come as WM_CHAR, made from the key here
                unsafe {
                    let _ = TranslateMessage(&msg);
                }
                t.key(VIRTUAL_KEY(msg.wParam.0 as u16))
            }
            WM_CHAR | WM_SYSCHAR => {
                crate::trace::log(|| format!("menu char {:#x}", msg.wParam.0));
                char::from_u32(msg.wParam.0 as u32).map_or(Next::Stay, |c| t.letter(c))
            }
            WM_KEYUP | WM_SYSKEYUP | WM_DEADCHAR | WM_SYSDEADCHAR => Next::Stay,
            WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONDOWN
            | WM_MBUTTONUP
                if t.levels.iter().any(|l| l.hwnd == msg.hwnd) =>
            {
                // the first menu holds the mouse, so its events come there; any menu's are taken all the same
                let o = t.levels.iter().find(|l| l.hwnd == msg.hwnd).map_or((0, 0), |l| (l.rect.x, l.rect.y));
                let (px, py) = (x_of(msg.lParam) + o.0, y_of(msg.lParam) + o.1);
                match msg.message {
                    WM_MOUSEMOVE => {
                        t.mouse_move(px, py);
                        Next::Stay
                    }
                    WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
                        crate::trace::log(|| format!("menu press {:#x} at {px},{py}", msg.message));
                        crate::ui::poke_idle();
                        t.press(px, py)
                    }
                    WM_MBUTTONUP => Next::Stay,
                    _ => t.release(px, py),
                }
            }
            WM_MOUSEWHEEL => {
                let (px, py) = (x_of(msg.lParam), y_of(msg.lParam));
                let k = t.at(px, py).map_or(t.levels.len() - 1, |(k, _)| k);
                let delta = (msg.wParam.0 >> 16) as u16 as i16 as i32;
                t.scroll_by(k, -delta.signum() * 3 * t.st.s(28.0));
                Next::Stay
            }
            WM_TIMER if msg.hwnd == first && msg.wParam.0 == T_HOVER => {
                t.hover_due();
                Next::Stay
            }
            WM_TIMER if msg.hwnd == first && msg.wParam.0 == T_FRONT => {
                let now = unsafe { GetForegroundWindow() };
                if now != front && now != root {
                    LOST.set(true);
                }
                Next::Stay
            }
            WM_TIMER if msg.hwnd == first && msg.wParam.0 == T_SCROLL => {
                if let Some((k, dir)) = t.scrolling {
                    if k < t.levels.len() {
                        t.scroll_by(k, dir * t.st.s(14.0));
                    }
                }
                Next::Stay
            }
            _ => {
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                Next::Stay
            }
        };
        if let Next::Done(r) = next {
            crate::trace::log(|| format!("menu done: {r:?}"));
            break r;
        }
        if LOST.get() {
            crate::trace::log(|| "menu lost the mouse or the front".to_string());
            break None;
        }
    };
    swallow_chars();
    t.kill(T_HOVER);
    t.kill(T_FRONT);
    t.stop_scroll();
    t.close_below(0);
    let l = t.levels.pop().unwrap();
    unsafe {
        if GetCapture() == first {
            let _ = ReleaseCapture();
        }
    }
    put_away(owner, l.hwnd);
    SURFACES.with(|s| s.borrow_mut().clear());
    result
}

/// A text box's menu (Undo, Cut, Copy, Paste, Delete, Select all) in dvfiles' style, in place of the one Windows
/// gives the box: the address, the search and the rename boxes are Windows' own edit controls.
pub fn edit_menu(owner: HWND, edit: HWND, x: i32, y: i32) {
    use windows::Win32::System::DataExchange::IsClipboardFormatAvailable;
    use windows::Win32::UI::Controls::{EM_CANUNDO, EM_GETSEL, EM_SETSEL};
    let (mut a, mut b) = (0u32, 0u32);
    let (len, read_only, can_undo, text_on_clipboard) = unsafe {
        SendMessageW(
            edit,
            EM_GETSEL,
            Some(WPARAM(&mut a as *mut u32 as usize)),
            Some(LPARAM(&mut b as *mut u32 as isize)),
        );
        (
            GetWindowTextLengthW(edit) as u32,
            GetWindowLongW(edit, GWL_STYLE) & ES_READONLY != 0,
            SendMessageW(edit, EM_CANUNDO, None, None).0 != 0,
            IsClipboardFormatAvailable(13).is_ok(),
        )
    };
    let sel = b > a;
    let on = |yes: bool| if yes { 0 } else { F_OFF };
    let items = vec![
        M::Item(1, "Undo\tCtrl+Z".into(), on(can_undo && !read_only)),
        M::Sep,
        M::Item(2, "Cut\tCtrl+X".into(), on(sel && !read_only)),
        M::Item(3, "Copy\tCtrl+C".into(), on(sel)),
        M::Item(4, "Paste\tCtrl+V".into(), on(text_on_clipboard && !read_only)),
        M::Item(5, "Delete\tDel".into(), on(sel && !read_only)),
        M::Sep,
        M::Item(6, "Select all\tCtrl+A".into(), on(len > 0 && b - a < len)),
    ];
    let Some(style) = crate::ui::with_app(Style::of) else { return };
    let Some(id) = track(owner, style, &items, x, y) else { return };
    unsafe {
        match id {
            6 => SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1))),
            _ => {
                SendMessageW(edit, [WM_UNDO, WM_CUT, WM_COPY, WM_PASTE, WM_CLEAR][(id as usize - 1).min(4)], None, None)
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str) -> Row {
        Row {
            label: label.into(),
            key: String::new(),
            kind: Kind::Item(1),
            check: false,
            off: false,
            default: false,
            icon: None,
            mnemonic: None,
            fill: None,
        }
    }

    fn sep() -> Row {
        Row {
            label: String::new(),
            key: String::new(),
            kind: Kind::Sep,
            check: false,
            off: true,
            default: false,
            icon: None,
            mnemonic: None,
            fill: None,
        }
    }

    #[test]
    fn steps_pass_separators_and_unavailable_items() {
        let mut off = item("Paste");
        off.off = true;
        let rows = vec![item("Open"), sep(), off, item("Copy"), sep()];
        assert_eq!(step(&rows, None, true), Some(0));
        assert_eq!(step(&rows, None, false), Some(3));
        assert_eq!(step(&rows, Some(0), true), Some(3));
        assert_eq!(step(&rows, Some(3), true), Some(0));
        assert_eq!(step(&rows, Some(0), false), Some(3));
        assert_eq!(step(&[sep()], None, true), None);
        assert_eq!(step(&[], None, true), None);
    }

    #[test]
    fn letters_pick_and_cycle() {
        let rows = vec![item("Copy"), item("Cut"), item("Paste"), item("Copy as path")];
        assert_eq!(by_letter(&rows, None, 'c'), Some((0, false)));
        assert_eq!(by_letter(&rows, Some(0), 'C'), Some((1, false)));
        assert_eq!(by_letter(&rows, Some(3), 'c'), Some((0, false)));
        assert_eq!(by_letter(&rows, None, 'p'), Some((2, true)));
        assert_eq!(by_letter(&rows, None, 'x'), None);
        // an underlined letter wins over the first one
        let mut send = item("Send to");
        send.mnemonic = Some('n');
        let rows = vec![item("Share"), send];
        assert_eq!(by_letter(&rows, None, 'n'), Some((1, true)));
        assert_eq!(by_letter(&rows, None, 's'), Some((0, true)));
    }

    #[test]
    fn menus_stay_on_the_screen() {
        let work = R::new(0, 0, 1920, 1040);
        // room below and to the right: where asked
        assert_eq!(place(200, 300, (100, 100), None, work), (100, 100));
        // at the right edge: turned left of the point
        assert_eq!(place(200, 300, (1850, 100), None, work), (1650, 100));
        // at the bottom: turned up
        assert_eq!(place(200, 300, (100, 900), None, work), (100, 600));
        // taller than the screen: from the top
        assert_eq!(place(200, 2000, (100, 900), None, work).1, 0);
        // a submenu beside its row, or on the other side when there is no room
        let row = R::new(500, 200, 220, 30);
        assert_eq!(place(200, 300, (0, 0), Some(row), work), (720, 200));
        let row = R::new(1700, 200, 220, 30);
        assert_eq!(place(200, 300, (0, 0), Some(row), work), (1500, 200));
        // and up, to fit
        let row = R::new(500, 1000, 220, 30);
        assert_eq!(place(200, 300, (0, 0), Some(row), work), (720, 740));
    }

    #[test]
    fn rows_come_from_the_builder() {
        let items = vec![
            M::Item(1, "Copy\tCtrl+C".into(), F_DEFAULT),
            M::Sep,
            M::Sub("View".into(), vec![M::Item(2, "Details".into(), F_CHECK)]),
        ];
        let r = rows(&items);
        assert_eq!((r[0].label.as_str(), r[0].key.as_str(), r[0].default), ("Copy", "Ctrl+C", true));
        assert!(matches!(r[1].kind, Kind::Sep));
        let Kind::Sub(sub) = &r[2].kind else { panic!() };
        assert!(sub[0].check);
    }
}
