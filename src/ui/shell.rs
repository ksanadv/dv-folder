// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! What only the shell can do: open a file with its program, "Open with", the full Properties sheet, the classic
//! context menu with every app's entries ("Show more options"), resolving shortcuts, and dragging files to other
//! programs. COM is started the first time one of these is used, never at startup.
use crate::ui::{from_wide, wstr};
use std::cell::{Cell, RefCell};
use windows::core::{Interface, PCSTR, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ};
use windows::Win32::System::Ole::OleInitialize;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const CMIC_MASK_UNICODE: u32 = 0x4000;
const CMIC_MASK_PTINVOKE: u32 = 0x2000_0000;

thread_local! {
    static OLE: Cell<bool> = const { Cell::new(false) };
    // the shell menu being shown, so owner-drawn submenus (Send to, Open with) can draw themselves
    static MENU: RefCell<Option<IContextMenu>> = const { RefCell::new(None) };
}

pub fn ole_init() {
    OLE.with(|c| {
        if !c.get() {
            unsafe {
                let _ = OleInitialize(None);
            }
            c.set(true);
        }
    });
}

/// a notice in dvfiles' own box (dialog.rs)
pub fn message(hwnd: HWND, text: &str, error: bool) {
    crate::ui::dialog::message(
        hwnd,
        text,
        if error { crate::ui::dialog::Icon::Error } else { crate::ui::dialog::Icon::Info },
    );
}

/// a Yes/No question in dvfiles' own box, No by default
pub fn ask(hwnd: HWND, text: &str) -> bool {
    crate::ui::dialog::ask(hwnd, text)
}

/// what Windows would run for a file type, or None when it would ask how to open it (no program, or several and no
/// choice made: its "How do you want to open this file?" box)
fn has_program(ext: &str) -> bool {
    let dotted = wstr(&format!(".{ext}"));
    let mut buf = [0u16; 1024];
    let mut n = buf.len() as u32;
    let r = unsafe {
        AssocQueryStringW(
            ASSOCF_NOTRUNCATE,
            ASSOCSTR_COMMAND,
            PCWSTR(dotted.as_ptr()),
            PCWSTR::null(),
            Some(windows::core::PWSTR(buf.as_mut_ptr())),
            &mut n,
        )
    };
    // a command that runs OpenWith.exe is Windows' own question; DelegateExecute verbs have no command but a program
    match r.ok() {
        Ok(()) => !from_wide(&buf).to_lowercase().contains("openwith.exe"),
        Err(_) => unsafe {
            let mut n = buf.len() as u32;
            AssocQueryStringW(
                ASSOCF_NOTRUNCATE,
                ASSOCSTR_DELEGATEEXECUTE,
                PCWSTR(dotted.as_ptr()),
                PCWSTR::null(),
                Some(windows::core::PWSTR(buf.as_mut_ptr())),
                &mut n,
            )
            .is_ok()
        },
    }
}

/// Opens a file with its program. A document type with no program yet gets dvfiles' Open with rather than Windows'
/// "How do you want to open this file?"; programs, scripts and shortcuts always go to Windows as they are.
pub fn open(hwnd: HWND, path: &str, dir: &str) {
    let ext = dvfiles::naming::ext_lower(path);
    const RUNS: [&str; 14] =
        ["exe", "com", "bat", "cmd", "msi", "msc", "ps1", "vbs", "js", "lnk", "url", "scr", "cpl", "appref-ms"];
    if !RUNS.contains(&ext.as_str()) && !has_program(&ext) {
        open_with(hwnd, path);
        return;
    }
    let (p, d) = (wstr(path), wstr(dir));
    let r = unsafe {
        ShellExecuteW(Some(hwnd), PCWSTR::null(), PCWSTR(p.as_ptr()), PCWSTR::null(), PCWSTR(d.as_ptr()), SW_SHOWNORMAL)
    };
    let code = r.0 as isize;
    if code <= 32 {
        // SE_ERR_NOASSOC
        if code == 31 {
            open_with(hwnd, path);
        } else if code != 5 && code != 1223 {
            message(hwnd, &format!("Windows cannot open \"{path}\" (error {code})."), true);
        }
    }
}

