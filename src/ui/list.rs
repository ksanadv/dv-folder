// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! The file list: Details (columns) and Large icons. Only the rows on screen are drawn, so a folder of 100,000 items
//! paints as fast as one of ten.
use crate::settings::{Col, ViewMode};
use crate::ui::art::{self, Art};
use crate::ui::cmds::{self, Cmd};
use crate::ui::gfx::{self, Surface, R};
use crate::ui::model::Loc;
use crate::ui::scroll::{self, Track};
use crate::ui::{client_size, invalidate, key_down, locale, with_app, wstr, x_of, y_of, App};
use dvfiles::fsx::Entry;
use dvfiles::selection::Selection;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, ScreenToClient, SetBkColor, SetTextColor, HDC, PAINTSTRUCT};
use windows::Win32::UI::Controls::{EM_SETSEL, WM_MOUSELEAVE};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const T_AUTOSCROLL: usize = 1;
// the wheel's short glide; the timer runs only while the list is moving
const T_GLIDE: usize = 2;

#[derive(Default, Clone)]
pub enum Drag {
    #[default]
    None,
    // pressed on an item; `defer` = it was already selected, so a click (not a drag) selects only it on release
    Pending {
        x: i32,
        y: i32,
        pos: usize,
        defer: bool,
    },
    // rubber band in content coordinates
    Band {
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        base: Option<Selection>,
    },
    Resize {
        col: Col,
        start_x: i32,
        start_w: i32,
    },
    Header {
        index: usize,
    },
    // dragging a scrollbar thumb: where the pointer started and the scroll position then
    Thumb {
        vertical: bool,
        from: i32,
        scroll0: i32,
    },
}

#[derive(Default)]
pub struct ListState {
    pub scroll_y: i32,
    pub scroll_x: i32,
    pub hover: Option<usize>,
    pub header_hot: Option<usize>,
    pub drag: Drag,
    pub rename_edit: Option<HWND>,
    // what is being renamed, by path: the listing may be reloaded while the box is open
    rename_target: Option<(String, String, bool)>,
    // the rename box's frame, drawn by the list around the text box
    rename_box: Option<R>,
    // a reload was asked for while renaming; it runs when the box closes
    pub refresh_after_rename: bool,
    typeahead: String,
    typeahead_at: u32,
    tracking: bool,
    // the pointer is near a scrollbar (true: the vertical one), which then widens
    pub bar_hot: Option<bool>,
    // where a wheel glide is heading
    pub glide: Option<i32>,
    // the folder lit up as the target of a drag over the list
    pub drop_hot: Option<usize>,
    // where to scroll once the next listing arrives (a tab brought back)
    pub restore_scroll: Option<i32>,
}

pub struct Geo {
    pub view: ViewMode,
    pub w: i32,
    pub h: i32,
    pub header_h: i32,
    pub row_h: i32,
    pub pad: i32,
    pub cols: Vec<(Col, i32, i32)>,
    pub total_w: i32,
    pub cell_w: i32,
    pub cell_h: i32,
    pub per_row: usize,
    pub content_h: i32,
    pub view_h: i32,
}

pub fn columns(a: &App) -> Vec<Col> {
    match (&a.model.loc, &a.model.search) {
        (Loc::Pc, _) => vec![Col::Name, Col::Type, Col::Total, Col::Free],
        (_, Some(_)) => vec![Col::Name, Col::Modified, Col::Type, Col::Size, Col::Folder],
        _ => vec![Col::Name, Col::Modified, Col::Type, Col::Size],
    }
}

pub fn geo(a: &App) -> Geo {
    let (w, h) = client_size(a.list);
    let view = a.view();
    let n = a.model.order.len();
    let pad = a.s(6.0);
    let mut g = Geo {
        view,
        w,
        h,
        header_h: 0,
        row_h: a.s(28.0).max(a.icons.small_px + a.s(10.0)),
        pad,
        cols: Vec::new(),
        total_w: 0,
        cell_w: 0,
        cell_h: 0,
        per_row: 1,
        content_h: 0,
        view_h: h,
    };
    match view {
        ViewMode::Details => {
            g.header_h = a.s(34.0);
            let mut x = pad;
            for c in columns(a) {
                let cw = a.s(a.settings.width(c) as f32);
                g.cols.push((c, x, cw));
                x += cw;
            }
            g.total_w = x + pad;
            g.content_h = n as i32 * g.row_h + a.s(10.0);
            g.view_h = (h - g.header_h).max(0);
        }
        ViewMode::Icons => {
            let icon = picture_box(a);
            g.cell_w = (icon + a.s(24.0)).max(a.s(112.0));
            g.cell_h = icon + a.s(52.0);
            g.per_row = ((w - 2 * pad) / g.cell_w).max(1) as usize;
            let rows = n.div_ceil(g.per_row) as i32;
            g.content_h = rows * g.cell_h + 2 * pad;
            g.total_w = w;
        }
    }
    g
}

impl Geo {
    /// an item's rectangle in window coordinates
    pub fn item_rect(&self, pos: usize, scroll_y: i32, scroll_x: i32) -> R {
        match self.view {
            // rows span the whole card, however narrow the columns are
            ViewMode::Details => R::new(
                self.pad - scroll_x,
                self.header_h + pos as i32 * self.row_h - scroll_y,
                self.total_w.max(self.w) - 2 * self.pad,
                self.row_h,
            ),
            ViewMode::Icons => {
                let (row, col) = ((pos / self.per_row) as i32, (pos % self.per_row) as i32);
                R::new(
                    self.pad + col * self.cell_w,
                    self.pad + row * self.cell_h - scroll_y,
                    self.cell_w - self.pad / 2,
                    self.cell_h - self.pad / 2,
                )
            }
        }
    }

    /// the item under a point (window coordinates)
    pub fn hit(&self, x: i32, y: i32, scroll_y: i32, scroll_x: i32, n: usize) -> Option<usize> {
        match self.view {
            ViewMode::Details => {
                if y < self.header_h || x + scroll_x >= self.total_w.max(self.w) - self.pad {
                    return None;
                }
                let pos = ((y - self.header_h + scroll_y) / self.row_h) as usize;
                (pos < n).then_some(pos)
            }
            ViewMode::Icons => {
                let cx = x - self.pad;
                let cy = y - self.pad + scroll_y;
                if cx < 0 || cy < 0 {
                    return None;
                }
                let col = (cx / self.cell_w) as usize;
                if col >= self.per_row {
                    return None;
                }
                let pos = (cy / self.cell_h) as usize * self.per_row + col;
                let r = self.item_rect(pos, scroll_y, scroll_x);
                (pos < n && r.contains(x, y)).then_some(pos)
            }
        }
    }

    /// the items a band (content coordinates) touches
    pub fn band_hits(&self, x0: i32, y0: i32, x1: i32, y1: i32, n: usize) -> Vec<usize> {
        let band = R::new(x0.min(x1), y0.min(y1), (x1 - x0).abs().max(1), (y1 - y0).abs().max(1));
        match self.view {
            ViewMode::Details => {
                if band.x > self.total_w.max(self.w) {
                    return Vec::new();
                }
                let a = ((band.y - self.header_h).max(0) / self.row_h) as usize;
                let b = ((band.bottom() - self.header_h).max(0) / self.row_h) as usize;
                (a..=b.min(n.saturating_sub(1))).filter(|&p| p < n && band.bottom() > self.header_h).collect()
            }
            ViewMode::Icons => (0..n).filter(|&p| self.item_rect(p, 0, 0).intersects(&band)).collect(),
        }
    }
}

/// the square each item's picture (thumbnail or large icon) gets in Large icons
pub fn picture_box(a: &App) -> i32 {
    a.s(96.0)
}

/// dvfiles' own icon for an item (see `art`), or None for programs and shortcuts, whose icon is their own
pub fn art_for(a: &App, e: &Entry, path: &str) -> Option<Art> {
    if a.model.loc == Loc::Pc && e.dir.is_none() {
        let removable = a.drive(&e.name).is_some_and(|d| d.kind == dvfiles::fsx::DriveKind::Removable);
        return Some(if removable { Art::Removable } else { Art::Drive });
    }
    if e.is_dir() {
        let special =
            a.known.iter().find(|(_, p)| dvfiles::pathx::same(p, path)).and_then(|(l, _)| art::special_for(l));
        return Some(special.map_or(Art::Folder, Art::Special));
    }
    let ext = dvfiles::naming::ext_lower(&e.name);
    (!crate::ui::icons::per_file(&ext)).then(|| Art::File(art::kind_of(&ext)))
}

