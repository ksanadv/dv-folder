// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Settings (Ctrl+, or ... > Settings), drawn by dvfiles: one page of switches, each on a card of its own, each taking
//! effect at once and saved at once. Every look and every everyday feature dvfiles adds can be turned off here, and
//! off means the window behaves as it did without it.
use crate::settings::ThemeMode;
use crate::ui::cmds::{self, Cmd};
use crate::ui::gfx::{self, Surface, R};
use crate::ui::{invalidate, with_app, wstr, x_of, y_of, App};
use std::cell::RefCell;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, DRAW_TEXT_FORMAT, DT_LEFT, DT_NOPREFIX, DT_WORDBREAK, PAINTSTRUCT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// one switch
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Glass,
    AppIcons,
    Hidden,
    Ext,
    FolderSizes,
    SearchIndex,
    DefaultHandler,
    PasteFiles,
    UndoBubble,
    Animations,
    Viewer,
    Inbox,
    CopyQueue,
    Spring,
}

struct Row {
    key: Key,
    title: &'static str,
    about: &'static str,
}

const SECTIONS: &[(&str, &[Row])] = &[
    (
        "Look",
        &[
            Row { key: Key::Glass, title: "Glass", about: "The toolbar, the left pane and the status bar show what is behind the window, blurred and darkened by dvfiles. The file list stays solid. While it is on, Windows leaves dvfiles out of screenshots and screen sharing." },
            Row { key: Key::AppIcons, title: "Programs' icons for their files", about: "Files show the icon their program gives them (VLC, Word, dvnote), as in Explorer. Off: dvfiles' own icons for every type." },
            Row { key: Key::Animations, title: "Animations", about: "Things slide in and settle softly: the undo bubble, panes, menus. Windows' own \"Show animations\" setting still applies." },
        ],
    ),
    (
        "Files and folders",
        &[
            Row { key: Key::Hidden, title: "Hidden items", about: "Show files and folders marked hidden." },
            Row { key: Key::Ext, title: "File name extensions", about: "Show the end of a file's name that tells its type, such as .jpg." },
            Row { key: Key::FolderSizes, title: "Folder sizes", about: "Measure each folder in the background and show its size in the Size column." },
            Row { key: Key::SearchIndex, title: "Fast search index", about: "Keep an index of names on each drive, so a search of a whole drive takes under a second." },
            Row { key: Key::DefaultHandler, title: "Open folders with dvfiles", about: "Folders and drives open in dvfiles instead of File Explorer. Turning it off puts Explorer back." },
        ],
    ),
    (
        "Everyday",
        &[
            Row { key: Key::PasteFiles, title: "Paste pictures and text as files", about: "Ctrl+V with a screenshot or picture on the clipboard saves it as a PNG in the folder; a web address becomes a shortcut, other text a .txt file." },
            Row { key: Key::UndoBubble, title: "Undo bubble", about: "After a rename, move, copy or delete, a small card at the bottom of the list says what happened, with Undo, for a few seconds." },
            Row { key: Key::Spring, title: "Folders open as you drag over them", about: "Hold dragged files over a folder for a moment and it opens, in the list or in the left pane, so you can drop them deeper without letting go." },
            Row { key: Key::CopyQueue, title: "Copy one at a time", about: "Copies (and moves to another drive) started while one runs wait their turn, so the disk does one thing at full speed instead of several slowly." },
            Row { key: Key::Inbox, title: "New downloads marked", about: "In Downloads, files that arrived since you last looked there have a small dot, and the status bar says how many are new." },
            Row { key: Key::Viewer, title: "Open pictures in dvfiles' viewer", about: "JPEG, PNG, GIF and BMP pictures open in dvfiles' own quick viewer: Left and Right go through the folder, F11 fills the screen, Esc closes. Off: they open in their own program." },
        ],
    ),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hit {
    Switch(Key),
    Theme(ThemeMode),
}

#[derive(Default)]
struct Win {
    hwnd: HWND,
    surface: Option<Surface>,
    hits: Vec<(Hit, R)>,
    hot: Option<Hit>,
    scroll: i32,
    content_h: i32,
}

thread_local! {
    static WIN: RefCell<Option<Win>> = const { RefCell::new(None) };
}

fn with_win<T>(f: impl FnOnce(&mut Win) -> T) -> Option<T> {
    WIN.with(|w| w.try_borrow_mut().ok()?.as_mut().map(f))
}

fn get(a: &App, k: Key) -> bool {
    match k {
        Key::Glass => a.settings.glass,
        Key::AppIcons => a.settings.app_icons,
        Key::Hidden => a.model.show_hidden,
        Key::Ext => a.settings.show_ext,
        Key::FolderSizes => a.settings.folder_sizes,
        Key::SearchIndex => a.settings.search_index,
        Key::DefaultHandler => crate::install::is_default_handler(),
        Key::PasteFiles => a.settings.paste_files,
        Key::UndoBubble => a.settings.undo_bubble,
        Key::Animations => a.settings.animations,
        Key::Viewer => a.settings.viewer,
        Key::Inbox => a.settings.inbox,
        Key::CopyQueue => a.settings.copy_queue,
        Key::Spring => a.settings.spring,
    }
}

/// turns a switch on or off, takes effect at once and is saved
fn set(k: Key, on: bool) {
    if with_app(|a| get(a, k)) == Some(on) {
        return;
    }
    match k {
        Key::Hidden => cmds::run(Cmd::ToggleHidden),
        Key::Ext => cmds::run(Cmd::ToggleExt),
        Key::FolderSizes => cmds::run(Cmd::ToggleFolderSizes),
        Key::SearchIndex => cmds::run(Cmd::ToggleSearchIndex),
        Key::DefaultHandler => cmds::run(Cmd::DefaultHandler(on)),
        Key::Glass => {
            with_app(|a| a.settings.glass = on);
            crate::ui::glass_setting_changed();
        }
        Key::PasteFiles => {
            with_app(|a| {
                a.settings.paste_files = on;
                invalidate(a.main);
            });
        }
        Key::UndoBubble => {
            with_app(|a| {
                a.settings.undo_bubble = on;
                if !on {
                    crate::ui::bubble::hide(a);
                }
            });
        }
        Key::Animations => {
            with_app(|a| a.settings.animations = on);
        }
        Key::Viewer => {
            with_app(|a| {
                a.settings.viewer = on;
            });
        }
        Key::Inbox => {
            with_app(|a| {
                a.settings.inbox = on;
                invalidate(a.list);
            });
        }
        Key::CopyQueue => {
            with_app(|a| {
                a.settings.copy_queue = on;
            });
        }
        Key::Spring => {
            with_app(|a| {
                a.settings.spring = on;
            });
        }
        Key::AppIcons => {
            with_app(|a| {
                a.settings.app_icons = on;
                a.icons.clear_own();
                invalidate(a.list);
                invalidate(a.preview);
            });
        }
    }
    with_app(|a| a.settings.save());
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
            lpszClassName: w!("dvfilesSettings"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
    }
}

fn dark_frame(hwnd: HWND) {
    let Some(t) = with_app(|a| a.theme) else { return };
    let dark = t.dark as i32;
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const i32 as *const _, 4);
    }
    crate::ui::tint_frame(hwnd, t.chrome, t.fg, t.frame);
}