pub fn run_as_admin(hwnd: HWND, path: &str, dir: &str) {
    let (p, d) = (wstr(path), wstr(dir));
    unsafe {
        ShellExecuteW(
            Some(hwnd),
            windows::core::w!("runas"),
            PCWSTR(p.as_ptr()),
            PCWSTR::null(),
            PCWSTR(d.as_ptr()),
            SW_SHOWNORMAL,
        );
    }
}

/// dvfiles' own Open with (openwith.rs)
pub fn open_with(hwnd: HWND, path: &str) {
    let owner = unsafe { GetAncestor(hwnd, GA_ROOT) };
    crate::ui::openwith::show(owner, path);
}

/// opens `file` with the program at `exe`, through Windows (so a downloaded program still gets its warning)
pub fn open_in(hwnd: HWND, exe: &str, file: &str) {
    let (e, args) = (wstr(exe), wstr(&format!("\"{file}\"")));
    let dir = wstr(&dvfiles::pathx::parent(file).unwrap_or_default());
    let r = unsafe {
        ShellExecuteW(
            Some(hwnd),
            PCWSTR::null(),
            PCWSTR(e.as_ptr()),
            PCWSTR(args.as_ptr()),
            PCWSTR(dir.as_ptr()),
            SW_SHOWNORMAL,
        )
    };
    if (r.0 as isize) <= 32 && r.0 as isize != 1223 {
        message(hwnd, &format!("\"{exe}\" could not be started."), true);
    }
}