/// The icon a file type's program gives it, at `size`. A program with no big picture of it (a browser's 32 px PDF
/// icon) keeps dvfiles' own icon at sizes it would only show as a speck or blurred.
fn program_icon(a: &mut App, ext: &str, size: i32) -> Option<std::rc::Rc<crate::ui::thumbs::Image>> {
    if !a.settings.app_icons {
        return None;
    }
    a.icons.of_type(ext, size).filter(|img| img.w * 3 >= size * 2)
}

/// a big picture of an item, the label for its icon's band ("PDF") when it is dvfiles' own icon, and whether it is
/// a shortcut (which gets the arrow)
pub struct Pic {
    pub img: std::rc::Rc<crate::ui::thumbs::Image>,
    pub label: Option<String>,
    pub arrow: bool,
}

/// An item's icon at `size`: dvfiles' own, or (programs, shortcuts, icon files) the one the file carries, read by
/// `icons`, with dvfiles' program page meanwhile. Also whether it is a shortcut.
pub fn icon_image(a: &mut App, e: &Entry, path: &str, size: i32) -> (std::rc::Rc<crate::ui::thumbs::Image>, bool) {
    use crate::ui::icons::Own;
    let arrow = !e.is_dir() && dvfiles::naming::ext_lower(&e.name) == "lnk";
    let art = match art_for(a, e, path) {
        // a type whose program gives it an icon shows that one, as in Explorer
        Some(Art::File(kind)) => match program_icon(a, &dvfiles::naming::ext_lower(&e.name), size) {
            Some(img) => return (img, arrow),
            None => Art::File(kind),
        },
        Some(art) => art,
        None => match a.icons.own(path, size) {
            Own::Image(img) => return (img, arrow),
            Own::Art(art) => art,
            Own::Pending => Art::File(art::Kind::App),
        },
    };
    (a.art.get(art, size, &a.theme), arrow)
}

/// the size of a shortcut's arrow on an icon of `size`
fn arrow_size(size: i32) -> i32 {
    // about a third of a big icon, as Windows' own overlay; never so small it cannot be told
    (size as f32 * if size >= 40 { 0.34 } else { 0.45 }).round().max(9.0) as i32
}

/// a shortcut's arrow over the lower left corner of an icon of `size` at (x, y)
pub fn draw_arrow(a: &mut App, which: fn(&App) -> Option<&Surface>, x: i32, y: i32, size: i32) {
    let s = arrow_size(size);
    let img = a.art.get(Art::Arrow, s, &a.theme);
    if let Some(surf) = which(a) {
        surf.image(x, y + size - s, img.w, img.h, &img.px);
    }
}

/// What Large icons and the preview pane show for an item: a photo's or video's thumbnail, a file's own icon, or
/// dvfiles' icon (also while a thumbnail is on its way).
pub fn picture(a: &mut App, e: &Entry, path: &str, size: (i32, i32)) -> Option<Pic> {
    use crate::ui::thumbs::{pictured, Key};
    let ext = dvfiles::naming::ext_lower(&e.name);
    let side = size.0.min(size.1);
    let own = art_for(a, e, path);
    if !e.is_dir() && pictured(&ext) {
        // a cloud file's thumbnail only if Windows already has one: making it would download the file
        let cached_only = dvfiles::fsx::is_cloud_only(e.attrs);
        if let Some(Some(img)) =
            a.thumbs.get(Key { what: path.to_string(), size }, path, cached_only, (e.size, e.modified))
        {
            return Some(Pic { img, label: None, arrow: false });
        }
    } else if own.is_none() {
        let (img, arrow) = icon_image(a, e, path, side);
        return Some(Pic { img, label: None, arrow });
    }
    let art = own.unwrap_or(Art::File(art::Kind::App));
    // icons no bigger than Large icons' (the preview pane shows them at that size, as Windows' Details pane does)
    let side = side.min(a.s(96.0));
    // a type whose program gives it an icon shows that one, as in Explorer
    if matches!(art, Art::File(_)) && !e.is_dir() {
        if let Some(img) = program_icon(a, &ext, side) {
            return Some(Pic { img, label: None, arrow: false });
        }
    }
    let label = if matches!(art, Art::File(_)) && side >= a.s(48.0) { art::band_label(&ext) } else { None };
    Some(Pic { img: a.art.get(art, side, &a.theme), label, arrow: false })
}

/// asks for the picture of the item at `pos`, if it has one, after what is on screen
fn ahead(a: &mut App, pos: usize, boxed: i32) {
    let Some(&i) = a.model.order.get(pos) else { return };
    let e = &a.model.entries[i as usize];
    if e.is_dir() || !crate::ui::thumbs::pictured(&dvfiles::naming::ext_lower(&e.name)) {
        return;
    }
    let (stamp, cloud) = ((e.size, e.modified), dvfiles::fsx::is_cloud_only(e.attrs));
    let path = a.model.path_of(i);
    a.thumbs.ahead(crate::ui::thumbs::Key { what: path.clone(), size: (boxed, boxed) }, &path, cloud, stamp);
}

/// draws a picture's band label in the band of a file icon drawn at (x, y)
pub fn band_text(a: &App, surf: &Surface, x: i32, y: i32, side: i32, label: &str) {
    let r = art::band_rect(x, y, side);
    surf.text(r, label, a.fonts.small, art::band_color(), gfx::CENTER);
}

pub fn clamp_scroll(a: &mut App) {
    let g = geo(a);
    let max_y = (g.content_h - g.view_h).max(0);
    a.lv.scroll_y = a.lv.scroll_y.clamp(0, max_y);
    let max_x = (g.total_w - g.w).max(0);
    a.lv.scroll_x = if g.view == ViewMode::Details { a.lv.scroll_x.clamp(0, max_x) } else { 0 };
}

pub fn scroll_to(a: &mut App, pos: usize) {
    a.lv.glide = None;
    let g = geo(a);
    let (top, h) = match g.view {
        ViewMode::Details => (pos as i32 * g.row_h, g.row_h),
        ViewMode::Icons => ((pos / g.per_row) as i32 * g.cell_h + g.pad, g.cell_h),
    };
    // before the first layout the list has no real size yet: nothing can be judged out of view
    if g.view_h < h {
        return;
    }
    if top < a.lv.scroll_y {
        a.lv.scroll_y = top;
    } else if top + h > a.lv.scroll_y + g.view_h {
        a.lv.scroll_y = top + h - g.view_h;
    }
    clamp_scroll(a);
}

fn display_name(a: &App, e: &Entry) -> String {
    if a.model.loc == Loc::Pc {
        return a.drive(&e.name).map_or_else(|| e.name.to_string(), |d| d.title());
    }
    // shortcuts never show their extension, as in Explorer (NeverShowExt)
    let never =
        !e.is_dir() && matches!(dvfiles::naming::ext_lower(&e.name).as_str(), "lnk" | "url" | "pif" | "appref-ms");
    if (!a.settings.show_ext || never) && !e.is_dir() {
        return dvfiles::naming::split_ext(&e.name, false).0.to_string();
    }
    e.name.to_string()
}

fn cell_text(a: &mut App, e: &Entry, c: Col) -> String {
    match c {
        Col::Name => String::new(),
        Col::Modified => locale::short(e.modified),
        Col::Type => {
            if a.model.loc == Loc::Pc {
                a.drive(&e.name).map_or("Drive", |d| d.type_name()).to_string()
            } else {
                a.icons.type_name(e).to_string()
            }
        }
        Col::Size => {
            if !e.is_dir() {
                a.nf.bytes(e.size)
            } else if a.settings.folder_sizes && a.model.search.is_none() {
                match a.model.sizes.get(&e.name.to_lowercase()) {
                    Some(&b) => a.nf.bytes(b),
                    None if a.model.sizes_pending > 0 && e.attrs & dvfiles::fsx::FA_REPARSE == 0 => "...".to_string(),
                    None => String::new(),
                }
            } else {
                String::new()
            }
        }
        Col::Folder => e.dir.as_deref().unwrap_or("").to_string(),
        Col::Total => a.drive(&e.name).filter(|d| d.total > 0).map_or(String::new(), |d| a.nf.bytes(d.total)),
        Col::Free => a.drive(&e.name).filter(|d| d.total > 0).map_or(String::new(), |d| a.nf.bytes(d.free)),
    }
}

/// the overlay scrollbars for the current geometry
fn bars(a: &App, g: &Geo) -> (Option<Track>, Option<Track>) {
    let top = if g.view == ViewMode::Details { g.header_h } else { 0 };
    let v = scroll::bar(
        true,
        R::new(0, top, g.w, g.h - top),
        g.content_h,
        g.view_h,
        a.lv.scroll_y,
        a.scale,
        a.lv.bar_hot == Some(true),
    );
    let h = if g.view == ViewMode::Details {
        scroll::bar(false, R::new(0, 0, g.w, g.h), g.total_w, g.w, a.lv.scroll_x, a.scale, a.lv.bar_hot == Some(false))
    } else {
        None
    };
    (v, h)
}

