// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! The navigation pane: Quick access (Desktop, Downloads, ... and the user's pins) and This PC with its drives.
//! Folders expand in place; their subfolders are read when first opened. A folder in the tree can be dragged out, as
//! in Windows' tree: it opens when the button is let go, or is dragged once the pointer moves past the drag distance.
use crate::ui::art::{self, Art};
use crate::ui::cmds;
use crate::ui::gfx::{self, Surface, R};
use crate::ui::model::Loc;
use crate::ui::{client_size, invalidate, navigate, with_app, x_of, y_of, App};
use dvfiles::fsx::Drive;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, ScreenToClient, PAINTSTRUCT};
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Header,
    Known,
    Pin,
    Drive,
    Folder,
    /// the shelf: files dropped on it are gathered there, a click offers to copy or move them here
    Shelf,
}

pub struct Node {
    pub label: String,
    pub loc: Option<Loc>,
    pub depth: i32,
    pub expanded: bool,
    pub children: Option<Vec<usize>>,
    pub kind: Kind,
}

#[derive(Default)]
pub struct Nav {
    nodes: Vec<Node>,
    roots: Vec<usize>,
    rows: Vec<usize>,
    pub scroll: i32,
    hover: Option<usize>,
    // the folder lit up as the target of a drag
    pub drop_hot: Option<usize>,
    current: Option<usize>,
    tracking: bool,
    bar_hot: bool,
    // dragging the scrollbar thumb: pointer start and scroll position then
    thumb: Option<(i32, i32)>,
    // a folder row pressed: where, and which row (it opens on release, or is dragged)
    press: Option<(i32, i32, usize)>,
}