/// Windows' Default apps settings: the only place a default program can be set
pub fn default_apps(hwnd: HWND) {
    let uri = wstr("ms-settings:defaultapps");
    unsafe {
        ShellExecuteW(Some(hwnd), PCWSTR::null(), PCWSTR(uri.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// the shell's data object for one file (what an Open with program is handed)
pub fn data_object(path: &str) -> Option<windows::Win32::System::Com::IDataObject> {
    ole_init();
    let (parent, ids) = children(&[path.to_string()])?;
    let list: Vec<*const ITEMIDLIST> = ids.0.iter().map(|&a| a as *const ITEMIDLIST).collect();
    unsafe { parent.GetUIObjectOf(HWND::default(), &list, None).ok() }
}

/// the Windows Properties sheet (Security, Details, Previous Versions, ...)
pub fn properties(hwnd: HWND, path: &str) {
    let p = wstr(path);
    unsafe {
        let _ = SHObjectProperties(Some(hwnd), SHOP_FILEPATH, PCWSTR(p.as_ptr()), PCWSTR::null());
    }
}

/// a shortcut's target path: read from the file itself, or (shortcuts to shell folders, installer shortcuts) asked of
/// the shell
pub fn resolve_link(path: &str) -> Option<String> {
    if let Some(t) = dvfiles::lnk::target(path) {
        return Some(t);
    }
    ole_init();
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = link.cast().ok()?;
        file.Load(PCWSTR(wstr(path).as_ptr()), STGM_READ).ok()?;
        let mut buf = [0u16; 1024];
        link.GetPath(&mut buf, std::ptr::null_mut(), 0).ok()?;
        let s = from_wide(&buf);
        (!s.is_empty()).then_some(s)
    }
}

/// Desktop, Downloads, Documents, ... as the user moved them, from the registry the shell keeps them in
pub fn known_folders() -> Vec<(&'static str, String)> {
    use winreg::enums::HKEY_CURRENT_USER;
    let key = winreg::RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders");
    let profile = std::env::var("USERPROFILE").unwrap_or_default();
    let names = [
        ("Desktop", "Desktop", "Desktop"),
        ("Downloads", "{374DE290-123F-4565-9164-39C4925E467B}", "Downloads"),
        ("Documents", "Personal", "Documents"),
        ("Pictures", "My Pictures", "Pictures"),
        ("Music", "My Music", "Music"),
        ("Videos", "My Video", "Videos"),
    ];
    names
        .iter()
        .filter_map(|(label, value, default)| {
            let raw: Option<String> = key.as_ref().ok().and_then(|k| k.get_value(value).ok());
            let path = match raw {
                Some(r) => expand(&r),
                None => format!(r"{profile}\{default}"),
            };
            dvfiles::fsx::is_dir(&path).then(|| (*label, path))
        })
        .collect()
}

pub fn expand(s: &str) -> String {
    let w = wstr(s);
    let mut buf = vec![0u16; 2048];
    let n =
        unsafe { windows::Win32::System::Environment::ExpandEnvironmentStringsW(PCWSTR(w.as_ptr()), Some(&mut buf)) };
    if n == 0 || n as usize > buf.len() {
        return s.to_string();
    }
    from_wide(&buf)
}

pub fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

/// Windows Terminal if it is installed, else PowerShell, starting in `dir`
pub fn terminal(dir: &str) {
    use std::os::windows::process::CommandExt;
    let wt = env("LOCALAPPDATA").map(|l| format!(r"{l}\Microsoft\WindowsApps\wt.exe"));
    if let Some(wt) = wt.filter(|p| std::path::Path::new(p).exists()) {
        if std::process::Command::new(wt).arg("-d").arg(dir).spawn().is_ok() {
            return;
        }
    }
    const CREATE_NEW_CONSOLE: u32 = 0x10;
    let _ = std::process::Command::new("powershell.exe").current_dir(dir).creation_flags(CREATE_NEW_CONSOLE).spawn();
}

/// a new dvfiles window (its own process, which keeps each window independent)
pub fn new_window(path: Option<&str>) {
    if let Ok(exe) = std::env::current_exe() {
        let mut c = std::process::Command::new(exe);
        if let Some(p) = path {
            c.arg(p);
        }
        let _ = c.spawn();
    }
}

struct Pidls(Vec<*mut ITEMIDLIST>);

impl Drop for Pidls {
    fn drop(&mut self) {
        for p in &self.0 {
            unsafe { ILFree(Some(*p)) };
        }
    }
}

fn pidls(paths: &[String]) -> Option<Pidls> {
    let mut out = Pidls(Vec::new());
    for p in paths {
        let mut pidl = std::ptr::null_mut();
        unsafe { SHParseDisplayName(PCWSTR(wstr(p).as_ptr()), None, &mut pidl, 0, None).ok()? };
        out.0.push(pidl);
    }
    Some(out)
}

/// The items' folder and each item's id in it, when they share one: the folder is parsed once and each name is
/// looked up in it, where parsing every full path from the desktop down took about a millisecond an item.
fn children(paths: &[String]) -> Option<(IShellFolder, Pidls)> {
    let folder = dvfiles::pathx::parent(paths.first()?)?;
    if !paths.iter().all(|p| dvfiles::pathx::parent(p).is_some_and(|d| dvfiles::pathx::same(&d, &folder))) {
        return None;
    }
    let fp = pidls(&[folder])?;
    unsafe {
        let sf: IShellFolder = SHGetDesktopFolder().ok()?.BindToObject(fp.0[0], None).ok()?;
        let mut out = Pidls(Vec::with_capacity(paths.len()));
        for p in paths {
            let name = wstr(dvfiles::pathx::file_name(p));
            let mut pidl = std::ptr::null_mut();
            sf.ParseDisplayName(HWND::default(), None, PCWSTR(name.as_ptr()), None, &mut pidl, std::ptr::null_mut())
                .ok()?;
            out.0.push(pidl);
        }
        Some((sf, out))
    }
}

/// The shell's own context menu for items in one folder, or for the folder's background when `items` is empty.
/// Returns once the chosen command has run.
pub fn context_menu(hwnd: HWND, folder: &str, items: &[String], x: i32, y: i32, extended: bool) {
    ole_init();
    let cm: Option<IContextMenu> = unsafe {
        if items.is_empty() {
            pidls(&[folder.to_string()]).and_then(|p| {
                let desktop = SHGetDesktopFolder().ok()?;
                let sf: IShellFolder = desktop.BindToObject(p.0[0], None).ok()?;
                sf.CreateViewObject(hwnd).ok()
            })
        } else {
            match children(items) {
                Some((parent, p)) => {
                    let ids: Vec<*const ITEMIDLIST> = p.0.iter().map(|&a| a as *const ITEMIDLIST).collect();
                    parent.GetUIObjectOf(hwnd, &ids, None).ok()
                }
                None => pidls(items).and_then(|p| {
                    let mut last = std::ptr::null_mut();
                    let parent: IShellFolder = SHBindToParent(p.0[0], Some(&mut last)).ok()?;
                    let ids: Vec<*const ITEMIDLIST> =
                        p.0.iter().map(|&a| ILFindLastID(a) as *const ITEMIDLIST).collect();
                    parent.GetUIObjectOf(hwnd, &ids, None).ok()
                }),
            }
        }
    };
    let Some(cm) = cm else { return };
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let mut flags = CMF_NORMAL | CMF_CANRENAME;
        if extended {
            flags |= CMF_EXTENDEDVERBS;
        }
        if cm.QueryContextMenu(menu, 0, 1, 0x7FFF, flags).is_err() {
            let _ = DestroyMenu(menu);
            return;
        }
        // drawn by dvfiles, from the shell's own items (with what 7-Zip, WinRAR and the like add); Windows' menu only
        // if an item draws itself and has no text to show
        let owner = GetAncestor(hwnd, GA_ROOT);
        let t = std::time::Instant::now();
        let rows = read_menu(&cm, menu, 0);
        crate::trace::log(|| {
            format!(
                "shell menu read in {:.1} ms: {}",
                t.elapsed().as_secs_f64() * 1000.0,
                rows.as_ref().map_or("Windows' menu".into(), |r| format!("{} rows", r.len()))
            )
        });
        let style = rows.as_ref().and_then(|_| crate::ui::with_app(crate::ui::menu::Style::of));
        let cmd = match (rows, style) {
            (Some(rows), Some(style)) if !rows.is_empty() => {
                crate::ui::menu::track_rows(owner, style, rows, x, y).map_or(0, |id| id as i32)
            }
            _ => {
                MENU.with(|m| *m.borrow_mut() = Some(cm.clone()));
                let cmd = TrackPopupMenuEx(menu, (TPM_RETURNCMD | TPM_RIGHTBUTTON).0, x, y, hwnd, None).0;
                MENU.with(|m| *m.borrow_mut() = None);
                cmd
            }
        };
        if cmd > 0 {
            let dir = wstr(folder);
            let info = CMINVOKECOMMANDINFOEX {
                cbSize: std::mem::size_of::<CMINVOKECOMMANDINFOEX>() as u32,
                fMask: CMIC_MASK_UNICODE | CMIC_MASK_PTINVOKE,
                hwnd,
                lpVerb: PCSTR((cmd - 1) as usize as *const u8),
                lpVerbW: PCWSTR((cmd - 1) as usize as *const u16),
                lpDirectoryW: PCWSTR(dir.as_ptr()),
                nShow: SW_SHOWNORMAL.0,
                ptInvoke: POINT { x, y },
                ..Default::default()
            };
            let _ = cm.InvokeCommand(&info as *const _ as *const CMINVOKECOMMANDINFO);
        }
        let _ = DestroyMenu(menu);
    }
}

/// asks the shell to fill a submenu, as it does when the submenu is about to open ("Send to", "Open with")
unsafe fn fill_submenu(cm: &IContextMenu, sub: HMENU, index: u32) {
    let (wp, lp) = (WPARAM(sub.0 as usize), LPARAM(index as isize));
    if let Ok(cm3) = cm.cast::<IContextMenu3>() {
        let mut r = LRESULT(0);
        let _ = cm3.HandleMenuMsg2(WM_INITMENUPOPUP, wp, lp, Some(&mut r));
    } else if let Ok(cm2) = cm.cast::<IContextMenu2>() {
        let _ = cm2.HandleMenuMsg(WM_INITMENUPOPUP, wp, lp);
    }
}

/// a menu item's own picture (a 32-bit bitmap the shell or an extension gave it), as premultiplied pixels
unsafe fn menu_bitmap(hbm: windows::Win32::Graphics::Gdi::HBITMAP) -> Option<std::rc::Rc<crate::ui::thumbs::Image>> {
    use windows::Win32::Graphics::Gdi::*;
    let mut bm = BITMAP::default();
    if GetObjectW(hbm.into(), std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut BITMAP as *mut _)) == 0
        || bm.bmBitsPixel != 32
    {
        return None;
    }
    let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
    if w <= 0 || h <= 0 || w > 64 || h > 64 {
        return None;
    }
    let mut bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut px = vec![0u32; (w * h) as usize];
    let dc = GetDC(None);
    let n = GetDIBits(dc, hbm, 0, h as u32, Some(px.as_mut_ptr() as *mut _), &mut bi, DIB_RGB_COLORS);
    ReleaseDC(None, dc);
    if n == 0 {
        return None;
    }
    // a bitmap with no alpha at all is opaque
    if px.iter().all(|p| p >> 24 == 0) {
        for p in px.iter_mut() {
            *p |= 0xFF00_0000;
        }
    }
    Some(std::rc::Rc::new(crate::ui::thumbs::Image { w, h, px: px.into_boxed_slice() }))
}

/// A label written for Windows' menus: the "&" before the underlined letter taken out (and that letter kept), the
/// keys after a tab kept apart.
fn menu_label(text: &str) -> (String, String, Option<char>) {
    let (text, key) = text.split_once('\t').unwrap_or((text, ""));
    let mut label = String::with_capacity(text.len());
    let mut mnemonic = None;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            match chars.next() {
                Some('&') => label.push('&'),
                Some(n) => {
                    mnemonic.get_or_insert(n);
                    label.push(n);
                }
                None => {}
            }
        } else {
            label.push(c);
        }
    }
    (label, key.to_string(), mnemonic)
}