pub fn paint(a: &mut App) {
    let g = geo(a);
    if Surface::ensure(&mut a.surf_list, g.w, g.h).is_none() {
        return;
    }
    let t = a.theme;
    let focused = unsafe { GetFocus() } == a.list;
    let active = unsafe { GetForegroundWindow() } == a.main;
    let sel_bg = if focused || active { t.sel } else { t.sel_inactive };
    let (sy, sx) = (a.lv.scroll_y, a.lv.scroll_x);
    let n = a.model.order.len();
    let rad = a.rad();
    {
        // the card's face; its edge and rounded corners are drawn last, over what scrolls
        let surf = a.surf_list.as_ref().unwrap();
        surf.set_clip(None);
        surf.fill(R::new(0, 0, g.w, g.h), t.bg);
    }
    // what to say when there is nothing to list
    let empty_msg = if let Some(e) = &a.model.error {
        Some((Art::Tool(art::Tool::Warning), e.clone()))
    } else if n == 0 {
        match &a.model.search {
            Some(s) if s.running => Some((Art::Tool(art::Tool::Search), "Searching...".to_string())),
            Some(_) => Some((Art::Tool(art::Tool::Search), "No items match your search".to_string())),
            None if a.model.loading => None,
            None => Some((Art::Folder, "This folder is empty".to_string())),
        }
    } else {
        None
    };

    let first;
    let last;
    match g.view {
        ViewMode::Details => {
            first = (sy / g.row_h).max(0) as usize;
            last = (((sy + g.view_h) / g.row_h) as usize + 1).min(n);
        }
        ViewMode::Icons => {
            first = ((sy - g.pad).max(0) / g.cell_h) as usize * g.per_row;
            last = ((((sy + g.h) / g.cell_h) as usize + 1) * g.per_row).min(n);
        }
    }
    let top = if g.view == ViewMode::Details { g.header_h } else { 1 };
    a.surf_list.as_ref().unwrap().set_clip(Some(R::new(1, top, g.w - 1, g.h - top)));
    let small = a.icons.small_px;
    for pos in first..last {
        let i = a.model.order[pos];
        let e = a.model.entries[i as usize].clone();
        let drop_hot = a.lv.drop_hot == Some(pos);
        let selected = a.model.sel.is(i) || drop_hot;
        let hot = a.lv.hover == Some(pos) || drop_hot;
        let focus_item = focused && a.model.sel.focus == Some(i);
        let r = g.item_rect(pos, sy, sx);
        let path = a.model.path_of(i);
        let faded = e.is_hidden() || a.is_cut(&path);
        let name = display_name(a, &e);
        // the row's icon (and a shortcut's arrow), fetched before the surface is in use
        let row_icon = (g.view == ViewMode::Details).then(|| {
            let (img, arrow) = icon_image(a, &e, &path, small);
            (img, arrow.then(|| a.art.get(Art::Arrow, arrow_size(small), &t)))
        });
        let texts: Vec<(Col, String)> =
            g.cols.iter().filter(|c| c.0 != Col::Name).map(|c| (c.0, cell_text(a, &e, c.0))).collect();
        let drive = if a.model.loc == Loc::Pc { a.drive(&e.name).cloned() } else { None };
        let surf = a.surf_list.as_ref().unwrap();
        let f = &a.fonts;
        let bg = if drop_hot {
            t.sel.mix(t.accent, 0.12)
        } else if selected {
            if hot {
                sel_bg.mix(t.accent, 0.08)
            } else {
                sel_bg
            }
        } else if hot {
            t.hover
        } else {
            t.bg
        };
        // a soft rounded tint: the accent's for the selection, a neutral one under the pointer
        if selected || hot {
            surf.round(r, rad, bg);
            if drop_hot {
                // the folder a drop would go into
                surf.round_ring(r, rad, t.accent);
            }
        } else if focus_item {
            surf.round_ring(r, rad, t.sel_border);
        }
        match g.view {
            ViewMode::Details => {
                for &(c, cx, cw) in &g.cols {
                    let cell = R::new(cx - sx, r.y, cw, g.row_h);
                    match c {
                        Col::Name => {
                            let ix = cell.x + a.s(10.0);
                            let iy = cell.y + (g.row_h - small) / 2;
                            if let Some((img, arrow)) = &row_icon {
                                surf.image(ix + (small - img.w) / 2, iy + (small - img.h) / 2, img.w, img.h, &img.px);
                                if let Some(ar) = arrow {
                                    surf.image(ix, iy + small - ar.h, ar.w, ar.h, &ar.px);
                                }
                            }
                            if faded {
                                surf.blend(R::new(ix, iy, small, small), bg, 0.5);
                            }
                            // new in Downloads since the last look: a small accent dot before the icon
                            if a.new_since.is_some_and(|t| e.created.max(e.modified) > t) {
                                let d = a.s(6.0);
                                surf.round(
                                    R::new(cell.x + a.s(1.0), cell.y + (g.row_h - d) / 2, d, d),
                                    d as f32 / 2.0,
                                    t.accent,
                                );
                            }
                            let surf = a.surf_list.as_ref().unwrap();
                            let tx = ix + small + a.s(10.0);
                            if !renaming(a, &e) {
                                surf.text(
                                    R::new(tx, cell.y, cell.right() - tx - a.s(6.0), g.row_h),
                                    &name,
                                    a.fonts.ui,
                                    t.fg,
                                    gfx::LEFT,
                                );
                            }
                        }
                        Col::Free => {
                            if let Some(d) = drive.as_ref().filter(|d| d.total > 0) {
                                // the free space, and a capacity bar to its left
                                let free = texts.iter().find(|x| x.0 == c).map(|x| x.1.clone()).unwrap_or_default();
                                let tw = gfx::text_width(a.mdc, &free, f.ui);
                                surf.text(
                                    cell.inset(a.s(8.0), 0),
                                    &free,
                                    f.ui,
                                    if selected { t.fg } else { t.muted },
                                    gfx::RIGHT,
                                );
                                let bw = cell.w - tw - a.s(24.0);
                                if bw > a.s(24.0) {
                                    let bar = R::new(cell.x + a.s(8.0), cell.y + g.row_h / 2 - a.s(3.0), bw, a.s(6.0));
                                    let used = 1.0 - d.free as f32 / d.total as f32;
                                    let pill = bar.h as f32 / 2.0;
                                    surf.round(
                                        bar,
                                        pill,
                                        if selected { bg.mix(t.fg, 0.12) } else { t.line.mix(t.fg, 0.06) },
                                    );
                                    let uw = ((bar.w as f32 * used).round() as i32).max(bar.h);
                                    surf.round(
                                        R::new(bar.x, bar.y, uw, bar.h),
                                        pill,
                                        if used > 0.9 { t.danger } else { t.accent },
                                    );
                                }
                            }
                        }
                        _ => {
                            let text = texts.iter().find(|x| x.0 == c).map(|x| x.1.as_str()).unwrap_or("");
                            let flags = if c.right_aligned() {
                                gfx::RIGHT
                            } else if c == Col::Folder {
                                gfx::PATH_LEFT
                            } else {
                                gfx::LEFT
                            };
                            surf.text(
                                cell.inset(a.s(8.0), 0),
                                text,
                                f.ui,
                                if selected { t.fg } else { t.muted },
                                flags,
                            );
                        }
                    }
                }
            }
            ViewMode::Icons => {
                let boxed = picture_box(a);
                let bx = r.x + (r.w - boxed) / 2;
                let by = r.y + a.s(6.0);
                match picture(a, &e, &path, (boxed, boxed)) {
                    Some(pic) => {
                        // centred in the box, sitting on its bottom edge as Explorer's thumbnails do
                        let img = &pic.img;
                        let (ix, iy) = (bx + (boxed - img.w) / 2, by + (boxed - img.h).max(0));
                        let surf = a.surf_list.as_ref().unwrap();
                        surf.image(ix, iy, img.w, img.h, &img.px);
                        if let Some(l) = &pic.label {
                            band_text(a, surf, ix, iy, img.w, l);
                        }
                        if pic.arrow {
                            draw_arrow(a, |a| a.surf_list.as_ref(), ix, iy, img.w);
                        }
                        if faded {
                            a.surf_list.as_ref().unwrap().blend(R::new(ix, iy, img.w, img.h), bg, 0.5);
                        }
                    }
                    None => {}
                }
                let surf = a.surf_list.as_ref().unwrap();
                let tr =
                    R::new(r.x + a.s(6.0), by + boxed + a.s(6.0), r.w - a.s(12.0), r.bottom() - by - boxed - a.s(8.0));
                if !renaming(a, &e) {
                    surf.text(tr, &name, a.fonts.ui, t.fg, gfx::WRAP_CENTER);
                }
            }
        }
    }

    // the pictures a screen above and below are made while this one is looked at, so scrolling finds them ready
    if g.view == ViewMode::Icons && last > first {
        let screen = last - first;
        let boxed = picture_box(a);
        for pos in (last..(last + screen).min(n)).chain(first.saturating_sub(screen)..first) {
            ahead(a, pos, boxed);
        }
    }
    let surf = a.surf_list.as_ref().unwrap();
    let f = &a.fonts;
    if let Some((symbol, msg)) = empty_msg {
        // a quiet empty state in the upper middle of the card
        let cy = top + (g.h - top) / 3;
        let side = a.s(40.0);
        surf.image_in(R::new(0, cy - a.s(44.0), g.w, a.s(44.0)), &a.art.get(symbol, side, &t));
        surf.text(R::new(a.s(16.0), cy + a.s(10.0), g.w - a.s(32.0), a.s(24.0)), &msg, f.bold, t.muted, gfx::CENTER);
    } else if a.model.loading && n == 0 {
        surf.text(R::new(0, top + a.s(40.0), g.w, a.s(24.0)), "Working on it...", f.ui, t.muted, gfx::CENTER);
    }

    if let (Some(b), Some(_)) = (a.lv.rename_box, a.lv.rename_edit) {
        // the rename box: the text box's look, the accent along its foot
        surf.fill(b.inset(1, 1), t.input_bg);
        surf.frame(b, t.input_border);
        surf.fill(R::new(b.x + 1, b.bottom() - a.s(2.0), b.w - 2, a.s(2.0)), t.accent);
    }
    if let Drag::Band { x0, y0, x1, y1, .. } = &a.lv.drag {
        let band = R::new((*x0).min(*x1) - sx, (*y0).min(*y1) - sy, (x1 - x0).abs(), (y1 - y0).abs());
        surf.round_blend(band, a.s(3.0) as f32, t.accent, 0.16);
        surf.round_ring(band, a.s(3.0) as f32, t.accent);
    }
    let (vb, hb) = bars(a, &g);
    let surf = a.surf_list.as_ref().unwrap();
    for (b, vertical) in [(vb, true), (hb, false)] {
        if let Some(b) = b {
            let hot =
                a.lv.bar_hot == Some(vertical) || matches!(a.lv.drag, Drag::Thumb { vertical: v, .. } if v == vertical);
            b.draw(surf, &t, hot);
        }
    }

    // the header, drawn last: rows scroll under it
    surf.set_clip(Some(R::new(1, 1, g.w - 1, g.h - 1)));
    if g.view == ViewMode::Details {
        for (k, &(c, cx, cw)) in g.cols.iter().enumerate() {
            let cell = R::new(cx - sx, 1, cw, g.header_h - 2);
            if a.lv.header_hot == Some(k) {
                surf.round(cell.inset(a.s(2.0), a.s(4.0)), rad, t.hover);
            }
            // the column edges, faint
            surf.blend(R::new(cell.right() - 1, cell.y + a.s(9.0), 1, cell.h - a.s(18.0)), t.fg, 0.10);
            let flags = if c.right_aligned() { gfx::RIGHT } else { gfx::LEFT };
            let sorted = a.settings.sort == c && a.model.loc != Loc::Pc;
            surf.text(cell.inset(a.s(8.0), 0), c.title(), f.ui, t.header_fg, flags);
            if sorted {
                // and the sort direction as a small chevron centred above the title
                let m = if a.settings.sort_desc { art::Mark::Down } else { art::Mark::Up };
                surf.image_in(R::new(cell.x, 0, cell.w, a.s(10.0)), &a.art.mark(m, a.s(10.0), t.muted));
            }
        }
        surf.fill(R::new(1, g.header_h - 1, g.w - 1, 1), t.line);
    }
    surf.set_clip(None);
    crate::ui::bubble::draw(a, g.w, g.h);
    // the card's edge, and its rounded corners: all four, or the left two when the preview carries on to the right
    let alone = a.bar.layout.as_ref().is_none_or(|l| g.w >= l.card.w);
    let surf = a.surf_list.as_ref().unwrap();
    surf.fill(R::new(0, 0, g.w, 1), t.card_edge);
    surf.fill(R::new(0, g.h - 1, g.w, 1), t.card_border);
    surf.fill(R::new(0, 0, 1, g.h), t.card_border);
    if alone {
        surf.fill(R::new(g.w - 1, 0, 1, g.h), t.card_border);
    }
    crate::ui::card_corners(a, surf, a.list, g.w, g.h, if alone { 15 } else { 1 | 8 });
    surf.present(a.list);
    a.icons.flush();
    crate::ui::preview::follow(a);
}

