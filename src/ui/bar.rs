// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! The top of the window (Back / Forward / Up, the breadcrumb address bar, the search box and the command bar) and
//! the status bar at the bottom. Win10's ribbon is replaced by one compact row of commands with their names, so the
//! common actions are a single click without tabs.
use crate::settings::ViewMode;
use crate::ui::art::{Art, Look, Mark, Tool};
use crate::ui::cmds::{self, Cmd};
use crate::ui::gfx::{self, Surface, R};
use crate::ui::model::Loc;
use crate::ui::{invalidate, navigate, with_app, wstr, App};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Controls::EM_SETSEL;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Id {
    Back,
    Forward,
    Recent,
    Up,
    Refresh,
    AddrBlank,
    Crumb(usize),
    CrumbMenu(usize),
    Overflow,
    SearchClear,
    SearchBox,
    New,
    Cut,
    Copy,
    Paste,
    Rename,
    Delete,
    Sort,
    View,
    Props,
    More,
    StatusDetails,
    StatusIcons,
    Tab(usize),
    TabClose(usize),
    NewTab,
}

pub struct Crumb {
    pub label: String,
    pub loc: Loc,
}

#[derive(Default)]
pub struct BarLayout {
    pub top: i32,
    pub status_h: i32,
    pub items: Vec<(Id, R)>,
    pub addr_box: R,
    pub addr_text: R,
    pub search_box: R,
    pub search_text: R,
    pub crumbs: Vec<Crumb>,
    // how many leading crumbs did not fit
    pub hidden: usize,
    pub status: R,
    // the content card: the list and the preview, between the sidebar and the frame's margin
    pub card: R,
}

#[derive(Default)]
pub struct BarState {
    pub layout: Option<BarLayout>,
    pub hot: Option<Id>,
    pub pressed: Option<Id>,
    pub dirty: bool,
    tracking: bool,
    split_drag: Option<(i32, i32)>,
}

fn crumbs_for(a: &App) -> Vec<Crumb> {
    let mut out = vec![Crumb { label: "This PC".into(), loc: Loc::Pc }];
    if let Loc::Dir(d) = &a.model.loc {
        for (label, path) in dvfiles::pathx::segments(d) {
            let label = if out.len() == 1 { a.display_name(&path) } else { label };
            out.push(Crumb { label, loc: Loc::Dir(path) });
        }
    }
    out
}

/// command bar entries: id, line icon, label, has a menu
const COMMANDS: [(Id, Mark, &str, bool); 10] = [
    (Id::New, Mark::Add, "New", true),
    (Id::Cut, Mark::Cut, "Cut", false),
    (Id::Copy, Mark::Copy, "Copy", false),
    (Id::Paste, Mark::Paste, "Paste", false),
    (Id::Rename, Mark::Rename, "Rename", false),
    (Id::Delete, Mark::Delete, "Delete", false),
    (Id::Sort, Mark::Sort, "Sort", true),
    (Id::View, Mark::Grid, "View", true),
    (Id::Props, Mark::Info, "Properties", false),
    (Id::More, Mark::More, "", true),
];

/// the frame's margin around the card, beside it and where the sidebar is hidden
pub fn card_margin(a: &App) -> i32 {
    a.s(8.0)
}