/// The shell's menu as dvfiles' menu rows: each item's text, state and picture, its submenu filled first. None if an
/// item draws itself with no text, so that nothing goes missing (Windows' own menu is shown then).
unsafe fn read_menu(cm: &IContextMenu, menu: HMENU, depth: usize) -> Option<Vec<crate::ui::menu::Row>> {
    use crate::ui::menu::{Kind, Row};
    let n = GetMenuItemCount(Some(menu));
    let mut rows: Vec<Row> = Vec::new();
    for i in 0..n.max(0) as u32 {
        let mut buf = [0u16; 512];
        let mut mii = MENUITEMINFOW {
            cbSize: std::mem::size_of::<MENUITEMINFOW>() as u32,
            fMask: MIIM_FTYPE | MIIM_STATE | MIIM_ID | MIIM_SUBMENU | MIIM_STRING | MIIM_BITMAP,
            dwTypeData: windows::core::PWSTR(buf.as_mut_ptr()),
            cch: (buf.len() - 1) as u32,
            ..Default::default()
        };
        if GetMenuItemInfoW(menu, i, true, &mut mii).is_err() {
            return None;
        }
        if mii.fType.0 & MFT_SEPARATOR.0 != 0 {
            // no separator first, last or twice
            if rows.last().is_some_and(|r| !matches!(r.kind, Kind::Sep)) {
                rows.push(Row {
                    label: String::new(),
                    key: String::new(),
                    kind: Kind::Sep,
                    check: false,
                    off: true,
                    default: false,
                    icon: None,
                    mnemonic: None,
                    fill: None,
                });
            }
            continue;
        }
        let text = String::from_utf16_lossy(&buf[..(mii.cch as usize).min(buf.len())]);
        if text.trim().is_empty() {
            return None;
        }
        let (label, key, mnemonic) = menu_label(&text);
        let mut fill = None;
        let kind = if !mii.hSubMenu.is_invalid() && depth < 4 {
            if GetMenuItemCount(Some(mii.hSubMenu)) > 1 {
                // made already (7-Zip, Send to), though some only give their items text when told they open
                fill_submenu(cm, mii.hSubMenu, i);
                Kind::Sub(read_menu(cm, mii.hSubMenu, depth + 1)?)
            } else {
                // empty or one placeholder until it opens (WinRAR 120 ms, Open with 100 ms): read then, as
                // Windows' menu would, rather than before the menu shows
                let (cm, sub) = (cm.clone(), mii.hSubMenu);
                fill = Some(crate::ui::menu::Filler(std::rc::Rc::new(move || {
                    fill_submenu(&cm, sub, i);
                    read_menu(&cm, sub, depth + 1).unwrap_or_default()
                })));
                Kind::Sub(Vec::new())
            }
        } else {
            Kind::Item(mii.wID)
        };
        // HBMMENU_CALLBACK (-1) and the HBMMENU_ system pictures (1 to 11) are not bitmaps
        let hb = mii.hbmpItem.0 as isize;
        let icon = if hb > 11 { menu_bitmap(mii.hbmpItem) } else { None };
        let state = mii.fState.0;
        let off = state & (MFS_DISABLED.0 | MFS_GRAYED.0) != 0
            || (fill.is_none() && matches!(&kind, Kind::Sub(s) if s.is_empty()));
        rows.push(Row {
            label,
            key,
            kind,
            check: state & MFS_CHECKED.0 != 0,
            off,
            default: state & MFS_DEFAULT.0 != 0,
            icon,
            mnemonic,
            fill,
        });
    }
    while rows.last().is_some_and(|r| matches!(r.kind, Kind::Sep)) {
        rows.pop();
    }
    Some(rows)
}