// ---------------------------------------------------------------------------------------------------------------
// input

fn header_hit(a: &App, g: &Geo, x: i32, y: i32) -> Option<(usize, bool)> {
    if g.view != ViewMode::Details || y >= g.header_h {
        return None;
    }
    let edge = a.s(5.0);
    for (k, &(_, cx, cw)) in g.cols.iter().enumerate() {
        let right = cx + cw - a.lv.scroll_x;
        if (x - right).abs() <= edge {
            return Some((k, true));
        }
        if x >= cx - a.lv.scroll_x && x < right {
            return Some((k, false));
        }
    }
    None
}

fn mouse_down(x: i32, y: i32, right: bool) {
    // the undo bubble sits over the rows: a press on it is its own
    use crate::ui::bubble::{self, Part};
    match with_app(|a| bubble::hit(a, x, y)).flatten() {
        Some(Part::Undo) if !right => {
            with_app(bubble::hide);
            cmds::run(Cmd::Undo);
            return;
        }
        Some(Part::Close) if !right => {
            with_app(bubble::hide);
            return;
        }
        Some(_) => return,
        None => {}
    }
    let list = with_app(|a| a.list);
    if let Some(l) = list {
        crate::ui::focus(l);
    }
    end_rename(true);
    let capture = with_app(|a| {
        let g = geo(a);
        let (ctrl, shift) = (key_down(VK_CONTROL), key_down(VK_SHIFT));
        if !right {
            // the overlay scrollbars come first: they sit on top of the rows
            let (vb, hb) = bars(a, &g);
            for b in [vb, hb].into_iter().flatten() {
                if b.near(x, y, a.s(4.0)) {
                    a.lv.glide = None;
                    let (from, scroll0) = if b.vertical { (y, a.lv.scroll_y) } else { (x, a.lv.scroll_x) };
                    if b.on_thumb(x, y) {
                        a.lv.drag = Drag::Thumb { vertical: b.vertical, from, scroll0 };
                    } else {
                        let to = b.page(x, y, scroll0);
                        if b.vertical {
                            a.lv.scroll_y = to;
                        } else {
                            a.lv.scroll_x = to;
                        }
                        clamp_scroll(a);
                    }
                    invalidate(a.list);
                    return true;
                }
            }
            if let Some((k, edge)) = header_hit(a, &g, x, y) {
                let c = g.cols[k].0;
                a.lv.drag = if edge {
                    Drag::Resize { col: c, start_x: x, start_w: a.settings.width(c) }
                } else {
                    Drag::Header { index: k }
                };
                return true;
            }
        }
        if g.view == ViewMode::Details && y < g.header_h {
            return false;
        }
        let n = a.model.order.len();
        match g.hit(x, y, a.lv.scroll_y, a.lv.scroll_x, n) {
            Some(pos) => {
                let entry = a.model.order[pos];
                let was = a.model.sel.is(entry);
                if right {
                    if !was {
                        a.model.sel.only(entry);
                    } else {
                        a.model.sel.focus = Some(entry);
                    }
                } else if ctrl || shift {
                    a.model.sel.click(&a.model.order, pos, ctrl, shift);
                } else if !was {
                    a.model.sel.only(entry);
                }
                if !right {
                    a.lv.drag = Drag::Pending { x, y, pos, defer: was && !ctrl && !shift };
                }
            }
            None => {
                if !right {
                    let base = ctrl.then(|| a.model.sel.clone());
                    if !ctrl {
                        a.model.sel.clear();
                    }
                    let (cx, cy) = (x + a.lv.scroll_x, y + a.lv.scroll_y);
                    a.lv.drag = Drag::Band { x0: cx, y0: cy, x1: cx, y1: cy, base };
                } else {
                    a.model.sel.clear();
                }
            }
        }
        invalidate(a.list);
        invalidate(a.main);
        !right
    });
    if capture == Some(true) {
        if let Some(l) = list {
            unsafe {
                SetCapture(l);
            }
        }
    }
}