pub fn layout(a: &App, w: i32, h: i32) -> BarLayout {
    let s = |v: f32| a.s(v);
    // the tab strip, when there are tabs, sits above everything else
    let y0 = crate::ui::tabs::strip_h(a);
    let mut l = BarLayout { top: s(94.0) + y0, status_h: s(30.0), ..Default::default() };
    if y0 > 0 {
        let n = a.tabs.list.len() as i32;
        let plus = s(36.0);
        let room = w - s(8.0) - plus - s(8.0);
        let tw = (room / n).clamp(s(80.0), s(240.0));
        let mut x = s(8.0);
        for i in 0..n as usize {
            let r = R::new(x, s(4.0), tw, y0 - s(4.0));
            let close = R::new(r.right() - s(30.0), r.y + (r.h - s(24.0)) / 2, s(24.0), s(24.0));
            l.items.push((Id::TabClose(i), close));
            l.items.push((Id::Tab(i), r));
            x += tw;
        }
        l.items.push((Id::NewTab, R::new(x + s(4.0), s(8.0), s(32.0), y0 - s(12.0))));
    }
    let btn = s(34.0);
    let y = s(8.0) + y0;
    l.items.push((Id::Back, R::new(s(8.0), y, btn, btn)));
    l.items.push((Id::Forward, R::new(s(44.0), y, btn, btn)));
    l.items.push((Id::Recent, R::new(s(80.0), y, s(18.0), btn)));
    l.items.push((Id::Up, R::new(s(100.0), y, btn, btn)));
    let search_w = (w as f32 * 0.24).clamp(s(170.0) as f32, s(340.0) as f32) as i32;
    l.search_box = R::new(w - search_w - s(8.0), y, search_w, btn);
    let clear = R::new(l.search_box.right() - s(32.0), l.search_box.y + s(3.0), s(28.0), l.search_box.h - s(6.0));
    l.items.push((Id::SearchClear, clear));
    l.items.push((Id::SearchBox, l.search_box));
    l.search_text = R::new(l.search_box.x + s(10.0), l.search_box.y, l.search_box.w - s(46.0), l.search_box.h);
    let ax = s(144.0);
    l.addr_box = R::new(ax, y, (l.search_box.x - s(8.0) - ax).max(s(60.0)), btn);
    let refresh = R::new(l.addr_box.right() - s(32.0), l.addr_box.y + s(3.0), s(28.0), l.addr_box.h - s(6.0));
    l.items.push((Id::Refresh, refresh));
    l.addr_text = R::new(l.addr_box.x + s(12.0), l.addr_box.y, l.addr_box.w - s(48.0), l.addr_box.h);

    // breadcrumbs, right-aligned to what fits: the folder itself is always shown
    l.crumbs = crumbs_for(a);
    let icon_w = s(32.0);
    let chevron_w = s(18.0);
    let start = l.addr_box.x + icon_w;
    let room = refresh.x - start;
    let last = l.crumbs.len().saturating_sub(1);
    let widths: Vec<i32> = l
        .crumbs
        .iter()
        .enumerate()
        .map(|(i, c)| gfx::text_width(a.mdc, &c.label, if i == last { a.fonts.bold } else { a.fonts.ui }) + s(18.0))
        .collect();
    let total: i32 = widths.iter().map(|w| w + chevron_w).sum();
    let mut first = l.crumbs.len();
    if total <= room {
        first = 0;
    } else {
        // the « button takes a place of its own
        let mut used = 0;
        while first > 0 && used + widths[first - 1] + chevron_w <= room - chevron_w {
            first -= 1;
            used += widths[first] + chevron_w;
        }
    }
    if first == l.crumbs.len() && !l.crumbs.is_empty() {
        first = l.crumbs.len() - 1;
    }
    l.hidden = first;
    let inner = |x: i32, w: i32| R::new(x, l.addr_box.y + s(4.0), w, l.addr_box.h - s(8.0));
    let mut x = start;
    if first > 0 {
        l.items.push((Id::Overflow, inner(x, chevron_w)));
        x += chevron_w;
    }
    for i in first..l.crumbs.len() {
        let wdt = widths[i].min(refresh.x - x - chevron_w).max(s(20.0));
        l.items.push((Id::Crumb(i), inner(x, wdt)));
        x += wdt;
        l.items.push((Id::CrumbMenu(i), inner(x, chevron_w)));
        x += chevron_w;
    }
    l.items.push((Id::AddrBlank, R::new(x, l.addr_box.y, (refresh.x - x).max(0), l.addr_box.h)));

    // command bar
    let mut x = s(8.0);
    let cy = s(50.0) + y0;
    let ch = s(34.0);
    for (id, _, label, menu) in COMMANDS {
        if matches!(id, Id::Props | Id::More) {
            // pushed to the right end
            continue;
        }
        let tw = gfx::text_width(a.mdc, label, if id == Id::New { a.fonts.bold } else { a.fonts.ui });
        let wdt = s(12.0) + s(18.0) + s(8.0) + tw + if menu { s(32.0) } else { s(14.0) };
        l.items.push((id, R::new(x, cy, wdt, ch)));
        x += wdt + if matches!(id, Id::New | Id::Paste | Id::Delete) { s(17.0) } else { s(2.0) };
    }
    let more = R::new(w - s(8.0) - s(38.0), cy, s(38.0), ch);
    let pw = s(12.0) + s(18.0) + s(8.0) + gfx::text_width(a.mdc, "Properties", a.fonts.ui) + s(12.0);
    let props = R::new((more.x - s(4.0) - pw).max(x), cy, pw, ch);
    l.items.push((Id::Props, props));
    l.items.push((Id::More, more));

    l.status = R::new(0, h - l.status_h, w, l.status_h);
    let margin = card_margin(a);
    let nav_w = if a.settings.nav_width < 40 { margin } else { s(a.settings.nav_width as f32).min(w / 2) };
    l.card = R::new(nav_w, l.top, (w - nav_w - margin).max(0), (l.status.y - l.top).max(0));
    let vb = s(30.0);
    let seg_y = l.status.y + s(4.0);
    let seg_h = l.status_h - s(8.0);
    l.items.push((Id::StatusDetails, R::new(w - s(10.0) - 2 * vb, seg_y, vb, seg_h)));
    l.items.push((Id::StatusIcons, R::new(w - s(10.0) - vb, seg_y, vb, seg_h)));
    l
}

fn enabled(a: &App, id: Id) -> bool {
    let in_dir = a.model.loc != Loc::Pc;
    let has_sel = a.model.sel.count() > 0;
    match id {
        Id::Back => !a.back.is_empty(),
        Id::Forward => !a.fwd.is_empty(),
        Id::Recent => !a.back.is_empty() || !a.fwd.is_empty(),
        Id::Up => a.model.loc != Loc::Pc,
        Id::New => in_dir && a.model.search.is_none(),
        Id::Cut | Id::Copy | Id::Delete => in_dir && has_sel,
        Id::Rename => in_dir && has_sel,
        Id::Paste => {
            in_dir
                && a.model.search.is_none()
                && (crate::ui::clip::has_files() || (a.settings.paste_files && crate::ui::clip::has_pasteable()))
        }
        Id::StatusIcons => a.model.loc != Loc::Pc,
        _ => true,
    }
}