/// the theme changed: the window follows
pub fn retheme() {
    if let Some(h) = with_win(|w| w.hwnd) {
        dark_frame(h);
        invalidate(h);
    }
}

/// opens the Settings window, or brings it forward if it is open
pub fn open() {
    if let Some(h) = with_win(|w| w.hwnd) {
        unsafe {
            let _ = ShowWindow(h, SW_RESTORE);
            let _ = SetForegroundWindow(h);
        }
        return;
    }
    let Some((owner, scale)) = with_app(|a| (a.main, a.scale)) else { return };
    let (w, h) = ((560.0 * scale) as i32, (640.0 * scale) as i32);
    let hwnd = unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let mut r = RECT { left: 0, top: 0, right: w, bottom: h };
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let _ = AdjustWindowRectEx(&mut r, style, false, WINDOW_EX_STYLE(0));
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("dvfilesSettings"),
            PCWSTR(wstr("Settings").as_ptr()),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            r.right - r.left,
            r.bottom - r.top,
            Some(owner),
            None,
            Some(hinst.into()),
            None,
        )
        .unwrap_or_default()
    };
    WIN.with(|x| *x.borrow_mut() = Some(Win { hwnd, ..Default::default() }));
    dark_frame(hwnd);
    unsafe {
        let (mut mr, mut wr) = (RECT::default(), RECT::default());
        let _ = GetWindowRect(owner, &mut mr);
        let _ = GetWindowRect(hwnd, &mut wr);
        let (ww, wh) = (wr.right - wr.left, wr.bottom - wr.top);
        let _ = SetWindowPos(
            hwnd,
            None,
            mr.left + (mr.right - mr.left - ww) / 2,
            mr.top + (mr.bottom - mr.top - wh) / 3,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER,
        );
        let quiet = std::env::var_os("DVFILES_NOACTIVATE").is_some();
        let _ = ShowWindow(hwnd, if quiet { SW_SHOWNOACTIVATE } else { SW_SHOW });
    }
}