fn mouse_move(x: i32, y: i32) {
    enum Next {
        Nothing,
        DragOut(Vec<String>, Option<crate::ui::thumbs::Image>),
    }
    let next = with_app(|a| {
        if !a.lv.tracking {
            let mut tme = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: a.list,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut tme);
            }
            a.lv.tracking = true;
        }
        let g = geo(a);
        match a.lv.drag.clone() {
            Drag::Pending { x: x0, y: y0, pos, .. } => {
                let (dx, dy) = unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) };
                if (x - x0).abs() > dx || (y - y0).abs() > dy {
                    a.lv.drag = Drag::None;
                    if a.model.loc != Loc::Pc {
                        let paths = a.model.selected_paths();
                        let image = crate::ui::drag::image(a, pos, paths.len());
                        return Next::DragOut(paths, image);
                    }
                }
                Next::Nothing
            }
            Drag::Band { x0, y0, base, .. } => {
                let (cx, cy) = (x + a.lv.scroll_x, y + a.lv.scroll_y);
                a.lv.drag = Drag::Band { x0, y0, x1: cx, y1: cy, base: base.clone() };
                let n = a.model.order.len();
                // band hits are worked out without the header offset in icon view
                let hits = g.band_hits(x0, y0, cx, cy, n);
                let order = a.model.order.clone();
                a.model.sel.band(&order, base.as_ref(), hits.into_iter());
                if y < g.header_h + a.s(4.0) || y > g.h - a.s(4.0) {
                    unsafe {
                        SetTimer(Some(a.list), T_AUTOSCROLL, 30, None);
                    }
                }
                invalidate(a.list);
                invalidate(a.main);
                Next::Nothing
            }
            Drag::Resize { col, start_x, start_w } => {
                let w = start_w + ((x - start_x) as f32 / a.scale) as i32;
                a.settings.set_width(col, w.clamp(40, 2000));
                clamp_scroll(a);
                invalidate(a.list);
                Next::Nothing
            }
            Drag::Header { .. } => Next::Nothing,
            Drag::Thumb { vertical, from, scroll0 } => {
                let (vb, hb) = bars(a, &g);
                if let Some(b) = if vertical { vb } else { hb } {
                    let to = b.dragged(scroll0, if vertical { y - from } else { x - from });
                    if vertical {
                        a.lv.scroll_y = to;
                    } else {
                        a.lv.scroll_x = to;
                    }
                    clamp_scroll(a);
                    invalidate(a.list);
                }
                Next::Nothing
            }
            Drag::None => {
                let (vb, hb) = bars(a, &g);
                let near = vb
                    .filter(|b| b.near(x, y, a.s(10.0)))
                    .map(|_| true)
                    .or(hb.filter(|b| b.near(x, y, a.s(10.0))).map(|_| false));
                if near != a.lv.bar_hot {
                    a.lv.bar_hot = near;
                    invalidate(a.list);
                }
                let n = a.model.order.len();
                let hover = g.hit(x, y, a.lv.scroll_y, a.lv.scroll_x, n);
                let hh = header_hit(a, &g, x, y).map(|h| h.0);
                if hover != a.lv.hover || hh != a.lv.header_hot {
                    a.lv.hover = hover;
                    a.lv.header_hot = hh;
                    invalidate(a.list);
                }
                Next::Nothing
            }
        }
    });
    if let Some(Next::DragOut(paths, image)) = next {
        unsafe {
            let _ = ReleaseCapture();
        }
        if let Some(l) = with_app(|a| a.list) {
            crate::ui::drag::start(l, &paths, image);
        }
    }
}

fn mouse_up(x: i32, y: i32) {
    let sort = with_app(|a| {
        let drag = std::mem::take(&mut a.lv.drag);
        unsafe {
            let _ = KillTimer(Some(a.list), T_AUTOSCROLL);
        }
        invalidate(a.list);
        invalidate(a.main);
        match drag {
            Drag::Pending { pos, defer: true, .. } => {
                if let Some(&e) = a.model.order.get(pos) {
                    a.model.sel.only(e);
                }
                None
            }
            Drag::Header { index } => {
                let g = geo(a);
                header_hit(a, &g, x, y).filter(|h| h.0 == index && !h.1).map(|_| g.cols[index].0)
            }
            _ => None,
        }
    })
    .flatten();
    unsafe {
        let _ = ReleaseCapture();
    }
    if let Some(c) = sort {
        cmds::run(Cmd::SortBy(c));
    }
}

fn autoscroll() {
    with_app(|a| {
        let Drag::Band { .. } = a.lv.drag else {
            unsafe {
                let _ = KillTimer(Some(a.list), T_AUTOSCROLL);
            }
            return;
        };
        let mut p = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut p);
            let _ = ScreenToClient(a.list, &mut p);
        }
        let g = geo(a);
        let step = a.s(20.0);
        if p.y < g.header_h {
            a.lv.scroll_y -= step;
        } else if p.y > g.h {
            a.lv.scroll_y += step;
        } else {
            unsafe {
                let _ = KillTimer(Some(a.list), T_AUTOSCROLL);
            }
        }
        clamp_scroll(a);
    });
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
        if let Some(l) = with_app(|a| a.list) {
            let _ = ScreenToClient(l, &mut p);
        }
    }
    mouse_move(p.x, p.y);
}

fn wheel(delta: i16) {
    if key_down(VK_CONTROL) {
        let next = with_app(|a| a.settings.view).map(|_| if delta > 0 { ViewMode::Icons } else { ViewMode::Details });
        if let Some(v) = next {
            if Some(v) != with_app(|a| a.settings.view) {
                cmds::run(Cmd::View(v));
            }
        }
        return;
    }
    with_app(|a| {
        let mut lines = 3u32;
        unsafe {
            let _ = SystemParametersInfoW(
                SPI_GETWHEELSCROLLLINES,
                0,
                Some(&mut lines as *mut u32 as *mut _),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            );
        }
        let g = geo(a);
        let unit = if g.view == ViewMode::Details { g.row_h } else { g.cell_h / 2 };
        let max = (g.content_h - g.view_h).max(0);
        let from = a.lv.glide.unwrap_or(a.lv.scroll_y);
        let to = (from - (delta as i32 * lines.min(20) as i32 * unit) / 120).clamp(0, max);
        if to != a.lv.scroll_y {
            a.lv.glide = Some(to);
            unsafe {
                SetTimer(Some(a.list), T_GLIDE, 8, None);
            }
        }
    });
}

/// one step of the wheel glide: a third of the way each tick, which settles in about 80 ms
fn glide_step() {
    with_app(|a| {
        let Some(to) = a.lv.glide else {
            unsafe {
                let _ = KillTimer(Some(a.list), T_GLIDE);
            }
            return;
        };
        let d = to - a.lv.scroll_y;
        let step = if d.abs() <= 2 { d } else { (d as f32 * 0.35).round() as i32 };
        a.lv.scroll_y += step;
        if a.lv.scroll_y == to {
            a.lv.glide = None;
            unsafe {
                let _ = KillTimer(Some(a.list), T_GLIDE);
            }
        }
        clamp_scroll(a);
        invalidate(a.list);
    });
}

fn scroll_bar(bar: SCROLLBAR_CONSTANTS, code: u16) {
    with_app(|a| {
        a.lv.glide = None;
        let g = geo(a);
        let line = if g.view == ViewMode::Details { g.row_h } else { g.cell_h / 2 };
        let mut si =
            SCROLLINFO { cbSize: std::mem::size_of::<SCROLLINFO>() as u32, fMask: SIF_ALL, ..Default::default() };
        unsafe {
            let _ = GetScrollInfo(a.list, bar, &mut si);
        }
        let (cur, page) = if bar == SB_VERT { (a.lv.scroll_y, g.view_h) } else { (a.lv.scroll_x, g.w) };
        let next = match SCROLLBAR_COMMAND(code as i32) {
            SB_LINEUP => cur - line,
            SB_LINEDOWN => cur + line,
            SB_PAGEUP => cur - page,
            SB_PAGEDOWN => cur + page,
            SB_THUMBTRACK | SB_THUMBPOSITION => si.nTrackPos,
            SB_TOP => 0,
            SB_BOTTOM => i32::MAX / 2,
            _ => cur,
        };
        if bar == SB_VERT {
            a.lv.scroll_y = next;
        } else {
            a.lv.scroll_x = next;
        }
        clamp_scroll(a);
        invalidate(a.list);
    });
}