/// Paints the main window: the toolbar (with the tab strip), the status bar and the frame's margin beside the card,
/// the only parts of it that show. Each has a buffer of its own, a band of the window, rather than one buffer the size
/// of the window under the list (which saves about 3 MB on a large window).
pub fn paint(a: &mut App) {
    let (w, h) = crate::ui::client_size(a.main);
    if a.bar.layout.is_none() || a.bar.dirty {
        a.bar.layout = Some(layout(a, w, h));
        a.bar.dirty = false;
    }
    let (top, status, card) = {
        let l = a.bar.layout.as_ref().unwrap();
        (l.top, l.status, l.card)
    };
    for (band_y, band_h, is_status) in [(0, top, false), (status.y, status.h, true)] {
        if is_status {
            std::mem::swap(&mut a.surf_main, &mut a.surf_status);
        }
        if Surface::ensure(&mut a.surf_main, w, band_h.max(1)).is_some() {
            a.surf_main.as_ref().unwrap().set_origin(band_y);
            paint_band(a, w, h, is_status);
            a.surf_main.as_ref().unwrap().present(a.main);
        }
        if is_status {
            std::mem::swap(&mut a.surf_main, &mut a.surf_status);
        }
    }
    // the frame beside the card: on its right, and on its left while the sidebar is hidden
    let left = if a.settings.nav_width < 40 { card.x } else { 0 };
    for e in [R::new(card.right(), top, w - card.right(), status.y - top), R::new(0, top, left, status.y - top)] {
        if e.w <= 0 || e.h <= 0 {
            continue;
        }
        if Surface::ensure(&mut a.surf_edge, e.w, e.h).is_some() {
            let surf = a.surf_edge.as_ref().unwrap();
            surf.set_origin_xy(e.x, e.y);
            crate::ui::backdrop(a, surf, a.main, e);
            surf.present(a.main);
        }
    }
}

/// draws one band: the toolbar (with the tab strip), or the status bar
fn paint_band(a: &mut App, w: i32, h: i32, status: bool) {
    if status {
        paint_status(a, w, h);
    } else {
        paint_toolbar(a, w, h);
        paint_tabs(a);
    }
}

/// the frame behind the toolbar at `r`: the glass, its top rows fading into the title bar's colour so the two read
/// as one pane, or the frame's colour
fn toolbar_under(a: &App, surf: &Surface, r: R) {
    if crate::ui::glass::paint(a, surf, a.main, r) {
        if let Some(c) = a.caption {
            let fade = a.s(10.0);
            for k in r.y.max(0)..fade.min(r.bottom()) {
                surf.blend(R::new(r.x, k, r.w, 1), c, 1.0 - k as f32 / fade as f32);
            }
        }
    } else {
        surf.fill(r, a.theme.chrome);
    }
}

/// a soft rounded veil for something lit on the frame (a hovered or pressed button): lighter on dark, darker on
/// light, so it reads the same over glass and over the plain frame
pub fn lit(surf: &Surface, r: R, rad: f32, dark: bool, strength: f32) {
    let c = if dark { gfx::Rgb::hex(0xFFFFFF) } else { gfx::Rgb(0, 0, 0) };
    surf.round_blend(r, rad, c, if dark { strength } else { strength * 0.75 });
}

/// a text box on the frame: rounded, a hairline edge, and while typing an accent line along its foot as Windows 11
/// has it. `fill` runs over the box's inside before its edge is drawn (the address bar's loading green).
fn text_box(a: &App, surf: &Surface, r: R, hot: bool, focused: bool, fill: &dyn Fn(&Surface, R)) {
    let t = a.theme;
    let rad = a.rad();
    surf.fill(r, t.input_bg);
    fill(surf, r.inset(1, 1));
    let edge = if hot && !focused { t.input_hot } else { t.input_border };
    surf.frame(r, edge);
    if focused {
        let k = a.s(2.0);
        surf.fill(R::new(r.x + rad as i32 / 2, r.bottom() - k, r.w - rad as i32, k), t.accent);
    }
    surf.round_corners(r, rad, 15, Some(edge), &|s, q| toolbar_under(a, s, q));
}