/// a switch: a pill, filled with the accent and its knob on the right when on
fn switch(a: &App, surf: &Surface, r: R, on: bool, hot: bool) {
    let t = a.theme;
    let rad = r.h as f32 / 2.0;
    let knob = r.h - a.s(10.0);
    let ky = r.y + (r.h - knob) / 2;
    if on {
        let fill = if hot { t.accent.mix(t.fg, 0.15) } else { t.accent };
        surf.round(r, rad, fill);
        let kc = t.primary_fg;
        surf.round(R::new(r.right() - a.s(5.0) - knob, ky, knob, knob), knob as f32 / 2.0, kc);
    } else {
        let border = if hot { t.fg.mix(t.bg, 0.15) } else { t.fg.mix(t.bg, 0.40) };
        surf.round_ring(r, rad, border);
        surf.round(R::new(r.x + a.s(5.0), ky, knob, knob), knob as f32 / 2.0, border);
    }
}

fn paint(hwnd: HWND) {
    let (cw, ch) = crate::ui::client_size(hwnd);
    let Some((scroll, hot)) = with_win(|w| (w.scroll, w.hot)) else { return };
    let mut surface = with_win(|w| w.surface.take()).flatten();
    if Surface::ensure(&mut surface, cw, ch).is_none() {
        return;
    }
    let surf = surface.as_ref().unwrap();
    let mut hits = Vec::new();
    let content_h = with_app(|a| {
        let t = a.theme;
        let s = |v: f32| a.s(v);
        let wrap = DRAW_TEXT_FORMAT(DT_LEFT.0 | DT_WORDBREAK.0 | DT_NOPREFIX.0);
        // the window is the frame; each setting a card on it
        surf.fill(R::new(0, 0, cw, ch), t.chrome);
        let m = s(28.0);
        let rad = a.rad_card();
        let card = |r: R, over: bool| {
            surf.round(r, rad, t.card_border);
            surf.round(r.inset(1, 1), rad - 1.0, if over { t.bg.mix(t.fg, 0.035) } else { t.bg });
        };
        let mut y = s(20.0) - scroll;
        surf.text(R::new(m, y, cw - 2 * m, s(40.0)), "Settings", a.fonts.big, t.fg, gfx::LEFT);
        y += s(52.0);

        // the theme, as three buttons side by side
        surf.text(R::new(m, y, cw - 2 * m, s(22.0)), "Theme", a.fonts.bold, t.muted, gfx::LEFT);
        y += s(30.0);
        let modes = [(ThemeMode::System, "Windows' setting"), (ThemeMode::Light, "Light"), (ThemeMode::Dark, "Dark")];
        let bw = (cw - 2 * m) / 3;
        for (k, (mode, label)) in modes.iter().enumerate() {
            let r = R::new(m + k as i32 * bw, y, bw - s(6.0), s(34.0));
            let chosen = a.settings.theme == *mode;
            let over = hot == Some(Hit::Theme(*mode));
            if chosen {
                surf.round(r, a.rad(), t.accent);
                surf.round(r.inset(1, 1), a.rad() - 1.0, t.sel);
            } else {
                surf.round(r, a.rad(), t.card_border);
                surf.round(r.inset(1, 1), a.rad() - 1.0, if over { t.bg.mix(t.fg, 0.035) } else { t.bg });
            }
            surf.text(r, label, a.fonts.ui, t.fg, gfx::CENTER);
            hits.push((Hit::Theme(*mode), r));
        }
        y += s(34.0) + s(22.0);

        for (heading, rows) in SECTIONS {
            surf.text(R::new(m, y, cw - 2 * m, s(22.0)), heading, a.fonts.bold, t.muted, gfx::LEFT);
            y += s(28.0);
            for row in rows.iter() {
                let text_w = cw - 2 * m - s(76.0);
                let about_h = {
                    let mut rc = RECT { left: 0, top: 0, right: text_w, bottom: 0 };
                    unsafe {
                        let old = windows::Win32::Graphics::Gdi::SelectObject(surf.dc, a.fonts.ui.into());
                        let mut buf = wstr(row.about);
                        buf.pop();
                        windows::Win32::Graphics::Gdi::DrawTextW(
                            surf.dc,
                            &mut buf,
                            &mut rc,
                            DRAW_TEXT_FORMAT(wrap.0 | windows::Win32::Graphics::Gdi::DT_CALCRECT.0),
                        );
                        windows::Win32::Graphics::Gdi::SelectObject(surf.dc, old);
                    }
                    rc.bottom
                };
                let rh = s(14.0) + s(22.0) + about_h + s(14.0);
                let r = R::new(m - s(12.0), y, cw - 2 * m + s(24.0), rh);
                let over = hot == Some(Hit::Switch(row.key));
                card(r, over);
                surf.text(R::new(m, y + s(12.0), text_w, s(22.0)), row.title, a.fonts.ui, t.fg, gfx::LEFT);
                surf.text(R::new(m, y + s(34.0), text_w, about_h), row.about, a.fonts.ui, t.muted, wrap);
                let sw = R::new(cw - m - s(44.0), y + (rh - s(22.0)) / 2, s(44.0), s(22.0));
                switch(a, surf, sw, get(a, row.key), over);
                hits.push((Hit::Switch(row.key), r));
                y += rh + s(4.0);
            }
            y += s(18.0);
        }
        y + scroll + s(12.0)
    })
    .unwrap_or(ch);
    surf.present(hwnd);
    with_win(|w| {
        w.surface = surface;
        w.hits = hits;
        w.content_h = content_h;
    });
}

fn hit_at(x: i32, y: i32) -> Option<Hit> {
    with_win(|w| w.hits.iter().find(|(_, r)| r.contains(x, y)).map(|(h, _)| *h)).flatten()
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
                let hot = hit_at(x_of(lp), y_of(lp));
                if with_win(|w| std::mem::replace(&mut w.hot, hot) != hot) == Some(true) {
                    invalidate(hwnd);
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                match hit_at(x_of(lp), y_of(lp)) {
                    Some(Hit::Switch(k)) => {
                        let on = with_app(|a| get(a, k)).unwrap_or(false);
                        set(k, !on);
                    }
                    Some(Hit::Theme(mode)) => cmds::run(Cmd::Theme(mode)),
                    None => {}
                }
                invalidate(hwnd);
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = ((wp.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                let (_, ch) = crate::ui::client_size(hwnd);
                let step = with_app(|a| a.s(48.0)).unwrap_or(48);
                with_win(|w| w.scroll = (w.scroll - delta * step / 120).clamp(0, (w.content_h - ch).max(0)));
                invalidate(hwnd);
                LRESULT(0)
            }
            WM_KEYDOWN if wp.0 as u16 == windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE.0 => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                WIN.with(|x| *x.borrow_mut() = None);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