impl Nav {
    pub fn new(known: &[(&'static str, String)], pins: &[String]) -> Nav {
        let mut n = Nav::default();
        let quick = n.push(Node {
            label: "Quick access".into(),
            loc: None,
            depth: 0,
            expanded: true,
            children: Some(Vec::new()),
            kind: Kind::Header,
        });
        let mut kids = Vec::new();
        for (label, path) in known {
            kids.push(n.push(Node {
                label: label.to_string(),
                loc: Some(Loc::Dir(path.clone())),
                depth: 1,
                expanded: false,
                children: None,
                kind: Kind::Known,
            }));
        }
        for p in pins {
            let label = dvfiles::pathx::file_name(p).to_string();
            kids.push(n.push(Node {
                label,
                loc: Some(Loc::Dir(p.clone())),
                depth: 1,
                expanded: false,
                children: None,
                kind: Kind::Pin,
            }));
        }
        n.nodes[quick].children = Some(kids);
        let pc = n.push(Node {
            label: "This PC".into(),
            loc: Some(Loc::Pc),
            depth: 0,
            expanded: true,
            children: Some(Vec::new()),
            kind: Kind::Header,
        });
        let drives: Vec<usize> = dvfiles::fsx::drive_roots()
            .into_iter()
            .map(|r| {
                let label = format!("Local Disk ({})", r.trim_end_matches('\\'));
                n.push(Node {
                    label,
                    loc: Some(Loc::Dir(r)),
                    depth: 1,
                    expanded: false,
                    children: None,
                    kind: Kind::Drive,
                })
            })
            .collect();
        n.nodes[pc].children = Some(drives);
        let shelf = n.push(Node {
            label: "Shelf".into(),
            loc: None,
            depth: 0,
            expanded: false,
            children: Some(Vec::new()),
            kind: Kind::Shelf,
        });
        n.roots = vec![quick, pc, shelf];
        n.rebuild();
        n
    }

    fn push(&mut self, node: Node) -> usize {
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        let mut stack: Vec<usize> = self.roots.iter().rev().copied().collect();
        while let Some(i) = stack.pop() {
            rows.push(i);
            let n = &self.nodes[i];
            if n.expanded {
                if let Some(kids) = &n.children {
                    stack.extend(kids.iter().rev());
                }
            }
        }
        self.rows = rows;
    }

    /// the row where the second section (This PC) starts, which gets some space above it
    fn section_row(&self) -> Option<usize> {
        let pc = *self.roots.get(1)?;
        self.rows.iter().position(|&i| i == pc)
    }

    pub fn set_drives(&mut self, drives: &[Drive]) {
        let Some(&pc) = self.roots.get(1) else { return };
        let old = self.nodes[pc].children.clone().unwrap_or_default();
        let mut kids = Vec::new();
        for d in drives {
            let existing = old
                .iter()
                .copied()
                .find(|&i| matches!(&self.nodes[i].loc, Some(Loc::Dir(p)) if p.eq_ignore_ascii_case(&d.root)));
            let i = match existing {
                Some(i) => i,
                None => self.push(Node {
                    label: String::new(),
                    loc: Some(Loc::Dir(d.root.clone())),
                    depth: 1,
                    expanded: false,
                    children: None,
                    kind: Kind::Drive,
                }),
            };
            self.nodes[i].label = d.title();
            kids.push(i);
        }
        self.nodes[pc].children = Some(kids);
        self.rebuild();
    }

    pub fn mark_current(&mut self, loc: &Loc) {
        self.current = self.rows.iter().copied().find(|&i| self.nodes[i].loc.as_ref().is_some_and(|l| l.same(loc)));
    }

    fn expandable(&self, i: usize) -> bool {
        let n = &self.nodes[i];
        match &n.children {
            Some(k) => !k.is_empty(),
            None => true,
        }
    }

    fn toggle(&mut self, i: usize, show_hidden: bool) {
        if self.nodes[i].children.is_none() {
            let path = match &self.nodes[i].loc {
                Some(Loc::Dir(p)) => p.clone(),
                _ => return,
            };
            let mut dirs: Vec<String> = dvfiles::fsx::list(&path)
                .unwrap_or_default()
                .into_iter()
                .filter(|e| e.is_dir() && (show_hidden || !e.is_hidden()) && !e.is_super_hidden())
                .map(|e| e.name.to_string())
                .collect();
            dirs.sort_by(|a, b| dvfiles::natsort::cmp(a, b));
            let depth = self.nodes[i].depth + 1;
            let kids: Vec<usize> = dirs
                .into_iter()
                .map(|name| {
                    let p = dvfiles::pathx::join(&path, &name);
                    self.push(Node {
                        label: name,
                        loc: Some(Loc::Dir(p)),
                        depth,
                        expanded: false,
                        children: None,
                        kind: Kind::Folder,
                    })
                })
                .collect();
            self.nodes[i].children = Some(kids);
        }
        self.nodes[i].expanded = !self.nodes[i].expanded && self.expandable(i);
        self.rebuild();
    }

    pub fn remove_pin(&mut self, path: &str) {
        let Some(&quick) = self.roots.first() else { return };
        let gone: Vec<usize> = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| {
                n.kind == Kind::Pin && matches!(&n.loc, Some(Loc::Dir(p)) if dvfiles::pathx::same(p, path))
            })
            .map(|(i, _)| i)
            .collect();
        if let Some(kids) = &mut self.nodes[quick].children {
            kids.retain(|i| !gone.contains(i));
        }
        self.rebuild();
    }

    pub fn add_pin(&mut self, path: &str) {
        let Some(&quick) = self.roots.first() else { return };
        let label = dvfiles::pathx::file_name(path).to_string();
        let i = self.push(Node {
            label,
            loc: Some(Loc::Dir(path.to_string())),
            depth: 1,
            expanded: false,
            children: None,
            kind: Kind::Pin,
        });
        if let Some(kids) = &mut self.nodes[quick].children {
            kids.push(i);
        }
        self.rebuild();
    }
}

fn row_h(a: &App) -> i32 {
    a.s(32.0).max(a.icons.small_px + a.s(12.0))
}

fn gap(a: &App) -> i32 {
    a.s(12.0)
}

// the top of the k-th row in content coordinates
fn row_top(a: &App, k: usize) -> i32 {
    let extra = if a.navs.section_row().is_some_and(|s| k >= s && s > 0) { gap(a) } else { 0 };
    a.s(4.0) + k as i32 * row_h(a) + extra
}

fn total_h(a: &App) -> i32 {
    row_top(a, a.navs.rows.len()) + a.s(8.0)
}

fn track(a: &App, w: i32, h: i32) -> Option<crate::ui::scroll::Track> {
    let total = total_h(a);
    crate::ui::scroll::bar(
        true,
        R::new(0, 0, w, h),
        total,
        h,
        a.navs.scroll,
        a.scale,
        a.navs.bar_hot || a.navs.thumb.is_some(),
    )
}

pub fn paint(a: &mut App) {
    let (w, h) = client_size(a.nav);
    if w <= 0 || Surface::ensure(&mut a.surf_nav, w, h).is_none() {
        return;
    }
    let rh = row_h(a);
    clamp(a, h);
    let t = a.theme;
    let focused = unsafe { GetFocus() } == a.nav;
    let first = (((a.navs.scroll - gap(a)).max(0)) / rh).max(0) as usize;
    let last = ((a.navs.scroll + h) / rh + 1) as usize;
    let rows: Vec<usize> = a.navs.rows.iter().copied().skip(first).take(last.saturating_sub(first)).collect();
    crate::ui::backdrop(a, a.surf_nav.as_ref().unwrap(), a.nav, R::new(0, 0, w, h));
    let rad = a.rad();

    for (k, i) in rows.into_iter().enumerate() {
        let y = row_top(a, first + k) - a.navs.scroll;
        let (label, depth, kind, loc, expanded) = {
            let n = &a.navs.nodes[i];
            (n.label.clone(), n.depth, n.kind, n.loc.clone(), n.expanded)
        };
        let expandable = a.navs.expandable(i);
        let label =
            if kind == Kind::Shelf && !a.shelf.is_empty() { format!("{label} ({})", a.shelf.len()) } else { label };
        let art = match (kind, &loc) {
            (Kind::Header, None) => Art::Star,
            (Kind::Header, _) => Art::Pc,
            (Kind::Drive, _) => Art::Drive,
            (Kind::Shelf, _) => Art::Tool(art::Tool::Copy),
            (Kind::Known, _) => art::special_for(&label).map_or(Art::Folder, Art::Special),
            _ => Art::Folder,
        };
        let px = a.icons.small_px;
        let img = a.art.get(art, px, &t);
        let surf = a.surf_nav.as_ref().unwrap();
        let f = &a.fonts;
        let r = R::new(a.s(6.0), y + a.s(1.0), w - a.s(12.0), rh - a.s(2.0));
        let current = a.navs.current == Some(i);
        if current {
            crate::ui::bar::lit(surf, r, rad, t.dark, if focused { 0.12 } else { 0.09 });
            // the accent mark on the folder being shown, a short rounded bar as Windows 11 has it
            let bh = a.s(16.0).min(r.h - a.s(8.0));
            let bw = a.s(3.0);
            surf.round(R::new(r.x, r.y + (r.h - bh) / 2, bw, bh), bw as f32 / 2.0, t.accent);
        } else if a.navs.hover == Some(i) {
            crate::ui::bar::lit(surf, r, rad, t.dark, 0.06);
        }
        if a.navs.drop_hot == Some(i) {
            surf.round_blend(r, rad, t.accent, 0.18);
            surf.round_ring(r, rad, t.accent);
        }
        let x = a.s(12.0) + depth * a.s(16.0);
        // Windows 10 shows the expand arrows only while the pane is pointed at or in use
        if expandable && (a.navs.tracking || focused) {
            let m = if expanded { crate::ui::art::Mark::Down } else { crate::ui::art::Mark::Right };
            surf.image_in(R::new(x, y, a.s(16.0), rh), &a.art.mark(m, a.s(12.0), t.faint));
        }
        let ix = x + a.s(20.0);
        surf.image(ix, y + (rh - px) / 2, img.w, img.h, &img.px);
        let tx = ix + px + a.s(10.0);
        let (font, color) = if kind == Kind::Header {
            (f.bold, t.fg)
        } else if current {
            (f.ui, t.fg)
        } else {
            (f.ui, t.fg.mix(t.muted, 0.35))
        };
        surf.text(R::new(tx, y, w - tx - a.s(10.0), rh), &label, font, color, gfx::LEFT);
    }
    if let Some(b) = track(a, w, h) {
        b.draw(a.surf_nav.as_ref().unwrap(), &t, a.navs.bar_hot || a.navs.thumb.is_some());
    }
    a.surf_nav.as_ref().unwrap().present(a.nav);
    a.icons.flush();
}

/// a folder held under a drag opens its branch, so the drop can go deeper
pub fn spring_open(a: &mut App, i: usize) {
    if i < a.navs.nodes.len() && !a.navs.nodes[i].expanded {
        let hidden = a.model.show_hidden;
        a.navs.toggle(i, hidden);
        invalidate(a.nav);
    }
}

fn clamp(a: &mut App, h: i32) {
    let total = total_h(a);
    a.navs.scroll = a.navs.scroll.clamp(0, (total - h).max(0));
}

fn row_at(a: &App, y: i32) -> Option<usize> {
    let cy = y + a.navs.scroll;
    if cy < a.s(4.0) {
        return None;
    }
    let mut k = ((cy - a.s(4.0)) / row_h(a)).max(0) as usize;
    if let Some(s) = a.navs.section_row().filter(|&s| s > 0 && k >= s) {
        // inside the gap above the section, or a row below it
        if cy < row_top(a, s) {
            return None;
        }
        k = ((cy - row_top(a, s)) / row_h(a)) as usize + s;
    }
    a.navs.rows.get(k).copied()
}

fn click(x: i32, y: i32, toggle_only: bool) {
    let go = with_app(|a| {
        let i = row_at(a, y)?;
        let n = &a.navs.nodes[i];
        let chevron_x = a.s(12.0) + n.depth * a.s(16.0);
        let on_chevron = x >= chevron_x - a.s(4.0) && x < chevron_x + a.s(18.0);
        if n.kind == Kind::Shelf {
            let mut pt = POINT { x, y };
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::ClientToScreen(a.nav, &mut pt);
            }
            return Some(Err(pt));
        }
        if on_chevron || toggle_only {
            let hidden = a.model.show_hidden;
            a.navs.toggle(i, hidden);
            invalidate(a.nav);
            return None;
        }
        n.loc.clone().map(Ok)
    })
    .flatten();
    match go {
        Some(Ok(loc)) => navigate(loc, true),
        Some(Err(pt)) => cmds::shelf_popup(pt.x, pt.y),
        None => {}
    }
}