/// lets the open shell menu draw and fill its owner-drawn submenus
pub fn menu_message(msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    if !matches!(msg, WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR) {
        return None;
    }
    MENU.with(|m| {
        let m = m.borrow();
        let cm = m.as_ref()?;
        unsafe {
            if let Ok(cm3) = cm.cast::<IContextMenu3>() {
                let mut r = LRESULT(0);
                cm3.HandleMenuMsg2(msg, wp, lp, Some(&mut r)).ok()?;
                return Some(r);
            }
            if let Ok(cm2) = cm.cast::<IContextMenu2>() {
                cm2.HandleMenuMsg(msg, wp, lp).ok()?;
                return Some(LRESULT(0));
            }
        }
        None
    })
}

/// DVFILES_SHELLTEST=1: the shell's menu for a scratch file, read into dvfiles' rows: nothing with text goes missing,
/// and the submenus the shell fills late ("Send to") come with their items
#[cfg(test)]
mod shell_menu_tests {
    use super::*;
    use crate::ui::menu::{Kind, Row};

    fn count(rows: &[Row]) -> usize {
        rows.iter()
            .map(|r| match &r.kind {
                Kind::Sep => 0,
                Kind::Sub(s) => 1 + count(s),
                Kind::Item(_) => 1,
            })
            .sum()
    }