fn paint_toolbar(a: &mut App, w: i32, h: i32) {
    let scale = a.scale;
    let s = |v: f32| (v * scale).round() as i32;
    let rad = a.rad();
    let t = a.theme;
    let f = &a.fonts;
    let surf = a.surf_main.as_ref().unwrap();
    let l = a.bar.layout.as_ref().unwrap();
    toolbar_under(a, surf, R::new(0, 0, w, h));
    let hot = a.bar.hot;
    let pressed = a.bar.pressed;
    let light = |r: R, id: Id| lit(surf, r, rad, t.dark, if pressed == Some(id) { 0.13 } else { 0.08 });
    let item = |id: Id| l.items.iter().find(|(i, _)| *i == id).map(|(_, r)| *r).unwrap_or_default();

    // Back, Forward, recent, Up: thin arrows in the text's colour
    for (id, m) in [(Id::Back, Mark::Back), (Id::Forward, Mark::Forward), (Id::Up, Mark::Rise)] {
        let r = item(id);
        let on = enabled(a, id);
        if on && (hot == Some(id) || pressed == Some(id)) {
            light(r, id);
        }
        surf.image_in(r, &a.art.mark(m, s(16.0), if on { t.fg } else { t.faint }));
    }
    let r = item(Id::Recent);
    if enabled(a, Id::Recent) && hot == Some(Id::Recent) {
        light(r, Id::Recent);
    }
    surf.image_in(r, &a.art.mark(Mark::Down, s(12.0), if enabled(a, Id::Recent) { t.muted } else { t.faint }));

    // the address bar: the folder's icon and its path as breadcrumbs; green fills it from the left while loading
    let editing = a.addr_editing;
    let addr_hot = matches!(hot, Some(Id::AddrBlank | Id::Crumb(_) | Id::CrumbMenu(_) | Id::Overflow | Id::Refresh));
    let busy = if editing { None } else { crate::ui::busy_fill(a) };
    text_box(a, surf, l.addr_box, addr_hot, editing, &|surf, inner| {
        if let Some((done, strength)) = busy {
            surf.fill_frac(inner, inner.w as f32 * done, t.input_bg.mix(t.progress_fill, strength));
        }
    });
    let crumb_hot = t.input_bg.mix(t.fg, if t.dark { 0.09 } else { 0.06 });
    if !editing {
        let icon_r = R::new(l.addr_box.x + s(8.0), l.addr_box.y, s(20.0), l.addr_box.h);
        let current = match &a.model.loc {
            Loc::Pc => None,
            Loc::Dir(d) => Some(d.clone()),
        };
        let px = a.icons.small_px;
        let img = a.art.get(loc_art(a, current.as_deref()), px, &t);
        let surf = a.surf_main.as_ref().unwrap();
        let l = a.bar.layout.as_ref().unwrap();
        surf.image(icon_r.x + (icon_r.w - px) / 2, icon_r.y + (icon_r.h - px) / 2, img.w, img.h, &img.px);
        let last = l.crumbs.len().saturating_sub(1);
        for (id, r) in &l.items {
            match id {
                Id::Overflow => {
                    if hot == Some(*id) {
                        surf.round(*r, rad - 1.0, crumb_hot);
                    }
                    surf.text(*r, "«", f.ui, t.muted, gfx::CENTER);
                }
                Id::Crumb(i) => {
                    if hot == Some(*id) || hot == Some(Id::CrumbMenu(*i)) {
                        surf.round(*r, rad - 1.0, crumb_hot);
                    }
                    let (font, color) = if *i == last { (f.bold, t.fg) } else { (f.ui, t.muted) };
                    surf.text(r.inset(s(7.0), 0), &l.crumbs[*i].label, font, color, gfx::LEFT);
                }
                Id::CrumbMenu(_) => {
                    if hot == Some(*id) {
                        surf.round(*r, rad - 1.0, crumb_hot);
                    }
                    surf.image_in(*r, &a.art.mark(Mark::Right, s(12.0), t.faint));
                }
                _ => {}
            }
        }
    }
    let surf = a.surf_main.as_ref().unwrap();
    let l = a.bar.layout.as_ref().unwrap();
    let r = item(Id::Refresh);
    if hot == Some(Id::Refresh) {
        surf.round(r, rad - 1.0, crumb_hot);
    }
    surf.image_in(r, &a.art.mark(Mark::Refresh, s(14.0), if hot == Some(Id::Refresh) { t.fg } else { t.muted }));

    // the search box: magnifier on the right, a clear mark while it holds text
    // (before the box exists, and while the window is in the background, GetFocus and the box are both null)
    let search_focused =
        !a.search.0.is_null() && unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() } == a.search;
    let has_text = !a.search.0.is_null() && unsafe { GetWindowTextLengthW(a.search) } > 0;
    text_box(a, surf, l.search_box, matches!(hot, Some(Id::SearchClear | Id::SearchBox)), search_focused, &|_, _| {});
    if a.search.0.is_null() {
        // until the box is first used, its placeholder is drawn here
        surf.text(l.search_text, "Search", f.ui, t.faint, gfx::LEFT);
    }
    let r = item(Id::SearchClear);
    if !has_text {
        surf.image_in(
            r,
            &a.art.look(Art::Tool(Tool::Search), s(16.0), if search_focused { Look::Hot } else { Look::Normal }, &t),
        );
    } else {
        if hot == Some(Id::SearchClear) {
            surf.round(r, rad - 1.0, crumb_hot);
        }
        surf.image_in(r, &a.art.mark(Mark::Close, s(12.0), t.muted));
    }

    // the command bar: New is the one filled button, in the accent; the rest are quiet until pointed at
    for (id, m, label, menu) in COMMANDS {
        let r = item(id);
        let on = enabled(a, id);
        let primary = id == Id::New;
        if primary {
            if on {
                surf.round(r, rad, if hot == Some(id) || pressed == Some(id) { t.primary_hot } else { t.primary });
            } else {
                lit(surf, r, rad, t.dark, 0.06);
            }
        } else if on && (hot == Some(id) || pressed == Some(id)) {
            light(r, id);
        }
        let fg = if primary && on {
            t.primary_fg
        } else if on {
            t.fg
        } else {
            t.faint
        };
        if id == Id::More {
            surf.image_in(r, &a.art.mark(Mark::More, s(16.0), fg));
            continue;
        }
        let ir = R::new(r.x + s(12.0), r.y, s(18.0), r.h);
        surf.image_in(ir, &a.art.mark(m, s(16.0), fg));
        let tx = ir.right() + s(8.0);
        let tr = R::new(tx, r.y, r.right() - tx - if menu { s(24.0) } else { s(4.0) }, r.h);
        surf.text(tr, label, if primary { f.bold } else { f.ui }, fg, gfx::LEFT);
        if menu {
            let mc = if primary && on {
                t.primary_fg
            } else if on {
                t.muted
            } else {
                t.faint
            };
            surf.image_in(R::new(r.right() - s(24.0), r.y, s(14.0), r.h), &a.art.mark(Mark::Down, s(12.0), mc));
        }
    }
    // short dividers between the groups
    for id in [Id::New, Id::Paste, Id::Delete] {
        let r = item(id);
        surf.blend(R::new(r.right() + s(8.0), r.y + s(10.0), 1, r.h - s(20.0)), t.fg, 0.16);
    }
}

