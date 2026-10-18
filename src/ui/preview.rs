// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! The preview pane (Alt+P), to the right of the list, laid out as Windows' Details pane: the item's picture across
//! the top (a photo's or video's thumbnail as wide as the pane, else its icon, or the first lines of a text file),
//! its name, its details (type, size, where it is, when it changed, a picture's dimensions, a song's or video's
//! length, a program's version) and a Properties button. With nothing selected it shows the folder itself; with
//! several items, how many and their size. The first lines and the details that need the file opened are read on a
//! thread, at most 256 KB, and never from a cloud file that is not on this PC.
use crate::ui::art::{Art, Tool};
use crate::ui::gfx::{self, Surface, R};
use crate::ui::model::Loc;
use crate::ui::{client_size, invalidate, post_box, with_app, x_of, y_of, App};
use dvfiles::fsx::Entry;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WM_PREVIEW_TEXT: u32 = WM_APP + 24;

#[derive(Default)]
pub struct State {
    // what the pane shows: a path, or a count of selected items; repainted when it changes
    pub subject: String,
    // what was read from a file, and the path being read
    facts: Option<Box<Text>>,
    reading: Option<String>,
    mono: Option<HFONT>,
    // dragging the left edge: where it started and the width then
    drag: Option<(i32, i32)>,
    // the Properties button: where it was drawn, the pointer over it, pressed on it
    button: Option<R>,
    hot: bool,
    pressed: bool,
    tracking: bool,
}

/// what was read from a file on the thread: its first lines (a text file) and the details only the file knows
pub struct Text {
    path: String,
    text: Option<String>,
    rows: Vec<(String, String)>,
}

const TEXT_EXT: [&str; 40] = [
    "txt",
    "md",
    "log",
    "ini",
    "cfg",
    "conf",
    "json",
    "xml",
    "csv",
    "tsv",
    "yaml",
    "yml",
    "toml",
    "rs",
    "py",
    "js",
    "ts",
    "jsx",
    "tsx",
    "html",
    "htm",
    "css",
    "c",
    "h",
    "cpp",
    "hpp",
    "cs",
    "java",
    "kt",
    "go",
    "sh",
    "ps1",
    "bat",
    "cmd",
    "sql",
    "lua",
    "rb",
    "php",
    "gitignore",
    "reg",
];

/// the pane's width in pixels, or 0 when it is off or there is nothing it could show
pub fn width(a: &App, total: i32) -> i32 {
    if !a.settings.preview || a.model.loc == Loc::Pc {
        return 0;
    }
    a.s(a.settings.preview_width as f32).min(total / 2)
}

/// the subject for the current selection: one item's path, or how many are selected
fn subject(a: &App) -> String {
    let sel = a.model.selected();
    match sel.len() {
        0 => String::new(),
        1 => a.model.path_of(sel[0]),
        n => format!("<{n}>"),
    }
}

/// called after the list paints: the pane follows the selection
pub fn follow(a: &mut App) {
    if a.preview.0.is_null() || !a.settings.preview {
        return;
    }
    let s = subject(a);
    if s != a.pv.subject {
        a.pv.subject = s;
        invalidate(a.preview);
    }
}