    fn print(rows: &[Row], depth: usize) {
        for r in rows {
            match &r.kind {
                Kind::Sep => eprintln!("{}----", "  ".repeat(depth)),
                Kind::Item(id) => eprintln!(
                    "{}{} [{}]{}{}{}",
                    "  ".repeat(depth),
                    r.label,
                    id,
                    if r.off { " (off)" } else { "" },
                    if r.icon.is_some() { " (icon)" } else { "" },
                    r.mnemonic.map(|m| format!(" &{m}")).unwrap_or_default()
                ),
                Kind::Sub(s) => {
                    eprintln!("{}{} >{}", "  ".repeat(depth), r.label, if r.icon.is_some() { " (icon)" } else { "" });
                    print(s, depth + 1);
                }
            }
        }
    }

    #[test]
    fn shell_menu_reads_whole() {
        if std::env::var_os("DVFILES_SHELLTEST").is_none() {
            return;
        }
        ole_init();
        let f = std::env::temp_dir().join(format!("dvfiles_shellmenu_{}.txt", std::process::id()));
        std::fs::write(&f, "x").unwrap();
        let p = f.to_string_lossy().into_owned();
        unsafe {
            let t = std::time::Instant::now();
            let (parent, ids) = children(&[p.clone()]).unwrap();
            let list: Vec<*const ITEMIDLIST> = ids.0.iter().map(|&a| a as *const ITEMIDLIST).collect();
            let cm: IContextMenu = parent.GetUIObjectOf(HWND::default(), &list, None).unwrap();
            let menu = CreatePopupMenu().unwrap();
            cm.QueryContextMenu(menu, 0, 1, 0x7FFF, CMF_NORMAL | CMF_CANRENAME).unwrap();
            let queried = t.elapsed();
            let rows = read_menu(&cm, menu, 0);
            eprintln!("queried in {:?}, read in {:?}", queried, t.elapsed() - queried);
            let rows = rows.expect("every item has text");
            print(&rows, 0);
            assert!(count(&rows) >= 5, "only {} items", count(&rows));
            // a submenu the shell fills when it opens is filled when it opens here too
            for r in rows.iter().filter(|r| r.fill.is_some()) {
                let t = std::time::Instant::now();
                let sub = (r.fill.as_ref().unwrap().0)();
                eprintln!("{} filled in {:?}: {} rows", r.label, t.elapsed(), sub.len());
                assert!(!sub.is_empty(), "{} came empty", r.label);
            }
            assert!(
                rows.iter().all(|r| !matches!(&r.kind, Kind::Sub(s) if s.is_empty() && r.fill.is_none() && !r.off)),
                "an empty submenu"
            );
            let _ = DestroyMenu(menu);
        }
        let _ = std::fs::remove_file(&f);
    }
}