fn paint_status(a: &mut App, w: i32, h: i32) {
    let scale = a.scale;
    let s = |v: f32| (v * scale).round() as i32;
    let rad = a.rad();
    let t = a.theme;
    let f = &a.fonts;
    let surf = a.surf_main.as_ref().unwrap();
    let l = a.bar.layout.as_ref().unwrap();
    crate::ui::backdrop(a, surf, a.main, R::new(0, 0, w, h));
    let hot = a.bar.hot;
    let item = |id: Id| l.items.iter().find(|(i, _)| *i == id).map(|(_, r)| *r).unwrap_or_default();

    let text = status_text(a);
    let surf = a.surf_main.as_ref().unwrap();
    let l = a.bar.layout.as_ref().unwrap();
    // the shown drive's free space on the right, before the view switch
    let space = match &a.model.loc {
        Loc::Dir(d) if d.as_bytes().get(1) == Some(&b':') => a
            .drive(&format!("{}\\", &d[..2]))
            .filter(|dr| dr.total > 0)
            .map(|dr| format!("{} free of {}", a.nf.bytes(dr.free), a.nf.bytes(dr.total))),
        _ => None,
    };
    let seg_x = item(Id::StatusDetails).x;
    let mut text_right = seg_x - s(12.0);
    if let Some(sp) = space {
        let sw = gfx::text_width(a.mdc, &sp, f.ui);
        surf.text(R::new(text_right - sw, l.status.y, sw, l.status.h), &sp, f.ui, t.muted, gfx::RIGHT);
        text_right -= sw + s(16.0);
    }
    surf.text(R::new(s(14.0), l.status.y, (text_right - s(14.0)).max(0), l.status.h), &text, f.ui, t.muted, gfx::LEFT);
    // the view switch: a two-part segmented control, the chosen part a small raised card
    let view = a.view();
    let seg = {
        let (d, i) = (item(Id::StatusDetails), item(Id::StatusIcons));
        R::new(d.x, d.y, i.right() - d.x, d.h)
    };
    lit(surf, seg, rad, t.dark, 0.06);
    for (id, m, active) in [
        (Id::StatusDetails, Mark::List, view == ViewMode::Details),
        (Id::StatusIcons, Mark::Grid, view == ViewMode::Icons),
    ] {
        let r = item(id);
        let inner = r.inset(s(2.0), s(2.0));
        if active {
            surf.round(inner, rad - 2.0, t.bg);
            surf.round_ring(inner, rad - 2.0, t.card_border);
        } else if hot == Some(id) && enabled(a, id) {
            lit(surf, inner, rad - 2.0, t.dark, 0.08);
        }
        let c = if !enabled(a, id) {
            t.faint
        } else if active {
            t.accent
        } else {
            t.muted
        };
        surf.image_in(r, &a.art.mark(m, s(16.0), c));
    }
}