/// The row under a press that can be dragged out: a folder found inside the tree. Desktop, Downloads and the other
/// known folders, the pins and the drives are not: dropped elsewhere they would be moved whole.
fn draggable(a: &App, x: i32, y: i32) -> Option<usize> {
    let i = row_at(a, y)?;
    let n = &a.navs.nodes[i];
    let chevron_x = a.s(12.0) + n.depth * a.s(16.0);
    let on_chevron = x >= chevron_x - a.s(4.0) && x < chevron_x + a.s(18.0);
    (n.kind == Kind::Folder && matches!(n.loc, Some(Loc::Dir(_))) && !on_chevron).then_some(i)
}

/// the folder at row `i` and its drag image
fn drag_of(a: &mut App, i: usize) -> Option<(String, Option<crate::ui::thumbs::Image>)> {
    let Some(Loc::Dir(path)) = a.navs.nodes[i].loc.clone() else { return None };
    let e = dvfiles::fsx::Entry {
        name: dvfiles::pathx::file_name(&path).into(),
        dir: None,
        attrs: dvfiles::fsx::attributes(&path).unwrap_or(dvfiles::fsx::FA_DIRECTORY),
        size: 0,
        modified: 0,
        created: 0,
    };
    let image = crate::ui::drag::picture_of(a, &e, &path, 1);
    Some((path, image))
}