/// reads, on a thread, a text file's first lines and what only the file itself knows: a picture's dimensions, a
/// song's or video's length, a program's version
fn read_facts(hwnd: HWND, path: String, text: bool) {
    let h = hwnd.0 as isize;
    std::thread::spawn(move || {
        use std::io::Read;
        let ext = dvfiles::naming::ext_lower(&path);
        let mut head = Vec::new();
        if let Ok(f) = std::fs::File::open(&path) {
            let _ = f.take(256 << 10).read_to_end(&mut head);
        }
        let text = text.then(|| {
            let buf = &head[..head.len().min(16 * 1024)];
            let s = if buf.starts_with(&[0xFF, 0xFE]) {
                let units: Vec<u16> = buf[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
                String::from_utf16_lossy(&units)
            } else {
                String::from_utf8_lossy(buf.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(buf)).into_owned()
            };
            // control characters other than line breaks and tabs would draw as boxes
            s.chars().map(|c| if c.is_control() && c != '\n' && c != '\t' { ' ' } else { c }).collect()
        });
        let mut rows = Vec::new();
        if dvfiles::img::readable(&ext) {
            if let Some((w, h)) = dvfiles::img::dimensions(&head) {
                rows.push(("Dimensions".to_string(), format!("{w} x {h}")));
            }
        }
        rows.extend(crate::ui::details::details(&path));
        post_box(HWND(h as *mut _), WM_PREVIEW_TEXT, Text { path, text, rows });
    });
}

/// what the band at the top shows
enum Band {
    Text(String),
    Picture(Entry, String),
    // several items: the first one's picture with their count, as on a drag
    Stack(Entry, String, usize),
}

/// what the pane is about
enum Subject {
    One(Entry, String),
    Many(Vec<u32>),
    Folder(String),
    Nothing,
}

/// the card's edge along the top, the right and the foot, its right corners rounded, and the pane to the screen
fn finish(a: &App, w: i32, h: i32) {
    let t = a.theme;
    let surf = a.surf_preview.as_ref().unwrap();
    surf.set_clip(None);
    surf.fill(R::new(0, 0, w, 1), t.card_edge);
    surf.fill(R::new(0, h - 1, w, 1), t.card_border);
    surf.fill(R::new(w - 1, 0, 1, h), t.card_border);
    crate::ui::card_corners(a, surf, a.preview, w, h, 2 | 4);
    surf.present(a.preview);
}

pub fn paint(a: &mut App) {
    let (w, h) = client_size(a.preview);
    if w <= 0 || Surface::ensure(&mut a.surf_preview, w, h).is_none() {
        return;
    }
    let t = a.theme;
    {
        let surf = a.surf_preview.as_ref().unwrap();
        surf.fill(R::new(0, 0, w, h), t.bg);
        // the right part of the card the list is on, set off from it by a faint divider
        surf.blend(R::new(0, a.s(12.0), 1, (h - a.s(24.0)).max(0)), t.fg, 0.10);
    }
    let sel = a.model.selected();
    let subject = match sel.len() {
        0 => a.model.loc.path().map_or(Subject::Nothing, |p| Subject::Folder(p.to_string())),
        1 => Subject::One(a.model.entries[sel[0] as usize].clone(), a.model.path_of(sel[0])),
        _ => Subject::Many(sel.clone()),
    };
    a.pv.button = None;
    if let Subject::Nothing = subject {
        let surf = a.surf_preview.as_ref().unwrap();
        surf.text(
            R::new(a.s(18.0), h / 3, (w - a.s(36.0)).max(0), a.s(48.0)),
            "Select a file to preview.",
            a.fonts.ui,
            t.muted,
            gfx::WRAP_CENTER,
        );
        finish(a, w, h);
        return;
    }

    let m = a.s(22.0);
    let inner_w = (w - 2 * m).max(0);

    // what the band shows, and the name and details below it
    let (name, small, rows, shows): (
        String,
        Option<std::rc::Rc<crate::ui::thumbs::Image>>,
        Vec<(String, String)>,
        Band,
    ) = match &subject {
        Subject::One(e, path) => {
            let ext = dvfiles::naming::ext_lower(&e.name);
            let cloud = dvfiles::fsx::is_cloud_only(e.attrs);
            let is_text = !e.is_dir() && TEXT_EXT.contains(&ext.as_str()) && !cloud;
            let facts = a.pv.facts.as_ref().filter(|f| f.path == *path);
            let text = facts.and_then(|f| f.text.clone());
            let extra: Vec<(String, String)> = facts.map(|f| f.rows.clone()).unwrap_or_default();
            if facts.is_none() && !e.is_dir() && !cloud && a.pv.reading.as_deref() != Some(path.as_str()) {
                a.pv.reading = Some(path.clone());
                read_facts(a.main, path.clone(), is_text);
            }
            let shows = match text.filter(|_| is_text) {
                Some(text) => Band::Text(text),
                None => Band::Picture(e.clone(), path.clone()),
            };
            let small = (!e.is_dir()).then(|| crate::ui::list::icon_image(a, e, path, a.s(24.0)).0);
            let mut rows = vec![("Type".to_string(), a.icons.type_name(e).to_string())];
            if !e.is_dir() {
                rows.push(("Size".into(), a.nf.bytes(e.size)));
            }
            if let Some(dir) = dvfiles::pathx::parent(path) {
                rows.push(("File location".into(), dir));
            }
            rows.push(("Date modified".into(), crate::ui::locale::short(e.modified)));
            rows.extend(extra);
            if cloud {
                rows.push(("Status".into(), "Available online only".into()));
            }
            (e.name.to_string(), small, rows, shows)
        }
        Subject::Many(sel) => {
            let first = sel[0];
            let e = a.model.entries[first as usize].clone();
            let path = a.model.path_of(first);
            let mut rows = Vec::new();
            if sel.iter().all(|&i| !a.model.entries[i as usize].is_dir()) {
                let total: u64 = sel.iter().map(|&i| a.model.entries[i as usize].size).sum();
                rows.push(("Size".to_string(), a.nf.bytes(total)));
            }
            let dirs: Vec<Option<String>> =
                sel.iter().take(2000).map(|&i| dvfiles::pathx::parent(&a.model.path_of(i))).collect();
            if let Some(Some(d)) = dirs.first().filter(|d| dirs.iter().all(|x| x == *d)) {
                rows.push(("File location".to_string(), d.clone()));
            }
            (format!("{} items selected", a.nf.group(sel.len() as u64)), None, rows, Band::Stack(e, path, sel.len()))
        }
        Subject::Folder(path) => {
            let e = dvfiles::fsx::stat(path).unwrap_or(Entry {
                name: dvfiles::pathx::file_name(path).into(),
                dir: None,
                attrs: dvfiles::fsx::FA_DIRECTORY,
                size: 0,
                modified: 0,
                created: 0,
            });
            let mut rows = vec![(
                "Type".to_string(),
                if path.len() <= 3 { "Local Disk".to_string() } else { "File folder".to_string() },
            )];
            if let Some(dir) = dvfiles::pathx::parent(path) {
                rows.push(("File location".into(), dir));
            }
            if e.modified != 0 {
                rows.push(("Date modified".into(), crate::ui::locale::short(e.modified)));
            }
            rows.push(("Items".into(), a.nf.group(a.model.order.len() as u64)));
            (a.display_name(path), None, rows, Band::Picture(e, path.clone()))
        }
        Subject::Nothing => unreachable!(),
    };

    // the band across the top, about as tall as Windows' for the pane's width, and shorter when the window is too
    // short for it and everything below (but never under two fifths of the pane: the picture comes first)
    let line = a.s(26.0);
    let rh = a.s(22.0);
    let below =
        a.s(24.0) + line + a.s(24.0) + (rh + a.s(8.0)) + rows.len() as i32 * rh + a.s(16.0) + a.s(30.0) + a.s(12.0);
    let natural = (w as f32 * 0.72) as i32;
    let band_h = natural.min(h - below).max((h * 2 / 5).min(natural)).max(a.s(120.0));
    // a rounded well inside the card, a shade off its face
    let inset = a.s(12.0);
    let band = R::new(inset, inset, (w - 2 * inset).max(0), (band_h - inset).max(0));
    let well = t.bg.mix(t.fg, if t.dark { 0.045 } else { 0.035 });
    a.surf_preview.as_ref().unwrap().fill(band, well);
    match shows {
        Band::Text(text) => {
            let font = *a.pv.mono.get_or_insert_with(|| mono_font(a.scale));
            let area = band.inset(a.s(14.0), a.s(12.0));
            let surf = a.surf_preview.as_ref().unwrap();
            surf.set_clip(Some(area));
            let flags = DRAW_TEXT_FORMAT(DT_LEFT.0 | DT_TOP.0 | DT_NOPREFIX.0 | DT_EXPANDTABS.0);
            surf.text(area, &text, font, t.fg, flags);
            surf.set_clip(None);
        }
        Band::Picture(e, path) => band_picture(a, band, &e, &path),
        Band::Stack(e, path, n) => {
            if let Some(img) = crate::ui::drag::picture_of(a, &e, &path, n) {
                a.surf_preview.as_ref().unwrap().image_in(band, &img);
            }
        }
    }
    // a photo filling the well takes its rounded corners too
    let surf = a.surf_preview.as_ref().unwrap();
    surf.set_clip(None);
    surf.round_corners(band, a.rad_card(), 15, None, &|s, r| s.fill(r, t.bg));

    // the name, with the item's small icon before it
    let mut y = band_h + a.s(24.0);
    let mut nx = m;
    if let Some(icon) = &small {
        let surf = a.surf_preview.as_ref().unwrap();
        surf.image(m, y + (line - icon.h) / 2, icon.w, icon.h, &icon.px);
        nx += icon.w + a.s(12.0);
    }
    let surf = a.surf_preview.as_ref().unwrap();
    surf.text(R::new(nx, y, (w - m - nx).max(0), line), &name, a.fonts.title, t.fg, gfx::LEFT);
    y += line + a.s(24.0);

    // Details: what it is on the left, its value on the right
    if !rows.is_empty() && y + rh < h {
        surf.text(R::new(m, y, inner_w, rh), "Details", a.fonts.bold, t.fg, gfx::LEFT);
        y += rh + a.s(8.0);
    }
    let label_w = rows.iter().map(|(l, _)| gfx::text_width(surf.dc, l, a.fonts.ui)).max().unwrap_or(0) + a.s(16.0);
    for (label, value) in &rows {
        if y + rh > h - a.s(8.0) {
            break;
        }
        surf.text(R::new(m, y, label_w, rh), label, a.fonts.ui, t.fg, gfx::LEFT);
        let vx = m + label_w;
        let flags = if label == "File location" { path_right() } else { gfx::RIGHT };
        surf.text(R::new(vx, y, (w - m - vx).max(0), rh), value, a.fonts.ui, t.fg, flags);
        y += rh;
    }

    // Properties, as a button
    y += a.s(16.0);
    let icon_side = a.s(16.0);
    let label = "Properties";
    let bw = a.s(12.0) + icon_side + a.s(8.0) + gfx::text_width(surf.dc, label, a.fonts.ui) + a.s(14.0);
    let bh = a.s(30.0);
    if y + bh <= h - a.s(8.0) {
        let b = R::new(m, y, bw, bh);
        let fill = if a.pv.pressed && a.pv.hot {
            t.btn_press
        } else if a.pv.hot {
            t.btn_hover
        } else if t.dark {
            t.chrome_hover.mix(t.bg, 0.25)
        } else {
            t.chrome
        };
        surf.fill(b, fill);
        if !t.dark {
            surf.frame(b, t.input_border);
        }
        let icon = a.art.get(Art::Tool(Tool::Props), icon_side, &t);
        let surf = a.surf_preview.as_ref().unwrap();
        surf.image(b.x + a.s(12.0), b.y + (bh - icon.h) / 2, icon.w, icon.h, &icon.px);
        let tx = b.x + a.s(12.0) + icon_side + a.s(8.0);
        surf.text(R::new(tx, b.y, b.right() - tx, bh - a.s(1.0)), label, a.fonts.ui, t.fg, gfx::LEFT);
        a.pv.button = Some(b);
    }
    finish(a, w, h);
}

/// a folder's path on the right, shortened at its end as Windows' Details pane does
fn path_right() -> DRAW_TEXT_FORMAT {
    DRAW_TEXT_FORMAT(DT_RIGHT.0 | DT_VCENTER.0 | DT_SINGLELINE.0 | DT_END_ELLIPSIS.0 | DT_NOPREFIX.0)
}

/// the item's picture in the band: a photo's or video's thumbnail as large as the band allows (as wide as the pane
/// for a landscape photo), else its icon at Large icons' size
fn band_picture(a: &mut App, band: R, e: &Entry, path: &str) {
    let t = a.theme;
    let pad = a.s(14.0);
    let ext = dvfiles::naming::ext_lower(&e.name);
    // in steps, so a resize does not ask for a new thumbnail at each pixel
    let step = |v: i32| (v / 32 * 32).clamp(a.s(48.0), 1024);
    let pictured = !e.is_dir() && crate::ui::thumbs::pictured(&ext);
    let size = if pictured { (step(band.w), step(band.h - 2 * pad)) } else { (a.s(96.0), a.s(96.0)) };
    match crate::ui::list::picture(a, e, path, size) {
        Some(pic) => {
            let img = &pic.img;
            let (ix, iy) = (band.x + (band.w - img.w) / 2, band.y + (band.h - img.h) / 2);
            let surf = a.surf_preview.as_ref().unwrap();
            surf.image(ix, iy, img.w, img.h, &img.px);
            if let Some(l) = &pic.label {
                crate::ui::list::band_text(a, surf, ix, iy, img.w, l);
            }
            if pic.arrow {
                let w = img.w;
                crate::ui::list::draw_arrow(a, |a| a.surf_preview.as_ref(), ix, iy, w);
            }
        }
        None => {
            let art = if e.is_dir() { Art::Folder } else { Art::File(crate::ui::art::Kind::Generic) };
            let img = a.art.get(art, a.s(96.0), &t);
            a.surf_preview.as_ref().unwrap().image_in(band, &img);
        }
    }
}

fn mono_font(scale: f32) -> HFONT {
    unsafe {
        CreateFontW(
            -((9.0 * 96.0 / 72.0 * scale).round() as i32),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            windows::core::w!("Consolas"),
        )
    }
}

/// the fonts made for the pane (the text font follows the DPI)
pub fn reset_fonts(a: &mut App) {
    if let Some(f) = a.pv.mono.take() {
        unsafe {
            let _ = DeleteObject(f.into());
        }
    }
}

fn on_edge(a: &App, x: i32) -> bool {
    x < a.s(6.0)
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
            WM_SIZE => {
                invalidate(hwnd);
                LRESULT(0)
            }
            WM_SETCURSOR if (lp.0 & 0xFFFF) as u32 == HTCLIENT => {
                let mut p = POINT::default();
                let _ = GetCursorPos(&mut p);
                let _ = ScreenToClient(hwnd, &mut p);
                let edge = with_app(|a| on_edge(a, p.x) || a.pv.drag.is_some()).unwrap_or(false);
                SetCursor(Some(LoadCursorW(None, if edge { IDC_SIZEWE } else { IDC_ARROW }).unwrap_or_default()));
                LRESULT(1)
            }
            WM_LBUTTONDOWN => {
                let (x, y) = (x_of(lp), y_of(lp));
                let mut p = POINT { x, y };
                let _ = ClientToScreen(hwnd, &mut p);
                let start = with_app(|a| {
                    if on_edge(a, x) {
                        a.pv.drag = Some((p.x, a.settings.preview_width));
                        true
                    } else if a.pv.button.is_some_and(|b| b.contains(x, y)) {
                        a.pv.pressed = true;
                        a.pv.hot = true;
                        invalidate(a.preview);
                        true
                    } else {
                        false
                    }
                });
                if start == Some(true) {
                    SetCapture(hwnd);
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let (x, y) = (x_of(lp), y_of(lp));
                let mut p = POINT { x, y };
                let _ = ClientToScreen(hwnd, &mut p);
                let moved = with_app(|a| {
                    if !a.pv.tracking {
                        let mut tme = TRACKMOUSEEVENT {
                            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_LEAVE,
                            hwndTrack: hwnd,
                            dwHoverTime: 0,
                        };
                        let _ = TrackMouseEvent(&mut tme);
                        a.pv.tracking = true;
                    }
                    let hot = a.pv.drag.is_none() && a.pv.button.is_some_and(|b| b.contains(x, y));
                    if hot != a.pv.hot {
                        a.pv.hot = hot;
                        invalidate(a.preview);
                    }
                    let (x0, w0) = a.pv.drag?;
                    // dragging left widens the pane
                    let w = w0 + ((x0 - p.x) as f32 / a.scale) as i32;
                    a.settings.preview_width = w.clamp(180, 900);
                    Some(())
                })
                .flatten();
                if moved.is_some() {
                    crate::ui::layout();
                }
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                with_app(|a| {
                    a.pv.tracking = false;
                    if a.pv.hot {
                        a.pv.hot = false;
                        invalidate(a.preview);
                    }
                });
                LRESULT(0)
            }
            WM_LBUTTONUP | WM_CAPTURECHANGED => {
                let (x, y) = (x_of(lp), y_of(lp));
                let clicked = with_app(|a| {
                    a.pv.drag = None;
                    let was = std::mem::take(&mut a.pv.pressed);
                    if was {
                        invalidate(a.preview);
                    }
                    was && msg == WM_LBUTTONUP && a.pv.button.is_some_and(|b| b.contains(x, y))
                })
                .unwrap_or(false);
                if msg == WM_LBUTTONUP {
                    let _ = ReleaseCapture();
                }
                if clicked {
                    // the selection's, or with nothing selected the folder's
                    crate::ui::cmds::run(crate::ui::cmds::Cmd::Properties);
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// a file's first lines and details have been read
pub fn on_text(t: Box<Text>) {
    with_app(|a| {
        if a.pv.reading.as_deref() == Some(t.path.as_str()) {
            a.pv.reading = None;
        }
        a.pv.facts = Some(t);
        invalidate(a.preview);
    });
}