/// the tab strip: the shown tab is part of the toolbar below it, with rounded top corners as a browser's; the others
/// sit back on a darker band
fn paint_tabs(a: &mut App) {
    let y0 = crate::ui::tabs::strip_h(a);
    if y0 == 0 {
        return;
    }
    let (w, _) = crate::ui::client_size(a.main);
    let t = a.theme;
    let sc = a.scale;
    let s = |v: f32| (v * sc).round() as i32;
    let rad = a.rad_card();
    let shade = if t.dark { 0.28 } else { 0.06 };
    let titles: Vec<(String, Option<String>)> = a
        .tabs
        .list
        .iter()
        .enumerate()
        .map(|(i, tab)| {
            let title = if i == a.tabs.active { a.title_text() } else { tab.title.clone() };
            (title, tab.loc.path().map(String::from))
        })
        .collect();
    let px = a.icons.small_px;
    let icons: Vec<_> = titles
        .iter()
        .map(|(_, p)| loc_art(a, p.as_deref()))
        .collect::<Vec<_>>()
        .into_iter()
        .map(|art| a.art.get(art, px, &t))
        .collect();
    let close = a.art.mark(Mark::Close, s(12.0), t.muted);
    let surf = a.surf_main.as_ref().unwrap();
    let l = a.bar.layout.as_ref().unwrap();
    let hot = a.bar.hot;
    // the strip: the toolbar's frame a shade darker
    let strip = |surf: &Surface, r: R| {
        toolbar_under(a, surf, r);
        surf.blend(r, gfx::Rgb(0, 0, 0), shade);
    };
    strip(surf, R::new(0, 0, w, y0));
    let item = |id: Id| l.items.iter().find(|(i, _)| *i == id).map(|(_, r)| *r);
    for (i, (title, path)) in titles.iter().enumerate() {
        let Some(r) = item(Id::Tab(i)) else { continue };
        let active = i == a.tabs.active;
        let over = hot == Some(Id::Tab(i)) || hot == Some(Id::TabClose(i));
        if active {
            toolbar_under(a, surf, r);
            surf.round_corners(R::new(r.x, r.y, r.w, r.h + rad as i32 + 2), rad, 1 | 2, None, &|s, q| strip(s, q));
        } else {
            if over {
                lit(surf, r.inset(s(2.0), s(3.0)), a.rad(), t.dark, 0.07);
            }
            // a short divider between tabs at rest
            let next_active = i + 1 == a.tabs.active;
            if !next_active && !over {
                surf.blend(R::new(r.right() - 1, r.y + s(9.0), 1, r.h - s(18.0)), t.fg, 0.14);
            }
        }
        let ir = R::new(r.x + s(10.0), r.y, s(20.0), r.h);
        let _ = path;
        let img = &icons[i];
        surf.image(ir.x + (ir.w - img.w) / 2, ir.y + (ir.h - img.h) / 2, img.w, img.h, &img.px);
        let show_close = active || over;
        let tx = ir.right() + s(8.0);
        let tr = R::new(tx, r.y, r.right() - tx - if show_close { s(34.0) } else { s(10.0) }, r.h);
        surf.text(tr, title, a.fonts.ui, if active { t.fg } else { t.muted }, gfx::LEFT);
        if show_close {
            if let Some(c) = item(Id::TabClose(i)) {
                if hot == Some(Id::TabClose(i)) {
                    lit(surf, c, a.rad(), t.dark, 0.10);
                }
                surf.image_in(c, &close);
            }
        }
    }
    if let Some(r) = item(Id::NewTab) {
        if hot == Some(Id::NewTab) {
            lit(surf, r, a.rad(), t.dark, 0.08);
        }
        surf.image_in(r, &a.art.mark(Mark::Add, s(12.0), t.muted));
    }
}

/// the icon for a place: This PC, a drive, one of the user's folders, or a folder
fn loc_art(a: &App, path: Option<&str>) -> crate::ui::art::Art {
    use crate::ui::art::{special_for, Art};
    match path {
        None => Art::Pc,
        Some(p) if p.len() <= 3 => Art::Drive,
        Some(p) => a
            .known
            .iter()
            .find(|(_, k)| dvfiles::pathx::same(k, p))
            .and_then(|(l, _)| special_for(l))
            .map_or(Art::Folder, Art::Special),
    }
}

/// a middle click on a tab closes it
pub fn middle_click(x: i32, y: i32) {
    if let Some(Some(Id::Tab(i) | Id::TabClose(i))) = with_app(|a| hit(a, x, y)) {
        crate::ui::tabs::close(i);
    }
}

fn status_text(a: &App) -> String {
    let nf = &a.nf;
    let m = &a.model;
    let n = m.order.len() as u64;
    if let Some(s) = &m.search {
        let dirs = s.stats.dirs.load(std::sync::atomic::Ordering::Relaxed);
        let ents = s.stats.entries.load(std::sync::atomic::Ordering::Relaxed);
        let base = if s.running {
            format!("Searching...   {} found in {} items", nf.group(n), nf.group(ents))
        } else if s.capped {
            format!("The first {} matches    type more to narrow the search", nf.group(n))
        } else {
            format!(
                "{} found    searched {} items in {} folders in {:.2} s{}",
                dvfiles::fmt::items(n, nf),
                nf.group(ents),
                nf.group(dirs),
                s.elapsed.as_secs_f64(),
                if s.stats.indexed.load(std::sync::atomic::Ordering::Relaxed) { " (index)" } else { "" }
            )
        };
        return base + &selection_text(a);
    }
    if m.loading && m.entries.is_empty() {
        return "Working on it...".into();
    }
    let sizing = if m.sizes_pending > 0 {
        format!("        measuring folder sizes ({} left)", nf.group(m.sizes_pending as u64))
    } else {
        String::new()
    };
    let fresh = a.new_since.map_or(0, |t| m.entries.iter().filter(|e| e.created.max(e.modified) > t).count());
    let sizing = if fresh > 0 { format!("{sizing}        {fresh} new since your last look") } else { sizing };
    let shelf = if a.shelf.is_empty() {
        String::new()
    } else {
        format!("        shelf: {}", dvfiles::fmt::items(a.shelf.len() as u64, nf))
    };
    format!("{}{}{}{}", dvfiles::fmt::items(n, nf), selection_text(a), sizing, shelf)
}