fn menu(x: i32, y: i32) {
    let target = with_app(|a| {
        let mut pt = POINT { x, y };
        unsafe {
            let _ = ScreenToClient(a.nav, &mut pt);
        }
        let i = row_at(a, pt.y)?;
        let n = &a.navs.nodes[i];
        match &n.loc {
            Some(Loc::Dir(p)) => Some(Some((p.clone(), n.kind == Kind::Pin))),
            _ => (n.kind == Kind::Shelf).then_some(None),
        }
    })
    .flatten();
    match target {
        Some(Some((path, pinned))) => cmds::nav_menu(&path, pinned, x, y),
        Some(None) => cmds::shelf_popup(x, y),
        None => {}
    }
}

/// the shelf's row, when it is under a point
pub fn shelf_at(a: &App, y: i32) -> Option<usize> {
    row_at(a, y).filter(|&i| a.navs.nodes[i].kind == Kind::Shelf)
}

/// the folder or drive under a point that a drag would drop into, and its row to light up
pub fn drop_target_at(a: &App, _x: i32, y: i32, paths: &[String]) -> Option<(String, Option<usize>)> {
    let i = row_at(a, y)?;
    match &a.navs.nodes[i].loc {
        Some(Loc::Dir(p)) if !paths.iter().any(|x| dvfiles::pathx::same(x, p) || dvfiles::pathx::is_within(p, x)) => {
            Some((p.clone(), Some(i)))
        }
        _ => None,
    }
}

fn drop_files(hdrop: HGLOBAL) {
    // the drop's DROPFILES block, read directly (DragQueryFile would load the shell), then given back as DragFinish
    // would
    let (paths, (x, y)) = crate::ui::clip::read_hdrop(hdrop);
    unsafe {
        let _ = windows::Win32::Foundation::GlobalFree(Some(hdrop));
    }
    let pt = POINT { x, y };
    let target = with_app(|a| {
        let i = row_at(a, pt.y)?;
        match &a.navs.nodes[i].loc {
            Some(Loc::Dir(p)) => Some(p.clone()),
            _ => None,
        }
    })
    .flatten();
    if let (Some(t), false) = (target, paths.is_empty()) {
        cmds::drop_into(paths, t);
    }
}