fn key(vk: VIRTUAL_KEY) -> bool {
    let (ctrl, shift) = (key_down(VK_CONTROL), key_down(VK_SHIFT));
    let cmd = match vk {
        VK_RETURN => Some(Cmd::Open),
        VK_BACK => Some(Cmd::Back),
        VK_DELETE if shift => Some(Cmd::DeletePermanently),
        VK_DELETE => Some(Cmd::Delete),
        VK_F2 => Some(Cmd::Rename),
        VK_A if ctrl => Some(Cmd::SelectAll),
        VK_C if ctrl && shift => Some(Cmd::CopyPath),
        VK_C if ctrl => Some(Cmd::Copy),
        VK_X if ctrl => Some(Cmd::Cut),
        VK_V if ctrl => Some(Cmd::Paste),
        VK_I if ctrl => Some(Cmd::InvertSelection),
        VK_Z if ctrl => Some(Cmd::Undo),
        _ => None,
    };
    if let Some(c) = cmd {
        cmds::run(c);
        return true;
    }
    with_app(|a| {
        let n = a.model.order.len();
        if n == 0 {
            return false;
        }
        let g = geo(a);
        let cur = a.model.sel.focus_pos(&a.model.order);
        let pos = cur.unwrap_or(0) as i64;
        let per_page = match g.view {
            ViewMode::Details => (g.view_h / g.row_h).max(1) as i64,
            ViewMode::Icons => ((g.view_h / g.cell_h).max(1) as usize * g.per_row) as i64,
        };
        let step = if g.view == ViewMode::Icons { g.per_row as i64 } else { 1 };
        let target = match vk {
            VK_UP => pos - step,
            VK_DOWN if cur.is_none() => 0,
            VK_DOWN => pos + step,
            VK_LEFT if g.view == ViewMode::Icons => pos - 1,
            VK_RIGHT if g.view == ViewMode::Icons => pos + 1,
            VK_HOME => 0,
            VK_END => n as i64 - 1,
            VK_PRIOR => pos - per_page,
            VK_NEXT => pos + per_page,
            VK_SPACE => {
                if ctrl {
                    a.model.sel.toggle_focus();
                } else if let Some(f) = a.model.sel.focus {
                    if !a.model.sel.is(f) {
                        a.model.sel.only(f);
                    }
                }
                invalidate(a.list);
                invalidate(a.main);
                return true;
            }
            _ => return false,
        };
        let target = target.clamp(0, n as i64 - 1) as usize;
        let order = a.model.order.clone();
        a.model.sel.key_move(&order, target, ctrl, shift);
        scroll_to(a, target);
        invalidate(a.list);
        invalidate(a.main);
        true
    })
    .unwrap_or(false)
}

/// type the start of a name to jump to it
fn typeahead(c: char) {
    if c.is_control() {
        return;
    }
    with_app(|a| {
        let now = unsafe { windows::Win32::System::SystemInformation::GetTickCount() };
        if now.wrapping_sub(a.lv.typeahead_at) > 1000 {
            a.lv.typeahead.clear();
        }
        a.lv.typeahead_at = now;
        a.lv.typeahead.push(c);
        let prefix = a.lv.typeahead.to_lowercase();
        let n = a.model.order.len();
        if n == 0 {
            return;
        }
        // the same letter again moves on to the next match
        let repeat = prefix.chars().count() > 1 && prefix.chars().all(|x| Some(x) == prefix.chars().next());
        let key: String = if repeat { prefix.chars().take(1).collect() } else { prefix.clone() };
        let start =
            a.model
                .sel
                .focus_pos(&a.model.order)
                .map_or(0, |p| if repeat || key.chars().count() == 1 { p + 1 } else { p });
        let found = (0..n).map(|k| (start + k) % n).find(|&p| {
            let e = &a.model.entries[a.model.order[p] as usize];
            display_name(a, e).to_lowercase().starts_with(&key)
        });
        if let Some(p) = found {
            let e = a.model.order[p];
            a.model.sel.only(e);
            scroll_to(a, p);
            invalidate(a.list);
            invalidate(a.main);
        }
    });
}

fn context_menu(x: i32, y: i32) {
    let info = with_app(|a| {
        let (sx, sy) = if x == -1 && y == -1 {
            // from the keyboard: at the focused item
            let g = geo(a);
            let p = a.model.sel.focus_pos(&a.model.order).unwrap_or(0);
            let r = g.item_rect(p, a.lv.scroll_y, a.lv.scroll_x);
            let mut pt = POINT { x: r.x + a.s(20.0), y: r.bottom() };
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::ClientToScreen(a.list, &mut pt);
            }
            (pt.x, pt.y)
        } else {
            (x, y)
        };
        let mut pt = POINT { x: sx, y: sy };
        unsafe {
            let _ = ScreenToClient(a.list, &mut pt);
        }
        let g = geo(a);
        let on_item = (x == -1 && y == -1 && a.model.sel.count() > 0)
            || g.hit(pt.x, pt.y, a.lv.scroll_y, a.lv.scroll_x, a.model.order.len()).is_some();
        (sx, sy, on_item && a.model.sel.count() > 0)
    });
    if let Some((sx, sy, on_item)) = info {
        if on_item {
            cmds::item_menu(sx, sy);
        } else {
            cmds::background_menu(sx, sy);
        }
    }
}

/// The folder a drag over the list would drop into: the folder (or drive) under the pointer, else the folder shown.
/// Also says which item to light up. None where nothing can go: onto the dragged items themselves or into them,
/// search results' background, This PC's background.
pub fn drop_target_at(a: &App, x: i32, y: i32, paths: &[String]) -> Option<(String, Option<usize>)> {
    let inside =
        |folder: &str| paths.iter().any(|p| dvfiles::pathx::same(p, folder) || dvfiles::pathx::is_within(folder, p));
    let g = geo(a);
    if g.view == ViewMode::Details && y < g.header_h {
        return None;
    }
    if let Some(pos) = g.hit(x, y, a.lv.scroll_y, a.lv.scroll_x, a.model.order.len()) {
        let i = a.model.order[pos];
        if a.model.entries[i as usize].is_dir() {
            let path = a.model.path_of(i);
            return (!inside(&path)).then_some((path, Some(pos)));
        }
    }
    if a.model.search.is_some() {
        return None;
    }
    let folder = a.model.loc.path()?.to_string();
    (!inside(&folder)).then_some((folder, None))
}

/// a drag held near the top or bottom edge scrolls the list
pub fn drag_scroll(a: &mut App, y: i32) {
    let g = geo(a);
    let edge = a.s(24.0);
    let top = if g.view == ViewMode::Details { g.header_h } else { 0 };
    let step = if y < top + edge {
        -a.s(18.0)
    } else if y > g.h - edge {
        a.s(18.0)
    } else {
        return;
    };
    a.lv.scroll_y += step;
    clamp_scroll(a);
    invalidate(a.list);
}

fn drop_files(hdrop: HGLOBAL) {
    // the drop's DROPFILES block, read directly (DragQueryFile would load the shell), then given back as DragFinish
    // would
    let (paths, (x, y)) = crate::ui::clip::read_hdrop(hdrop);
    unsafe {
        let _ = windows::Win32::Foundation::GlobalFree(Some(hdrop));
    }
    let pt = POINT { x, y };
    if paths.is_empty() {
        return;
    }
    let target = with_app(|a| {
        let g = geo(a);
        let over = g.hit(pt.x, pt.y, a.lv.scroll_y, a.lv.scroll_x, a.model.order.len());
        if let Some(pos) = over {
            let i = a.model.order[pos];
            let path = a.model.path_of(i);
            if a.model.entries[i as usize].is_dir() && !paths.iter().any(|p| dvfiles::pathx::same(p, &path)) {
                return Some(path);
            }
        }
        if a.model.search.is_some() {
            return None;
        }
        a.model.loc.path().map(String::from)
    })
    .flatten();
    if let Some(t) = target {
        cmds::drop_into(paths, t);
    }
}

// ---------------------------------------------------------------------------------------------------------------
// rename in place