fn selection_text(a: &App) -> String {
    let m = &a.model;
    let k = m.sel.count() as u64;
    if k == 0 {
        return String::new();
    }
    let picked = m.selected();
    let all_files = picked.iter().all(|&i| !m.entries[i as usize].is_dir());
    let size = if all_files && m.loc != Loc::Pc {
        let total: u64 = picked.iter().map(|&i| m.entries[i as usize].size).sum();
        format!("    {}", a.nf.bytes(total))
    } else {
        String::new()
    };
    let what = if k == 1 { "1 item selected".to_string() } else { format!("{} items selected", a.nf.group(k)) };
    format!("        {what}{size}")
}

fn hit(a: &App, x: i32, y: i32) -> Option<Id> {
    let l = a.bar.layout.as_ref()?;
    l.items.iter().find(|(_, r)| r.contains(x, y)).map(|(id, _)| *id)
}

pub fn over_splitter(a: &App, x: i32, y: i32) -> bool {
    let Some(l) = a.bar.layout.as_ref() else { return false };
    let nav_w = if a.settings.nav_width < 40 { 0 } else { a.s(a.settings.nav_width as f32) };
    y >= l.top && y < l.status.y && x >= nav_w - 2 && x < nav_w + a.s(6.0)
}

pub fn mouse_move(x: i32, y: i32) {
    let resize = with_app(|a| {
        if let Some((start_x, start_w)) = a.bar.split_drag {
            let w = ((start_w as f32 * a.scale) as i32 + x - start_x) as f32 / a.scale;
            a.settings.nav_width = if w < 60.0 { 0 } else { (w as i32).clamp(120, 800) };
            return true;
        }
        if !a.bar.tracking {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: a.main,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
            a.bar.tracking = true;
        }
        let h = hit(a, x, y);
        if h != a.bar.hot {
            a.bar.hot = h;
            invalidate(a.main);
        }
        false
    });
    if resize == Some(true) {
        crate::ui::layout();
    }
}

pub fn mouse_leave() {
    with_app(|a| {
        a.bar.tracking = false;
        if a.bar.hot.is_some() {
            a.bar.hot = None;
            invalidate(a.main);
        }
    });
}

pub fn mouse_down(x: i32, y: i32) {
    crate::trace::log(|| format!("bar down {x},{y}"));
    let r = with_app(|a| {
        if over_splitter(a, x, y) {
            a.bar.split_drag = Some((x, if a.settings.nav_width < 40 { 0 } else { a.settings.nav_width }));
            return (a.main, true, None);
        }
        let h = hit(a, x, y);
        crate::trace::log(|| format!("  hit {h:?}"));
        a.bar.pressed = h.filter(|&id| enabled(a, id));
        invalidate(a.main);
        (a.main, a.bar.pressed.is_some(), a.bar.pressed)
    });
    let Some((main, capture, pressed)) = r else { return };
    if capture {
        unsafe {
            SetCapture(main);
        }
    }
    // these act on press, like menus do
    match pressed {
        Some(Id::AddrBlank) => begin_address_edit(),
        Some(Id::SearchBox) => {
            unsafe {
                let _ = ReleaseCapture();
            }
            cmds::run(Cmd::FocusSearch);
        }
        Some(
            id @ (Id::Tab(_) | Id::Recent | Id::CrumbMenu(_) | Id::Overflow | Id::New | Id::Sort | Id::View | Id::More),
        ) => {
            unsafe {
                let _ = ReleaseCapture();
            }
            act(id);
            with_app(|a| {
                a.bar.pressed = None;
                invalidate(a.main);
            });
        }
        _ => {}
    }
}

pub fn mouse_up(x: i32, y: i32) {
    crate::trace::log(|| format!("bar up {x},{y}"));
    let r = with_app(|a| {
        if a.bar.split_drag.take().is_some() {
            return None;
        }
        let p = a.bar.pressed.take();
        invalidate(a.main);
        let h = hit(a, x, y);
        p.filter(|&id| Some(id) == h)
    });
    unsafe {
        let _ = ReleaseCapture();
    }
    if let Some(Some(id)) = r {
        act(id);
    }
}

fn menu_pos(id: Id) -> (i32, i32) {
    with_app(|a| {
        let r = a
            .bar
            .layout
            .as_ref()
            .and_then(|l| l.items.iter().find(|(i, _)| *i == id).map(|(_, r)| *r))
            .unwrap_or_default();
        let mut p = windows::Win32::Foundation::POINT { x: r.x, y: r.bottom() };
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::ClientToScreen(a.main, &mut p);
        }
        (p.x, p.y)
    })
    .unwrap_or((0, 0))
}

