// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Properties, drawn by dvfiles, with Windows' three main tabs: General (a folder's size, size on disk and contents
//! counted by the parallel walker while the window is open, metadata only), Details (a program's version, a
//! picture's size, a song's length: see details.rs) and Security (the owner and who may do what, read-only).
//! Changing permissions and Previous versions open Windows' own sheet.
use crate::ui::gfx::{self, Surface, R};
use crate::ui::{invalidate, locale, shell, with_app, wstr, x_of, y_of};
use dvfiles::fsx::{self, Drive, FA_DIRECTORY, FA_HIDDEN, FA_READONLY};
use dvfiles::walker::{LiveTotals, Totals};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, SetBkColor, SetTextColor, HDC, PAINTSTRUCT};
use windows::Win32::Storage::FileSystem::{
    GetCompressedFileSizeW, GetFileAttributesExW, GetFileExInfoStandard, WIN32_FILE_ATTRIBUTE_DATA,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::EM_SETREADONLY;
use windows::Win32::UI::Shell::{AssocQueryStringW, ASSOCF_NONE, ASSOCSTR_FRIENDLYAPPNAME};
use windows::Win32::UI::WindowsAndMessaging::*;

const T_MEASURE: usize = 1;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Page {
    General,
    Details,
    Security,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Btn {
    Tab(Page),
    Windows,
    Ok,
    Cancel,
    Apply,
    ReadOnly,
    Hidden,
}

// checkbox state for several items: on, off, or mixed
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tri {
    On,
    Off,
    Mixed,
}

struct Props {
    paths: Vec<String>,
    drive: Option<Drive>,
    is_dir: bool,
    name: String,
    type_line: String,
    opens_with: String,
    location: String,
    times: Option<(u64, u64, u64)>,
    own_bytes: u64,
    own_disk: u64,
    files: u64,
    dirs: u64,
    live: Arc<LiveTotals>,
    measuring: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    readonly: Tri,
    hidden: Tri,
    readonly0: Tri,
    hidden0: Tri,
    edit: Option<HWND>,
    // a program's, shortcut's or icon file's path, whose own icon is shown
    own_path: Option<String>,
    // dvfiles' own icon, for all but programs and shortcuts
    art: Option<crate::ui::art::Art>,
    hot: Option<Btn>,
    hits: Vec<(Btn, R)>,
    surface: Option<Surface>,
    page: Page,
    // read when their tab is first shown
    details: Option<Vec<(String, String)>>,
    security: Option<Option<crate::ui::details::Security>>,
}

impl Drop for Props {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

thread_local! {
    static WINDOWS: RefCell<HashMap<isize, Props>> = RefCell::new(HashMap::new());
}

fn with_props<R>(h: HWND, f: impl FnOnce(&mut Props) -> R) -> Option<R> {
    WINDOWS.with(|m| m.try_borrow_mut().ok()?.get_mut(&(h.0 as isize)).map(f))
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
            lpszClassName: w!("dvfilesProps"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
    }
}

/// Enter and Esc inside a Properties window's name box act on the window
pub fn dialog_message(msg: &MSG) -> bool {
    if msg.message != WM_KEYDOWN {
        return false;
    }
    let vk = windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(msg.wParam.0 as u16);
    if vk != windows::Win32::UI::Input::KeyboardAndMouse::VK_RETURN
        && vk != windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE
    {
        return false;
    }
    let parent = unsafe { GetParent(msg.hwnd).unwrap_or_default() };
    let ours = WINDOWS.with(|m| m.borrow().contains_key(&(parent.0 as isize)));
    if !ours || parent.0.is_null() {
        return false;
    }
    unsafe {
        let _ = PostMessageW(Some(parent), WM_KEYDOWN, msg.wParam, msg.lParam);
    }
    true
}

pub fn retheme() {
    let all: Vec<isize> = WINDOWS.with(|m| m.borrow().keys().copied().collect());
    for h in all {
        let h = HWND(h as *mut _);
        dark_frame(h);
        invalidate(h);
    }
}

fn dark_frame(hwnd: HWND) {
    let Some(t) = with_app(|a| a.theme) else { return };
    let dark = t.dark as i32;
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const i32 as *const _, 4);
    }
    crate::ui::tint_frame(hwnd, t.bg, t.fg, t.frame);
}

fn times(path: &str) -> Option<(u64, u64, u64)> {
    let mut d = WIN32_FILE_ATTRIBUTE_DATA::default();
    unsafe {
        GetFileAttributesExW(
            PCWSTR(dvfiles::wide::long(path).as_ptr()),
            GetFileExInfoStandard,
            &mut d as *mut _ as *mut _,
        )
        .ok()?;
    }
    let ft = |t: FILETIME| (t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64;
    Some((ft(d.ftCreationTime), ft(d.ftLastWriteTime), ft(d.ftLastAccessTime)))
}

fn opens_with(ext: &str) -> String {
    if ext.is_empty() {
        return String::new();
    }
    let assoc = wstr(&format!(".{ext}"));
    let mut buf = [0u16; 260];
    let mut n = buf.len() as u32;
    let r = unsafe {
        AssocQueryStringW(
            ASSOCF_NONE,
            ASSOCSTR_FRIENDLYAPPNAME,
            PCWSTR(assoc.as_ptr()),
            PCWSTR::null(),
            Some(PWSTR(buf.as_mut_ptr())),
            &mut n,
        )
    };
    if r.is_ok() {
        crate::ui::from_wide(&buf)
    } else {
        String::new()
    }
}

fn disk_size(path: &str, size: u64, attrs: u32, cluster: u64) -> u64 {
    if fsx::is_cloud_only(attrs) {
        return 0;
    }
    let mut high = 0u32;
    let low = unsafe { GetCompressedFileSizeW(PCWSTR(dvfiles::wide::long(path).as_ptr()), Some(&mut high)) };
    let on_disk = if low == u32::MAX && high == 0 { size } else { (high as u64) << 32 | low as u64 };
    on_disk.div_ceil(cluster) * cluster
}

fn tri(items: &[u32], bit: u32) -> Tri {
    let on = items.iter().filter(|a| *a & bit != 0).count();
    if on == 0 {
        Tri::Off
    } else if on == items.len() {
        Tri::On
    } else {
        Tri::Mixed
    }
}

/// opens a Properties window for these items (all in one folder) or for a drive
pub fn open(paths: Vec<String>, drive_view: bool) {
    let Some((owner, scale, font)) = with_app(|a| (a.main, a.scale, a.fonts.ui)) else { return };
    let first = paths[0].clone();
    let mut p = Props {
        paths: paths.clone(),
        drive: None,
        is_dir: false,
        name: String::new(),
        type_line: String::new(),
        opens_with: String::new(),
        location: String::new(),
        times: None,
        own_bytes: 0,
        own_disk: 0,
        files: 0,
        dirs: 0,
        live: Arc::new(LiveTotals::default()),
        measuring: Arc::new(AtomicBool::new(false)),
        stop: Arc::new(AtomicBool::new(false)),
        readonly: Tri::Off,
        hidden: Tri::Off,
        readonly0: Tri::Off,
        hidden0: Tri::Off,
        edit: None,
        own_path: None,
        art: None,
        hot: None,
        hits: Vec::new(),
        surface: None,
        page: Page::General,
        details: None,
        security: None,
    };
    let root = fsx::volume_root(&first).unwrap_or_else(|| first.clone());
    let cluster = fsx::cluster_size(&root);
    let mut walk_roots = Vec::new();
    let mut attrs_all = Vec::new();
    if drive_view && paths.len() == 1 {
        let d = fsx::drive_info(&first);
        p.name = d.label.clone();
        p.type_line = d.type_name().to_string();
        p.drive = Some(d);
        p.is_dir = true;
        p.art = Some(crate::ui::art::Art::Drive);
    } else {
        for path in &paths {
            let Ok(e) = fsx::stat(path) else { continue };
            attrs_all.push(e.attrs);
            if e.is_dir() {
                p.dirs += 1;
                if e.attrs & fsx::FA_REPARSE == 0 {
                    walk_roots.push(path.clone());
                }
            } else {
                p.files += 1;
                p.own_bytes += e.size;
                p.own_disk += disk_size(path, e.size, e.attrs, cluster);
            }
        }
        let single = paths.len() == 1;
        p.is_dir = single && p.dirs == 1;
        p.location = dvfiles::pathx::parent(&first).unwrap_or_default();
        if single {
            p.name = dvfiles::pathx::file_name(&first).to_string();
            p.times = times(&first);
            let e = fsx::stat(&first).ok();
            if let Some(e) = &e {
                let ext = dvfiles::naming::ext_lower(&e.name);
                let own = e.is_dir() || !crate::ui::icons::per_file(&ext);
                let (ty, icon, art) = with_app(|a| {
                    let ty = a.icons.type_name(e).to_string();
                    if own {
                        let special = a
                            .known
                            .iter()
                            .find(|(_, k)| dvfiles::pathx::same(k, &first))
                            .and_then(|(l, _)| crate::ui::art::special_for(l));
                        let art = if e.is_dir() {
                            special.map_or(crate::ui::art::Art::Folder, crate::ui::art::Art::Special)
                        } else {
                            crate::ui::art::Art::File(crate::ui::art::kind_of(&ext))
                        };
                        (ty, None, Some(art))
                    } else {
                        (ty, Some(first.clone()), None)
                    }
                })
                .unwrap_or_default();
                p.art = art;
                p.type_line = if e.is_dir() || ext.is_empty() { ty } else { format!("{ty} (.{ext})") };
                p.own_path = icon;
                if !e.is_dir() {
                    p.opens_with = opens_with(&ext);
                }
            }
        } else {
            p.name = format!(
                "{}, {}",
                dvfiles::fmt::plural(p.files, "File", "Files", &Default::default()),
                dvfiles::fmt::plural(p.dirs, "Folder", "Folders", &Default::default())
            );
            p.type_line = "Multiple types".into();
            p.location = format!("All in {}", p.location);
        }
        p.readonly = tri(&attrs_all, FA_READONLY);
        p.hidden = tri(&attrs_all, FA_HIDDEN);
        p.readonly0 = p.readonly;
        p.hidden0 = p.hidden;
    }
    if !walk_roots.is_empty() {
        p.measuring.store(true, Ordering::SeqCst);
        let (live, measuring, stop) = (p.live.clone(), p.measuring.clone(), p.stop.clone());
        std::thread::spawn(move || {
            dvfiles::walker::measure(&walk_roots, cluster, &stop, &live);
            measuring.store(false, Ordering::SeqCst);
        });
    }
    let title = if paths.len() == 1 {
        let n = if p.drive.is_some() { p.drive.as_ref().unwrap().title() } else { p.name.clone() };
        format!("{n} Properties")
    } else {
        "Properties".to_string()
    };
    let height = if p.drive.is_some() { 510.0 } else { 560.0 };
    let (w, h) = ((400.0 * scale) as i32, (height * scale) as i32);
    let hwnd = unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let mut r = RECT { left: 0, top: 0, right: w, bottom: h };
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;
        let _ = AdjustWindowRectEx(&mut r, style, false, WINDOW_EX_STYLE(0));
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("dvfilesProps"),
            PCWSTR(wstr(&title).as_ptr()),
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
    // the name box, for one item (a drive's label too)
    let editable = paths.len() == 1;
    if editable {
        unsafe {
            let style = WS_CHILD | WS_VISIBLE | WS_BORDER | WINDOW_STYLE(ES_AUTOHSCROLL as u32);
            let s = |v: f32| (v * scale) as i32;
            if let Ok(e) = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("EDIT"),
                PCWSTR(wstr(&p.name).as_ptr()),
                style,
                s(78.0),
                s(24.0),
                w - s(96.0),
                s(26.0),
                Some(hwnd),
                None,
                None,
                None,
            ) {
                SendMessageW(e, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
                // a drive's label needs administrator rights to change, so it is shown read-only
                if p.drive.is_some() {
                    SendMessageW(e, EM_SETREADONLY, Some(WPARAM(1)), None);
                }
                p.edit = Some(e);
            }
        }
    }
    let measuring = p.measuring.load(Ordering::SeqCst);
    WINDOWS.with(|m| m.borrow_mut().insert(hwnd.0 as isize, p));
    dark_frame(hwnd);
    unsafe {
        if measuring {
            SetTimer(Some(hwnd), T_MEASURE, 120, None);
        }
        // centered over the main window
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
        let _ = ShowWindow(hwnd, SW_SHOW);
    }
}

fn paint(h: HWND) {
    let (art, own_path, drive, is_dir) =
        with_props(h, |p| (p.art, p.own_path.clone(), p.drive.is_some(), p.is_dir)).unwrap_or_default();
    let Some((t, ui, bold, check, scale, nf, art_img)) = with_app(|a| {
        use crate::ui::art::{Art, Kind, Mark};
        let size = a.s(48.0);
        let img = art.map(|x| a.art.get(x, size, &a.theme)).or_else(|| {
            own_path.map(|p| match a.icons.own(&p, size) {
                crate::ui::icons::Own::Image(i) => i,
                crate::ui::icons::Own::Art(x) => a.art.get(x, size, &a.theme),
                crate::ui::icons::Own::Pending => a.art.get(Art::File(Kind::App), size, &a.theme),
            })
        });
        let fallback = if drive {
            Art::Drive
        } else if is_dir {
            Art::Folder
        } else {
            Art::File(Kind::Generic)
        };
        let img = img.unwrap_or_else(|| a.art.get(fallback, size, &a.theme));
        let check = a.art.mark(Mark::Check, a.s(14.0), a.theme.accent);
        a.icons.flush();
        (a.theme, a.fonts.ui, a.fonts.bold, check, a.scale, a.nf, img)
    }) else {
        return;
    };
    let (w, ht) = crate::ui::client_size(h);
    with_props(h, |p| {
        if Surface::ensure(&mut p.surface, w, ht).is_none() {
            return;
        }
        let s = |v: f32| (v * scale).round() as i32;
        let surf = p.surface.as_ref().unwrap();
        let mut hits = Vec::new();
        surf.fill(R::new(0, 0, w, ht), t.bg);
        let pad = s(18.0);
        // icon
        let ir = R::new(pad, s(14.0), s(48.0), s(48.0));
        surf.image_in(ir, &art_img);
        if p.edit.is_none() {
            surf.text(R::new(s(78.0), s(24.0), w - s(96.0), s(26.0)), &p.name, ui, t.fg, gfx::LEFT);
        }
        // the tabs: Details for one file, Security for one item
        let single = p.paths.len() == 1;
        let pages: Vec<(Page, &str)> =
            [(Page::General, "General"), (Page::Details, "Details"), (Page::Security, "Security")]
                .into_iter()
                .filter(|(pg, _)| match pg {
                    Page::General => true,
                    Page::Details => single && !p.is_dir && p.drive.is_none(),
                    Page::Security => single,
                })
                .collect();
        let ty = s(70.0);
        if pages.len() > 1 {
            let mut tx = pad - s(10.0);
            for (pg, label) in &pages {
                let tw = gfx::text_width(surf.dc, label, ui) + s(20.0);
                let r = R::new(tx, ty, tw, s(32.0));
                let on = p.page == *pg;
                if !on && p.hot == Some(Btn::Tab(*pg)) {
                    surf.fill(r, t.hover);
                }
                surf.text(r, label, if on { bold } else { ui }, if on { t.fg } else { t.muted }, gfx::CENTER);
                if on {
                    surf.fill(R::new(r.x + s(10.0), r.bottom() - s(2.0), r.w - s(20.0), s(2.0)), t.accent);
                }
                hits.push((Btn::Tab(*pg), r));
                tx += tw;
            }
            surf.fill(R::new(pad, ty + s(32.0), w - 2 * pad, 1), t.line);
        }
        let mut y = if pages.len() > 1 { s(112.0) } else { s(72.0) };
        let line = |y: &mut i32| {
            surf.fill(R::new(pad, *y + s(6.0), w - 2 * pad, 1), t.line);
            *y += s(14.0);
        };
        if pages.len() <= 1 {
            line(&mut y);
        }
        let row = |y: &mut i32, k: &str, v: &str| {
            surf.text(R::new(pad, *y, s(110.0), s(22.0)), k, ui, t.muted, gfx::LEFT);
            // paths lose their middle rather than their end
            let flags = if v.contains('\\') { gfx::PATH_LEFT } else { gfx::LEFT };
            surf.text(R::new(pad + s(112.0), *y, w - 2 * pad - s(112.0), s(22.0)), v, ui, t.fg, flags);
            *y += s(24.0);
        };
        if p.page == Page::Details {
            let rows = p.details.clone().unwrap_or_default();
            if rows.is_empty() {
                surf.text(
                    R::new(pad, y, w - 2 * pad, s(22.0)),
                    "Nothing more is known about this file.",
                    ui,
                    t.muted,
                    gfx::LEFT,
                );
            }
            for (k, v) in &rows {
                row(&mut y, &format!("{k}:"), v);
            }
        } else if p.page == Page::Security {
            match &p.security {
                Some(Some(sec)) => {
                    row(&mut y, "Owner:", &sec.owner);
                    line(&mut y);
                    surf.text(R::new(pad, y, w - 2 * pad, s(22.0)), "Who may do what", bold, t.fg, gfx::LEFT);
                    y += s(28.0);
                    for (who, what) in &sec.entries {
                        // an account's own name first, its domain after in grey
                        let (domain, name) = who.rsplit_once('\\').map_or(("", who.as_str()), |(d, n)| (d, n));
                        surf.text(R::new(pad, y, s(170.0), s(22.0)), name, ui, t.fg, gfx::LEFT);
                        surf.text(
                            R::new(pad + s(172.0), y, w - 2 * pad - s(172.0), s(22.0)),
                            what,
                            ui,
                            if what.starts_with("Deny") { t.danger } else { t.muted },
                            gfx::LEFT,
                        );
                        let _ = domain;
                        y += s(24.0);
                    }
                }
                _ => surf.text(
                    R::new(pad, y, w - 2 * pad, s(22.0)),
                    "The permissions could not be read.",
                    ui,
                    t.muted,
                    gfx::LEFT,
                ),
            }
            let wr = R::new(pad - s(6.0), ht - s(86.0), w - 2 * pad, s(30.0));
            if p.hot == Some(Btn::Windows) {
                surf.round(wr, s(5.0) as f32, t.btn_hover);
            }
            surf.text(
                wr.inset(s(6.0), 0),
                "Change permissions or see previous versions in Windows...",
                ui,
                t.accent,
                gfx::LEFT,
            );
            hits.push((Btn::Windows, wr));
        } else if let Some(d) = &p.drive {
            row(&mut y, "Type:", d.type_name());
            row(&mut y, "File system:", &d.fs);
            line(&mut y);
            let used = d.total.saturating_sub(d.free);
            for (k, v, c) in [("Used space:", used, t.accent), ("Free space:", d.free, t.line)] {
                surf.fill(R::new(pad, y + s(5.0), s(12.0), s(12.0)), c);
                surf.text(R::new(pad + s(20.0), y, s(100.0), s(22.0)), k, ui, t.muted, gfx::LEFT);
                surf.text(
                    R::new(pad + s(122.0), y, w - 2 * pad - s(122.0), s(22.0)),
                    &nf.bytes_exact(v),
                    ui,
                    t.fg,
                    gfx::LEFT,
                );
                y += s(24.0);
            }
            line(&mut y);
            row(&mut y, "Capacity:", &nf.bytes_exact(d.total));
            let frac = if d.total > 0 { used as f32 / d.total as f32 } else { 0.0 };
            let cy = y as f32 + s(70.0) as f32;
            surf.donut(w as f32 / 2.0, cy, s(62.0) as f32, s(36.0) as f32, frac, t.accent, t.line);
            surf.text(
                R::new(0, (cy - s(10.0) as f32) as i32, w, s(20.0)),
                &format!("{:.0}%", frac * 100.0),
                bold,
                t.fg,
                gfx::CENTER,
            );
            y = (cy as i32) + s(80.0);
            surf.text(
                R::new(0, y - s(4.0), w, s(20.0)),
                &format!("Drive {}", d.root.trim_end_matches('\\')),
                ui,
                t.muted,
                gfx::CENTER,
            );
        } else {
            let tot: Totals = p.live.get();
            let measuring = p.measuring.load(Ordering::Relaxed);
            row(&mut y, if p.paths.len() == 1 && !p.is_dir { "Type of file:" } else { "Type:" }, &p.type_line);
            if !p.opens_with.is_empty() {
                row(&mut y, "Opens with:", &p.opens_with);
            }
            line(&mut y);
            row(&mut y, "Location:", &p.location);
            let bytes = p.own_bytes + tot.bytes;
            let disk = p.own_disk + tot.on_disk;
            let more = if measuring { "  ..." } else { "" };
            row(&mut y, "Size:", &format!("{}{more}", nf.bytes_exact(bytes)));
            row(&mut y, "Size on disk:", &format!("{}{more}", nf.bytes_exact(disk)));
            if p.dirs > 0 {
                let files = tot.files + p.files;
                let dirs = tot.dirs + p.dirs.saturating_sub(if p.paths.len() == 1 { 1 } else { 0 });
                let text = format!(
                    "{}, {}{more}",
                    dvfiles::fmt::plural(files, "File", "Files", &nf),
                    dvfiles::fmt::plural(dirs, "Folder", "Folders", &nf)
                );
                row(&mut y, "Contains:", &text);
            }
            line(&mut y);
            if let Some((c, m, acc)) = p.times {
                row(&mut y, "Created:", &locale::long(c));
                row(&mut y, "Modified:", &locale::long(m));
                row(&mut y, "Accessed:", &locale::long(acc));
                line(&mut y);
            }
            surf.text(R::new(pad, y, s(110.0), s(22.0)), "Attributes:", ui, t.muted, gfx::LEFT);
            let mut x = pad + s(112.0);
            for (b, label, state) in [(Btn::ReadOnly, "Read-only", p.readonly), (Btn::Hidden, "Hidden", p.hidden)] {
                let cb = R::new(x, y + s(3.0), s(16.0), s(16.0));
                surf.frame(cb, if p.hot == Some(b) { t.accent } else { t.muted });
                match state {
                    Tri::On => surf.image_in(cb, &check),
                    Tri::Mixed => surf.fill(cb.inset(s(4.0), s(4.0)), t.accent),
                    Tri::Off => {}
                }
                surf.text(R::new(cb.right() + s(6.0), y, s(90.0), s(22.0)), label, ui, t.fg, gfx::LEFT);
                hits.push((b, R::new(x, y, s(100.0), s(22.0))));
                x += s(110.0);
            }
        }
        // buttons
        let by = ht - s(46.0);
        let bw = s(80.0);
        let mut bx = w - pad - bw;
        let changed = p.readonly != p.readonly0
            || p.hidden != p.hidden0
            || p.edit.is_some_and(|e| crate::ui::window_text(e) != p.name);
        for (b, label, primary, enabled) in
            [(Btn::Apply, "Apply", false, changed), (Btn::Cancel, "Cancel", false, true), (Btn::Ok, "OK", true, true)]
        {
            let r = R::new(bx, by, bw, s(30.0));
            let hot = p.hot == Some(b) && enabled;
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
            surf.round(r, s(5.0) as f32, bg);
            let fg = if primary {
                t.primary_fg
            } else if enabled {
                t.fg
            } else {
                t.faint
            };
            surf.text(r, label, ui, fg, gfx::CENTER);
            hits.push((b, r));
            bx -= bw + s(8.0);
        }
        p.hits = hits;
        surf.present(h);
    });
}

/// applies the name and attributes; false if something failed and the window should stay open
fn apply(h: HWND) -> bool {
    let Some((paths, name0, edit, ro, hid, ro0, hid0, is_drive)) = with_props(h, |p| {
        (p.paths.clone(), p.name.clone(), p.edit, p.readonly, p.hidden, p.readonly0, p.hidden0, p.drive.is_some())
    }) else {
        return true;
    };
    if is_drive {
        return true;
    }
    let mut paths = paths;
    let mut ok = true;
    if let Some(e) = edit {
        let typed = crate::ui::window_text(e);
        if typed != name0 {
            match dvfiles::naming::validate(&typed) {
                Ok(new) => {
                    let dir = dvfiles::pathx::parent(&paths[0]).unwrap_or_default();
                    let to = dvfiles::pathx::join(&dir, &new);
                    let r = unsafe {
                        windows::Win32::Storage::FileSystem::MoveFileExW(
                            PCWSTR(dvfiles::wide::long(&paths[0]).as_ptr()),
                            PCWSTR(dvfiles::wide::long(&to).as_ptr()),
                            windows::Win32::Storage::FileSystem::MOVE_FILE_FLAGS(0),
                        )
                    };
                    match r {
                        Ok(()) => {
                            paths[0] = to;
                            with_props(h, |p| {
                                p.paths = paths.clone();
                                p.name = new.clone();
                            });
                            unsafe {
                                let _ = SetWindowTextW(h, PCWSTR(wstr(&format!("{new} Properties")).as_ptr()));
                            }
                        }
                        Err(err) => {
                            shell::message(h, &format!("Could not rename: {}", err.message()), true);
                            ok = false;
                        }
                    }
                }
                Err(msg) => {
                    shell::message(h, msg, true);
                    ok = false;
                }
            }
        }
    }
    if ro != ro0 || hid != hid0 {
        for path in &paths {
            let Some(mut a) = fsx::attributes(path) else { continue };
            if ro != ro0 && ro != Tri::Mixed {
                a = if ro == Tri::On { a | FA_READONLY } else { a & !FA_READONLY };
            }
            if hid != hid0 && hid != Tri::Mixed {
                a = if hid == Tri::On { a | FA_HIDDEN } else { a & !FA_HIDDEN };
            }
            // a folder keeps its directory bit; SetFileAttributes ignores it anyway
            if let Err(e) = fsx::set_attributes(path, a & !FA_DIRECTORY) {
                shell::message(h, &format!("Could not change attributes of {path}: {e}"), true);
                ok = false;
                break;
            }
        }
        with_props(h, |p| {
            p.readonly0 = p.readonly;
            p.hidden0 = p.hidden;
        });
    }
    crate::ui::refresh();
    invalidate(h);
    ok
}

fn click(h: HWND, x: i32, y: i32) {
    let Some(b) = with_props(h, |p| p.hits.iter().find(|(_, r)| r.contains(x, y)).map(|(b, _)| *b)).flatten() else {
        return;
    };
    match b {
        Btn::Tab(pg) => {
            with_props(h, |p| {
                p.page = pg;
                if pg == Page::Details && p.details.is_none() {
                    p.details = Some(crate::ui::details::details(&p.paths[0]));
                }
                if pg == Page::Security && p.security.is_none() {
                    p.security = Some(crate::ui::details::security(&p.paths[0]));
                }
            });
            invalidate(h);
        }
        Btn::ReadOnly | Btn::Hidden => {
            with_props(h, |p| {
                let s = if b == Btn::ReadOnly { &mut p.readonly } else { &mut p.hidden };
                *s = if *s == Tri::On { Tri::Off } else { Tri::On };
            });
            invalidate(h);
        }
        Btn::Windows => {
            if let Some(path) = with_props(h, |p| p.paths[0].clone()) {
                shell::properties(h, &path);
            }
        }
        Btn::Apply => {
            apply(h);
        }
        Btn::Ok => {
            if apply(h) {
                unsafe {
                    let _ = DestroyWindow(h);
                }
            }
        }
        Btn::Cancel => unsafe {
            let _ = DestroyWindow(h);
        },
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
            WM_TIMER => {
                let done = with_props(hwnd, |p| !p.measuring.load(Ordering::Relaxed)).unwrap_or(true);
                if done {
                    let _ = KillTimer(Some(hwnd), T_MEASURE);
                }
                invalidate(hwnd);
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let (x, y) = (x_of(lp), y_of(lp));
                let changed = with_props(hwnd, |p| {
                    let hot = p.hits.iter().find(|(_, r)| r.contains(x, y)).map(|(b, _)| *b);
                    let c = hot != p.hot;
                    p.hot = hot;
                    c
                });
                if changed == Some(true) {
                    invalidate(hwnd);
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                click(hwnd, x_of(lp), y_of(lp));
                LRESULT(0)
            }
            WM_COMMAND => {
                if ((wp.0 >> 16) & 0xFFFF) as u32 == EN_CHANGE {
                    invalidate(hwnd);
                }
                LRESULT(0)
            }
            WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC => {
                let colors = with_app(|a| (a.theme.fg, a.theme.input_bg, a.input_brush));
                if let Some((fg, bg, brush)) = colors {
                    let dc = HDC(wp.0 as *mut _);
                    SetTextColor(dc, fg.colorref());
                    SetBkColor(dc, bg.colorref());
                    return LRESULT(brush.0 as isize);
                }
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            WM_KEYDOWN => {
                match windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(wp.0 as u16) {
                    windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE => {
                        let _ = DestroyWindow(hwnd);
                    }
                    windows::Win32::UI::Input::KeyboardAndMouse::VK_RETURN => {
                        if apply(hwnd) {
                            let _ = DestroyWindow(hwnd);
                        }
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                WINDOWS.with(|m| m.borrow_mut().remove(&(hwnd.0 as isize)));
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