pub fn begin_rename() {
    let plan = with_app(|a| {
        if a.model.loc == Loc::Pc || a.lv.rename_edit.is_some() {
            return None;
        }
        let f = a.model.sel.focus.filter(|&f| a.model.sel.is(f)).or_else(|| a.model.selected().first().copied())?;
        let pos = a.model.order.iter().position(|&e| e == f)?;
        scroll_to(a, pos);
        let e = &a.model.entries[f as usize];
        let folder = e.dir.as_deref().map(String::from).unwrap_or_else(|| a.model.folder().to_string());
        a.lv.rename_target = Some((folder, e.name.to_string(), e.is_dir()));
        let name = e.name.to_string();
        let rect = rename_rect(a, pos, &name);
        a.lv.rename_box = Some(rect);
        Some((a.list, rect, name, a.model.entries[f as usize].is_dir(), a.fonts.ui, a.s(2.0)))
    })
    .flatten();
    let Some((list, r, name, is_dir, font, margin)) = plan else { return };
    unsafe {
        // no border of its own: the list draws a thin one around it (see paint), and the text sits where the label was
        let style = WS_CHILD | WS_VISIBLE | WINDOW_STYLE(ES_AUTOHSCROLL as u32);
        let inner = r.inset(1, 1);
        let Ok(edit) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("EDIT"),
            PCWSTR(wstr(&name).as_ptr()),
            style,
            inner.x,
            inner.y,
            inner.w,
            inner.h,
            Some(list),
            None,
            None,
            None,
        ) else {
            return;
        };
        SendMessageW(edit, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        // EC_LEFTMARGIN | EC_RIGHTMARGIN
        SendMessageW(edit, 0xD3, Some(WPARAM(3)), Some(LPARAM((margin | margin << 16) as isize)));
        let stem = dvfiles::naming::split_ext(&name, is_dir).0.encode_utf16().count();
        SendMessageW(edit, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(stem as isize)));
        with_app(|a| {
            a.lv.rename_edit = Some(edit);
            invalidate(a.list);
        });
        crate::ui::focus(edit);
    }
}

/// whether `e` is the item whose rename box is open (its label is not drawn then: the box holds it)
fn renaming(a: &App, e: &Entry) -> bool {
    a.lv.rename_edit.is_some() && a.lv.rename_target.as_ref().is_some_and(|(_, n, _)| **n == *e.name)
}

/// Where the rename box goes for the item at `pos` holding `text`, as Explorer's: from where the label starts, as
/// wide as the text and a little more (up to the column's edge), one line high and centred on the row.
fn rename_rect(a: &App, pos: usize, text: &str) -> R {
    let g = geo(a);
    let r = g.item_rect(pos, a.lv.scroll_y, a.lv.scroll_x);
    let h = gfx::line_height(a.mdc, a.fonts.ui) + a.s(4.0);
    let w = gfx::text_width(a.mdc, text, a.fonts.ui) + a.s(16.0);
    match g.view {
        ViewMode::Details => {
            let (cx, cw) = g.cols.first().map_or((r.x, a.s(200.0)), |c| (c.1 - a.lv.scroll_x, c.2));
            // the label's own start (see paint), less the box's edge and margin
            let x = cx + a.s(10.0) + a.icons.small_px + a.s(10.0) - a.s(3.0);
            let w = w.clamp(a.s(48.0), (cx + cw - x - a.s(4.0)).max(a.s(48.0)));
            R::new(x, r.y + (r.h - h) / 2, w, h)
        }
        ViewMode::Icons => {
            let w = w.clamp(a.s(48.0), (r.w - a.s(8.0)).max(a.s(48.0)));
            R::new(r.x + (r.w - w) / 2, r.y + a.s(6.0) + picture_box(a) + a.s(4.0), w, h)
        }
    }
}

/// the rename box follows its text as it is typed
fn rename_resized() {
    let Some((edit, list)) = with_app(|a| a.lv.rename_edit.map(|e| (e, a.list))).flatten() else { return };
    let text = crate::ui::window_text(edit);
    let r = with_app(|a| {
        let name = a.lv.rename_target.as_ref()?.1.clone();
        let pos = a.model.order.iter().position(|&i| *a.model.entries[i as usize].name == *name)?;
        let r = rename_rect(a, pos, &text);
        a.lv.rename_box = Some(r);
        Some(r)
    })
    .flatten();
    if let Some(r) = r {
        let inner = r.inset(1, 1);
        unsafe {
            let _ = MoveWindow(edit, inner.x, inner.y, inner.w, inner.h, true);
        }
        invalidate(list);
    }
}

pub fn end_rename(commit: bool) {
    let Some((edit, target, main, deferred)) = with_app(|a| {
        let edit = a.lv.rename_edit.take()?;
        a.lv.rename_box = None;
        invalidate(a.list);
        Some((edit, a.lv.rename_target.take(), a.main, std::mem::take(&mut a.lv.refresh_after_rename)))
    })
    .flatten() else {
        return;
    };
    let text = crate::ui::window_text(edit);
    unsafe {
        let _ = DestroyWindow(edit);
    }
    cmds::focus_list();
    // with several items chosen, the name typed renames them all
    let several = with_app(|a| {
        let paths = a.model.selected_paths();
        (paths.len() > 1).then(|| paths.iter().map(|p| (p.clone(), dvfiles::fsx::is_dir(p))).collect::<Vec<_>>())
    })
    .flatten();
    match (target.filter(|_| commit), several) {
        (Some(_), Some(items)) => rename_many(main, items, &text),
        (Some(t), None) => rename_item(main, t, &text),
        _ => {}
    }
    // the reload held back while the box was open, now that the rename is done
    if deferred {
        crate::ui::refresh();
    }
}

fn rename_item(main: HWND, (folder, old, is_dir): (String, String, bool), text: &str) {
    let mut new = match dvfiles::naming::validate(text) {
        Ok(n) => n,
        Err(e) => {
            crate::ui::shell::message(main, e, true);
            return;
        }
    };
    if new == old {
        return;
    }
    // with extensions hidden the user typed only the stem
    let hide_ext = with_app(|a| !a.settings.show_ext).unwrap_or(false);
    if hide_ext && !is_dir {
        let ext = dvfiles::naming::split_ext(&old, false).1;
        if !ext.is_empty() && !new.to_lowercase().ends_with(&ext.to_lowercase()) {
            new.push_str(ext);
        }
    }
    let from = dvfiles::pathx::join(&folder, &old);
    let to = dvfiles::pathx::join(&folder, &new);
    let case_only = old.eq_ignore_ascii_case(&new);
    if !case_only && dvfiles::fsx::exists(&to) {
        crate::ui::shell::message(main, &format!("There is already an item named \"{new}\" here."), true);
        return;
    }
    let r = unsafe {
        windows::Win32::Storage::FileSystem::MoveFileExW(
            PCWSTR(dvfiles::wide::long(&from).as_ptr()),
            PCWSTR(dvfiles::wide::long(&to).as_ptr()),
            windows::Win32::Storage::FileSystem::MOVE_FILE_FLAGS(0),
        )
    };
    match r {
        Ok(()) => {
            with_app(|a| {
                // the item is found again by its old name and folder, wherever the listing has put it since
                let at = a.model.entries.iter().position(|e| {
                    *e.name == *old
                        && e.dir.as_deref().map_or_else(
                            || dvfiles::pathx::same(a.model.folder(), &folder),
                            |d| dvfiles::pathx::same(d, &folder),
                        )
                });
                if let Some(i) = at {
                    a.model.entries[i].name = new.clone().into_boxed_str();
                }
                a.push_undo(dvfiles::undo::Action::Rename { dir: folder.clone(), from: old.clone(), to: new.clone() });
                a.model.pending_select = vec![new.clone()];
                a.resort();
                a.ensure_visible_focus();
            });
        }
        Err(e) => crate::ui::shell::message(main, &format!("Could not rename \"{old}\": {}", e.message()), true),
    }
}

/// Renames several items from one typed pattern (see `naming::batch_names`), all or none, as one step to undo.
fn rename_many(main: HWND, items: Vec<(String, bool)>, text: &str) {
    use dvfiles::naming::Batch;
    let names: Vec<(String, bool)> =
        items.iter().map(|(p, d)| (dvfiles::pathx::file_name(p).to_string(), *d)).collect();
    let batch = Batch::parse(text);
    let new = if let Batch::Pattern(_) = batch {
        pattern_names(main, &items, &names, text)
    } else {
        let new = dvfiles::naming::batch_names(batch, &names);
        match new.iter().find_map(|n| dvfiles::naming::validate(n).err()) {
            Some(e) => {
                crate::ui::shell::message(main, e, true);
                None
            }
            None => Some(new),
        }
    };
    let Some(new) = new else { return };
    rename_pairs(main, &items, new);
}

/// the names from a numbering pattern, with the focused item's extension taken off it when it was typed
fn pattern_names(main: HWND, items: &[(String, bool)], names: &[(String, bool)], text: &str) -> Option<Vec<String>> {
    let pattern = match dvfiles::naming::validate(text) {
        Ok(n) => n,
        Err(e) => {
            crate::ui::shell::message(main, e, true);
            return None;
        }
    };
    // with extensions shown the typed name may carry the focused item's extension: each keeps its own instead
    let pattern = items.iter().find(|(_, d)| !d).map_or(pattern.clone(), |(p, _)| {
        let ext = dvfiles::naming::split_ext(dvfiles::pathx::file_name(p), false).1;
        if !ext.is_empty() && pattern.to_lowercase().ends_with(&ext.to_lowercase()) {
            pattern[..pattern.len() - ext.len()].to_string()
        } else {
            pattern.clone()
        }
    });
    Some(dvfiles::naming::batch_names(dvfiles::naming::Batch::Pattern(&pattern), names))
}