pub extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
                with_app(paint);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_SIZE | WM_SETFOCUS | WM_KILLFOCUS => {
                invalidate(hwnd);
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                crate::ui::focus(hwnd);
                let (x, y) = (x_of(lp), y_of(lp));
                let on_bar = with_app(|a| {
                    let (w, h) = client_size(a.nav);
                    let b = track(a, w, h).filter(|b| b.near(x, y, a.s(4.0)))?;
                    if b.on_thumb(x, y) {
                        a.navs.thumb = Some((y, a.navs.scroll));
                    } else {
                        a.navs.scroll = b.page(x, y, a.navs.scroll);
                    }
                    invalidate(a.nav);
                    Some(())
                })
                .flatten()
                .is_some();
                let pressed = !on_bar
                    && with_app(|a| draggable(a, x, y).map(|i| a.navs.press = Some((x, y, i)))).flatten().is_some();
                if on_bar || pressed {
                    SetCapture(hwnd);
                } else {
                    click(x, y, false);
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let pressed = with_app(|a| {
                    a.navs.thumb = None;
                    a.navs.press.take()
                })
                .flatten();
                let _ = ReleaseCapture();
                // a folder pressed and let go without a drag opens, as a click
                if let Some((px, py, _)) = pressed {
                    click(px, py, false);
                }
                invalidate(hwnd);
                LRESULT(0)
            }
            WM_LBUTTONDBLCLK => {
                click(x_of(lp), y_of(lp), true);
                LRESULT(0)
            }
            WM_MBUTTONUP => {
                let loc = with_app(|a| row_at(a, y_of(lp)).and_then(|i| a.navs.nodes[i].loc.clone())).flatten();
                if let Some(l) = loc {
                    crate::ui::tabs::open(l, false);
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let (x, y) = (x_of(lp), y_of(lp));
                let drag = with_app(|a| {
                    // a pressed folder moved past the drag distance is dragged out
                    if let Some((px, py, i)) = a.navs.press {
                        let (dx, dy) = (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG));
                        if (x - px).abs() > dx || (y - py).abs() > dy {
                            a.navs.press = None;
                            return drag_of(a, i);
                        }
                        return None;
                    }
                    if !a.navs.tracking {
                        let mut tme = TRACKMOUSEEVENT {
                            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: a.nav,
                            dwHoverTime: 0,
                        };
                        let _ = TrackMouseEvent(&mut tme);
                        a.navs.tracking = true;
                    }
                    let (w, ch) = client_size(a.nav);
                    if let Some((from, scroll0)) = a.navs.thumb {
                        if let Some(b) = track(a, w, ch) {
                            a.navs.scroll = b.dragged(scroll0, y - from);
                            invalidate(a.nav);
                        }
                        return None;
                    }
                    let near = track(a, w, ch).is_some_and(|b| b.near(x, y, a.s(10.0)));
                    let h = row_at(a, y);
                    if h != a.navs.hover || near != a.navs.bar_hot {
                        a.navs.hover = h;
                        a.navs.bar_hot = near;
                        invalidate(a.nav);
                    }
                    None
                })
                .flatten();
                if let Some((path, image)) = drag {
                    let _ = ReleaseCapture();
                    crate::ui::drag::start(hwnd, &[path], image);
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                with_app(|a| {
                    a.navs.tracking = false;
                    a.navs.hover = None;
                    a.navs.bar_hot = false;
                    invalidate(a.nav);
                });
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = ((wp.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                with_app(|a| {
                    a.navs.scroll -= delta * 3 * row_h(a) / 120;
                    invalidate(a.nav);
                });
                LRESULT(0)
            }
            WM_VSCROLL => {
                with_app(|a| {
                    let (_, h) = client_size(a.nav);
                    let mut si = SCROLLINFO {
                        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                        fMask: SIF_ALL,
                        ..Default::default()
                    };
                    let _ = GetScrollInfo(a.nav, SB_VERT, &mut si);
                    let rh = row_h(a);
                    a.navs.scroll = match SCROLLBAR_COMMAND((wp.0 & 0xFFFF) as i32) {
                        SB_LINEUP => a.navs.scroll - rh,
                        SB_LINEDOWN => a.navs.scroll + rh,
                        SB_PAGEUP => a.navs.scroll - h,
                        SB_PAGEDOWN => a.navs.scroll + h,
                        SB_THUMBTRACK | SB_THUMBPOSITION => si.nTrackPos,
                        _ => a.navs.scroll,
                    };
                    invalidate(a.nav);
                });
                LRESULT(0)
            }
            WM_CONTEXTMENU => {
                menu(x_of(lp), y_of(lp));
                LRESULT(0)
            }
            WM_DROPFILES => {
                drop_files(HGLOBAL(wp.0 as *mut _));
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