fn act(id: Id) {
    crate::trace::log(|| format!("act {id:?}"));
    match id {
        Id::Back => cmds::run(Cmd::Back),
        Id::Forward => cmds::run(Cmd::Forward),
        Id::Up => cmds::run(Cmd::Up),
        Id::Refresh => cmds::run(Cmd::Refresh),
        Id::Crumb(i) => {
            if let Some(loc) = with_app(|a| a.bar.layout.as_ref().map(|l| l.crumbs[i].loc.clone())).flatten() {
                navigate(loc, true);
            }
        }
        Id::CrumbMenu(i) => {
            let (x, y) = menu_pos(id);
            if let Some(loc) = with_app(|a| a.bar.layout.as_ref().map(|l| l.crumbs[i].loc.clone())).flatten() {
                cmds::subfolder_menu(&loc, x, y);
            }
        }
        Id::Overflow => {
            let (x, y) = menu_pos(id);
            let hidden: Vec<(String, Loc)> = with_app(|a| {
                a.bar
                    .layout
                    .as_ref()
                    .map(|l| l.crumbs[..l.hidden].iter().rev().map(|c| (c.label.clone(), c.loc.clone())).collect())
            })
            .flatten()
            .unwrap_or_default();
            cmds::location_menu(&hidden, x, y);
        }
        Id::Recent => {
            let (x, y) = menu_pos(id);
            cmds::recent_menu(x, y);
        }
        Id::SearchClear => {
            crate::ui::set_search_text("");
            crate::ui::start_search(String::new());
        }
        Id::New => {
            let (x, y) = menu_pos(id);
            cmds::new_menu(x, y);
        }
        Id::Cut => cmds::run(Cmd::Cut),
        Id::Copy => cmds::run(Cmd::Copy),
        Id::Paste => cmds::run(Cmd::Paste),
        Id::Rename => cmds::run(Cmd::Rename),
        Id::Delete => cmds::run(Cmd::Delete),
        Id::Sort => {
            let (x, y) = menu_pos(id);
            cmds::sort_menu(x, y);
        }
        Id::View => {
            let (x, y) = menu_pos(id);
            cmds::view_menu(x, y);
        }
        Id::More => {
            let (x, y) = menu_pos(id);
            cmds::more_menu(x, y);
        }
        Id::Props => cmds::run(Cmd::Properties),
        Id::StatusDetails => cmds::run(Cmd::View(ViewMode::Details)),
        Id::StatusIcons => cmds::run(Cmd::View(ViewMode::Icons)),
        Id::Tab(i) => crate::ui::tabs::switch(i),
        Id::TabClose(i) => crate::ui::tabs::close(i),
        Id::NewTab => cmds::run(Cmd::NewTab),
        Id::AddrBlank | Id::SearchBox => {}
    }
}

pub fn begin_address_edit() {
    crate::ui::ensure_edits();
    let r = with_app(|a| {
        a.addr_editing = true;
        let text = match &a.model.loc {
            Loc::Pc => "This PC".to_string(),
            Loc::Dir(d) => d.clone(),
        };
        invalidate(a.main);
        (a.addr, text)
    });
    let Some((addr, text)) = r else { return };
    unsafe {
        let _ = SetWindowTextW(addr, PCWSTR(wstr(&text).as_ptr()));
        let _ = ShowWindow(addr, SW_SHOW);
        crate::ui::focus(addr);
        SendMessageW(addr, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(-1)));
    }
}

pub fn end_address_edit() {
    let r = with_app(|a| {
        if !a.addr_editing {
            return None;
        }
        a.addr_editing = false;
        invalidate(a.main);
        Some(a.addr)
    })
    .flatten();
    if let Some(addr) = r {
        unsafe {
            let _ = ShowWindow(addr, SW_HIDE);
        }
    }
}

pub fn cancel_address() {
    end_address_edit();
    cmds::focus_list();
}

pub fn commit_address() {
    let Some((addr, cwd, main)) = with_app(|a| (a.addr, a.model.loc.path().map(String::from), a.main)) else { return };
    let text = crate::ui::window_text(addr);
    end_address_edit();
    cmds::focus_list();
    let typed = text.trim();
    if typed.is_empty() {
        return;
    }
    if typed.eq_ignore_ascii_case("this pc") || typed.eq_ignore_ascii_case("computer") {
        navigate(Loc::Pc, true);
        return;
    }
    let path = dvfiles::pathx::address_input(typed, cwd.as_deref(), crate::ui::shell::env);
    if dvfiles::fsx::is_dir(&path) {
        navigate(Loc::Dir(path), true);
    } else if dvfiles::fsx::exists(&path) {
        let dir = dvfiles::pathx::parent(&path).unwrap_or_default();
        crate::ui::shell::open(main, &path, &dir);
    } else if let Some(dir) =
        with_app(|a| a.visits.find(typed, dvfiles::visits::now()).into_iter().find(|d| dvfiles::fsx::is_dir(d)))
            .flatten()
    {
        // a few letters of a folder gone to before: the best of them
        navigate(Loc::Dir(dir), true);
    } else {
        // like Explorer's address bar, anything else is handed to Windows to run (a program, a URL, shell:...)
        let w = wstr(typed);
        let r = unsafe {
            windows::Win32::UI::Shell::ShellExecuteW(
                Some(HWND(main.0)),
                PCWSTR::null(),
                PCWSTR(w.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        if r.0 as isize <= 32 {
            crate::ui::shell::message(
                main,
                &format!("Windows can't find '{typed}'. Check the spelling and try again."),
                true,
            );
        }
    }
}