fn rename_pairs(main: HWND, items: &[(String, bool)], new: Vec<String>) {
    let pairs: Vec<(String, String)> = items
        .iter()
        .zip(&new)
        .map(|((p, _), n)| (p.clone(), dvfiles::pathx::join(&dvfiles::pathx::parent(p).unwrap_or_default(), n)))
        .filter(|(f, t)| f != t)
        .collect();
    if let Some((_, t)) =
        pairs.iter().find(|(_, t)| dvfiles::fsx::exists(t) && !pairs.iter().any(|(f, _)| f.eq_ignore_ascii_case(t)))
    {
        crate::ui::shell::message(
            main,
            &format!("There is already an item named \"{}\" here.", dvfiles::pathx::file_name(t)),
            true,
        );
        return;
    }
    // the names before and after, to see before anything changes
    if pairs.len() > 1 {
        const SHOWN: usize = 8;
        let mut text = format!("Rename {} items?\n", pairs.len());
        for (f, t) in pairs.iter().take(SHOWN) {
            text += &format!("\n{}  \u{2192}  {}", dvfiles::pathx::file_name(f), dvfiles::pathx::file_name(t));
        }
        if pairs.len() > SHOWN {
            text += &format!("\n... and {} more", pairs.len() - SHOWN);
        }
        if !crate::ui::shell::ask(main, &text) {
            return;
        }
    }
    let mv = |a: &str, b: &str| unsafe {
        windows::Win32::Storage::FileSystem::MoveFileExW(
            PCWSTR(dvfiles::wide::long(a).as_ptr()),
            PCWSTR(dvfiles::wide::long(b).as_ptr()),
            windows::Win32::Storage::FileSystem::MOVE_FILE_FLAGS(0),
        )
    };
    // through passing names first, so items can take each other's names
    let tmp: Vec<String> = pairs.iter().enumerate().map(|(k, (f, _))| format!("{f}.dvrename{k}")).collect();
    let mut done = 0;
    for ((f, _), t) in pairs.iter().zip(&tmp) {
        if mv(f, t).is_err() {
            break;
        }
        done += 1;
    }
    if done < pairs.len() {
        for ((f, _), t) in pairs.iter().zip(&tmp).take(done) {
            let _ = mv(t, f);
        }
        crate::ui::shell::message(main, "Could not rename the items: one of them is in use.", true);
        return;
    }
    for ((_, to), t) in pairs.iter().zip(&tmp) {
        let _ = mv(t, to);
    }
    with_app(|a| {
        a.push_undo(dvfiles::undo::Action::Moved(pairs.clone()));
        a.model.pending_select = pairs.iter().map(|(_, t)| dvfiles::pathx::file_name(t).to_string()).collect();
    });
    crate::ui::refresh();
}

// ---------------------------------------------------------------------------------------------------------------

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
            WM_SIZE => {
                with_app(|a| {
                    clamp_scroll(a);
                    invalidate(a.list);
                });
                LRESULT(0)
            }
            WM_SETFOCUS | WM_KILLFOCUS => {
                invalidate(hwnd);
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                mouse_down(x_of(lp), y_of(lp), false);
                LRESULT(0)
            }
            WM_RBUTTONDOWN => {
                mouse_down(x_of(lp), y_of(lp), true);
                LRESULT(0)
            }
            // a middle click on a folder opens it in a new tab, behind this one
            WM_MBUTTONUP => {
                let folder = with_app(|a| {
                    let g = geo(a);
                    let pos = g.hit(x_of(lp), y_of(lp), a.lv.scroll_y, a.lv.scroll_x, a.model.order.len())?;
                    let i = a.model.order[pos];
                    a.model.entries[i as usize].is_dir().then(|| a.model.path_of(i))
                })
                .flatten();
                if let Some(p) = folder {
                    crate::ui::tabs::open(Loc::Dir(p), false);
                }
                LRESULT(0)
            }
            WM_LBUTTONDBLCLK => {
                let open = with_app(|a| {
                    let g = geo(a);
                    if let Some((k, true)) = header_hit(a, &g, x_of(lp), y_of(lp)) {
                        // double-click a column edge: fit the Name column to its content is left out; reset width
                        let c = g.cols[k].0;
                        a.settings.set_width(c, c.default_width());
                        clamp_scroll(a);
                        invalidate(a.list);
                        return false;
                    }
                    g.hit(x_of(lp), y_of(lp), a.lv.scroll_y, a.lv.scroll_x, a.model.order.len()).is_some()
                });
                if open == Some(true) {
                    cmds::run(Cmd::Open);
                } else if open == Some(false) {
                    mouse_down(x_of(lp), y_of(lp), false);
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                with_app(|a| {
                    let part = crate::ui::bubble::hit(a, x_of(lp), y_of(lp));
                    if let Some(b) = a.bubble.as_mut() {
                        if b.hot != part {
                            b.hot = part;
                            invalidate(a.list);
                        }
                    }
                });
                mouse_move(x_of(lp), y_of(lp));
                LRESULT(0)
            }
            WM_TIMER if wp.0 == crate::ui::bubble::T_BUBBLE => {
                with_app(crate::ui::bubble::tick);
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                with_app(|a| {
                    a.lv.tracking = false;
                    if let Some(b) = a.bubble.as_mut() {
                        b.hot = None;
                    }
                    if a.lv.hover.is_some() || a.lv.header_hot.is_some() || a.lv.bar_hot.is_some() {
                        a.lv.hover = None;
                        a.lv.header_hot = None;
                        a.lv.bar_hot = None;
                        invalidate(a.list);
                    }
                });
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                mouse_up(x_of(lp), y_of(lp));
                LRESULT(0)
            }
            WM_CAPTURECHANGED => {
                with_app(|a| {
                    if !matches!(a.lv.drag, Drag::None) && HWND(lp.0 as *mut _) != a.list {
                        a.lv.drag = Drag::None;
                        invalidate(a.list);
                    }
                });
                LRESULT(0)
            }
            WM_TIMER if wp.0 == T_AUTOSCROLL => {
                autoscroll();
                LRESULT(0)
            }
            WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
                // the "working in the background" pointer while a folder loads or a search runs
                let busy = with_app(|a| a.busy.is_some_and(|(_, end)| end.is_none())).unwrap_or(false);
                SetCursor(Some(LoadCursorW(None, if busy { IDC_APPSTARTING } else { IDC_ARROW }).unwrap_or_default()));
                LRESULT(1)
            }
            WM_TIMER if wp.0 == T_GLIDE => {
                glide_step();
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                wheel(((wp.0 >> 16) & 0xFFFF) as u16 as i16);
                LRESULT(0)
            }
            WM_MOUSEHWHEEL => {
                with_app(|a| {
                    a.lv.scroll_x += ((wp.0 >> 16) & 0xFFFF) as u16 as i16 as i32 / 4;
                    clamp_scroll(a);
                    invalidate(a.list);
                });
                LRESULT(0)
            }
            WM_VSCROLL => {
                scroll_bar(SB_VERT, (wp.0 & 0xFFFF) as u16);
                LRESULT(0)
            }
            WM_HSCROLL => {
                scroll_bar(SB_HORZ, (wp.0 & 0xFFFF) as u16);
                LRESULT(0)
            }
            WM_KEYDOWN => {
                if key(VIRTUAL_KEY(wp.0 as u16)) {
                    return LRESULT(0);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_SYSKEYDOWN if wp.0 as u16 == VK_RETURN.0 => {
                cmds::run(Cmd::Properties);
                LRESULT(0)
            }
            WM_CHAR => {
                if let Some(c) = char::from_u32(wp.0 as u32) {
                    if c != ' ' || with_app(|a| a.lv.typeahead.is_empty()) == Some(false) {
                        typeahead(c);
                    }
                }
                LRESULT(0)
            }
            WM_GETDLGCODE => LRESULT((DLGC_WANTARROWS | DLGC_WANTCHARS) as isize),
            WM_CONTEXTMENU => {
                context_menu(x_of(lp), y_of(lp));
                LRESULT(0)
            }
            WM_DROPFILES => {
                drop_files(HGLOBAL(wp.0 as *mut _));
                LRESULT(0)
            }
            WM_COMMAND => {
                let code = ((wp.0 >> 16) & 0xFFFF) as u32;
                if code == EN_KILLFOCUS {
                    end_rename(true);
                } else if code == EN_CHANGE {
                    rename_resized();
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
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
