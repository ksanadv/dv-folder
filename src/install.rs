// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Per-user install, no administrator rights: the program goes to `%LOCALAPPDATA%\Programs\dvfiles` with an Apps &
//! features entry, and the `Options` chosen in the setup: shortcuts, "Open in dvfiles" on folders and drives,
//! `dvfiles-cli` on the user's Path, and dvfiles as the default for folders. The uninstaller undoes all of it.
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};
use winreg::RegKey;

pub const APP_NAME: &str = "dvfiles";
pub const EXE_NAME: &str = "dvfiles.exe";
pub const CLI_NAME: &str = "dvfiles-cli.exe";
const VERB: &str = "dvfiles";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
// the shell's folder kinds dvfiles adds its verb to; "Folder" is left alone, since Control Panel, the Recycle Bin and
// other shell folders open through it
const FOLDER_CLASSES: [&str; 2] = ["Directory", "Drive"];

pub fn install_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("Programs").join(APP_NAME)
}

/// What the setup adds besides the program and its Apps & features entry; the setup shows a box for each.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Options {
    pub start_menu: bool,
    pub desktop: bool,
    /// `dvfiles-cli` on the user's Path
    pub path: bool,
    /// "Open in dvfiles" on folders' and drives' menus
    pub folder_menu: bool,
    /// folders open in dvfiles instead of File Explorer; it needs `folder_menu`
    pub default: bool,
}

impl Options {
    /// a new install's: everything but the desktop shortcut and taking folders over from Explorer
    pub const FRESH: Options =
        Options { start_menu: true, desktop: false, path: true, folder_menu: true, default: false };

    /// what the installed copy has now, so an update keeps it
    fn current() -> Options {
        let verb = hkcu().open_subkey_with_flags(format!(r"Software\Classes\Directory\shell\{VERB}"), KEY_READ).is_ok();
        Options {
            start_menu: start_menu_link().is_some_and(|p| p.is_file()),
            desktop: desktop_link().is_some_and(|p| p.is_file()),
            path: on_path(&install_dir()),
            folder_menu: verb,
            default: is_default_handler(),
        }
    }

    /// the installed copy's options, else a new install's
    pub fn for_install() -> Options {
        if installed_version().is_some() {
            Options::current()
        } else {
            Options::FRESH
        }
    }
}

fn start_menu_link() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs").join(format!("{APP_NAME}.lnk")))
}

/// the desktop as the user has it, which OneDrive may have moved
fn desktop_link() -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Desktop, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Desktop, KF_FLAG_DEFAULT, None).ok()?;
        let dir = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        Some(PathBuf::from(dir?).join(format!("{APP_NAME}.lnk")))
    }
}

fn hkcu() -> RegKey {
    RegKey::predef(HKEY_CURRENT_USER)
}

fn notify_shell() {
    use windows::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
}

pub fn installed_version() -> Option<String> {
    hkcu()
        .open_subkey_with_flags(format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{APP_NAME}"), KEY_READ)
        .ok()?
        .get_value("DisplayVersion")
        .ok()
}

/// A running exe cannot be overwritten but it can be renamed, so the old file is moved aside and the new one written
/// in its place. The old copy is deleted right away unless it is still running; the next update removes it then.
pub fn replace_exe(target: &Path, write: impl FnOnce(&Path) -> std::io::Result<()>) -> Result<(), String> {
    let old = target.with_extension("old");
    let _ = std::fs::remove_file(&old);
    let moved = target.exists();
    if moved {
        std::fs::rename(target, &old).map_err(|e| format!("Cannot replace {}: {e}", target.display()))?;
    }
    if let Err(e) = write(target) {
        if moved {
            let _ = std::fs::rename(&old, target);
        }
        return Err(format!("Cannot write {}: {e}", target.display()));
    }
    let _ = std::fs::remove_file(&old);
    Ok(())
}

#[allow(dead_code)]
pub fn install() -> Result<PathBuf, String> {
    let options = Options::for_install();
    let current = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = install_dir();
    let target = dir.join(EXE_NAME);
    let same = std::fs::canonicalize(&current).ok() == std::fs::canonicalize(&target).ok();
    if !same {
        std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
        replace_exe(&target, |t| std::fs::copy(&current, t).map(|_| ()))?;
        // the console tool travels with the window when it sits next to it
        if let Some(cli) = current.parent().map(|p| p.join(CLI_NAME)).filter(|p| p.is_file()) {
            replace_exe(&dir.join(CLI_NAME), |t| std::fs::copy(&cli, t).map(|_| ()))?;
        }
    }
    finish_install(&target, options)?;
    Ok(target)
}

/// Registers the program in place at `exe` and applies `o`, adding what it turns on and taking away what it turns off.
pub fn finish_install(exe: &Path, o: Options) -> Result<(), String> {
    let verb = o.folder_menu || o.default;
    register(exe).map_err(|e| format!("Registry write failed: {e}"))?;
    if verb {
        register_verb(exe).map_err(|e| format!("Registry write failed: {e}"))?;
    }
    set_default_handler(exe, o.default)?;
    if !verb {
        remove_verbs();
    }
    set_on_path(&install_dir(), o.path);
    for (lnk, on) in [(start_menu_link(), o.start_menu), (desktop_link(), o.desktop)] {
        let Some(lnk) = lnk else { continue };
        if on {
            write_shortcut(&lnk, exe).map_err(|e| format!("Cannot create {}: {e}", lnk.display()))?;
        } else {
            let _ = std::fs::remove_file(lnk);
        }
    }
    notify_shell();
    Ok(())
}

fn register_verb(exe: &Path) -> std::io::Result<()> {
    let exe = exe.display().to_string();
    for class in FOLDER_CLASSES {
        let (verb, _) = hkcu().create_subkey(format!(r"Software\Classes\{class}\shell\{VERB}"))?;
        verb.set_value("", &"Open in dvfiles")?;
        verb.set_value("Icon", &format!("\"{exe}\",0"))?;
        verb.create_subkey("command")?.0.set_value("", &format!("\"{exe}\" \"%1\""))?;
    }
    Ok(())
}

/// the install's size in KB and today as `YYYYMMDD`, which Apps & features shows
fn size_and_date() -> (u32, String) {
    let kb = std::fs::read_dir(install_dir())
        .map(|d| d.flatten().filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum::<u64>() / 1024)
        .unwrap_or(0);
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    (kb.min(u32::MAX as u64) as u32, format!("{:04}{:02}{:02}", t.wYear, t.wMonth, t.wDay))
}

fn register(exe: &Path) -> std::io::Result<()> {
    let exe_s = exe.display().to_string();
    let (un, _) = hkcu().create_subkey(format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{APP_NAME}"))?;
    un.set_value("DisplayName", &APP_NAME)?;
    un.set_value("DisplayVersion", &env!("CARGO_PKG_VERSION"))?;
    un.set_value("Publisher", &APP_NAME)?;
    un.set_value("DisplayIcon", &exe_s)?;
    un.set_value("InstallLocation", &install_dir().display().to_string())?;
    un.set_value("UninstallString", &format!("\"{exe_s}\" --uninstall"))?;
    let (kb, date) = size_and_date();
    un.set_value("EstimatedSize", &kb)?;
    un.set_value("InstallDate", &date)?;
    un.set_value("NoModify", &1u32)?;
    un.set_value("NoRepair", &1u32)?;
    Ok(())
}

/// a shortcut to `exe` at `lnk`, written by the shell's own link object
fn write_shortcut(lnk: &Path, exe: &Path) -> windows::core::Result<()> {
    use windows::core::{Interface, PCWSTR};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    let wide = |p: &Path| p.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let (exe_w, lnk_w) = (wide(exe), wide(lnk));
    unsafe {
        // false when this thread already has COM in another mode, which serves as well
        let own = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let r = (|| {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            link.SetPath(PCWSTR(exe_w.as_ptr()))?;
            link.SetIconLocation(PCWSTR(exe_w.as_ptr()), 0)?;
            link.cast::<IPersistFile>()?.Save(PCWSTR(lnk_w.as_ptr()), true)
        })();
        if own {
            CoUninitialize();
        }
        r
    }
}

fn broadcast_environment() {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
    };
    let env: Vec<u16> = "Environment".encode_utf16().chain(Some(0)).collect();
    unsafe {
        let _ = SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(env.as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            2000,
            None,
        );
    }
}

fn user_path() -> (Option<RegKey>, String) {
    let key = hkcu().open_subkey_with_flags("Environment", KEY_READ | KEY_SET_VALUE).ok();
    let path: String = key.as_ref().and_then(|k| k.get_value("Path").ok()).unwrap_or_default();
    (key, path)
}

fn on_path(dir: &Path) -> bool {
    has_entry(&user_path().1, &dir.display().to_string())
}

/// `dir` is in the `;`-separated `path`, with or without a trailing `\`, in any case
fn has_entry(path: &str, dir: &str) -> bool {
    path.split(';').any(|p| p.trim_end_matches('\\').eq_ignore_ascii_case(dir))
}

/// `path` with `dir` added at the end or taken out; left as it is when it already is, so the user's order stays
fn path_with(path: &str, dir: &str, on: bool) -> String {
    if has_entry(path, dir) == on {
        return path.to_string();
    }
    let mut kept: Vec<&str> =
        path.split(';').filter(|p| !p.is_empty() && !p.trim_end_matches('\\').eq_ignore_ascii_case(dir)).collect();
    if on {
        kept.push(dir);
    }
    kept.join(";")
}

fn set_on_path(dir: &Path, on: bool) {
    let (Some(key), path) = user_path() else { return };
    let new = path_with(&path, &dir.display().to_string(), on);
    if new != path {
        // keep it expandable, as Windows stores the user Path
        let bytes: Vec<u8> = new.encode_utf16().chain(Some(0)).flat_map(|u| u.to_le_bytes()).collect();
        let _ =
            key.set_raw_value("Path", &winreg::RegValue { vtype: winreg::enums::REG_EXPAND_SZ, bytes: bytes.into() });
        broadcast_environment();
    }
}

pub fn is_default_handler() -> bool {
    hkcu()
        .open_subkey_with_flags(r"Software\Classes\Directory\shell", KEY_READ)
        .ok()
        .and_then(|k| k.get_value::<String, _>("").ok())
        .is_some_and(|v| v.eq_ignore_ascii_case(VERB))
}

/// Makes double-clicking a folder or drive (on the desktop, in shortcuts, from other programs) open dvfiles, or
/// gives that back to Explorer. The previous default verb is kept in `HKCU\Software\dvfiles` and restored. `exe` is
/// the dvfiles the verb runs.
pub fn set_default_handler(exe: &Path, on: bool) -> Result<(), String> {
    let root = hkcu();
    let (backup, _) =
        root.create_subkey(format!(r"Software\{APP_NAME}\DefaultVerbBackup")).map_err(|e| e.to_string())?;
    if on {
        register_verb(exe).map_err(|e| e.to_string())?;
        for class in FOLDER_CLASSES {
            let (shell, _) =
                root.create_subkey(format!(r"Software\Classes\{class}\shell")).map_err(|e| e.to_string())?;
            let prev: Option<String> = shell.get_value("").ok();
            if prev.as_deref().is_some_and(|p| p.eq_ignore_ascii_case(VERB)) {
                continue;
            }
            let _ = backup.set_value(class, &prev.unwrap_or_else(|| "<none>".into()));
            shell.set_value("", &VERB).map_err(|e| e.to_string())?;
        }
    } else {
        for class in FOLDER_CLASSES {
            let Ok(shell) =
                root.open_subkey_with_flags(format!(r"Software\Classes\{class}\shell"), KEY_READ | KEY_SET_VALUE)
            else {
                continue;
            };
            let current: Option<String> = shell.get_value("").ok();
            if !current.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(VERB)) {
                continue;
            }
            match backup.get_value::<String, _>(class) {
                Ok(prev) if prev != "<none>" => shell.set_value("", &prev).map_err(|e| e.to_string())?,
                _ => {
                    let _ = shell.delete_value("");
                }
            }
            let _ = backup.delete_value(class);
        }
        // a portable copy leaves nothing behind: its "Open in dvfiles" verb and its search index go too
        if installed_version().is_none() {
            remove_verbs();
            if let Some(local) = std::env::var_os("LOCALAPPDATA") {
                let _ = std::fs::remove_dir_all(PathBuf::from(local).join(APP_NAME));
            }
            let _ = root.delete_subkey(format!(r"Software\{APP_NAME}\DefaultVerbBackup"));
            let _ = root.delete_subkey(format!(r"Software\{APP_NAME}"));
        }
    }
    notify_shell();
    Ok(())
}

/// the settings and the search index (which lists every file name on the drive, so it never stays behind)
fn remove_data() {
    for var in ["LOCALAPPDATA", "APPDATA"] {
        if let Some(base) = std::env::var_os(var) {
            let _ = std::fs::remove_dir_all(PathBuf::from(base).join(APP_NAME));
        }
    }
}

/// removes the verb, and the `shell` and class keys it needed when nothing else is in them
fn remove_verbs() {
    let root = hkcu();
    for class in FOLDER_CLASSES {
        let _ = root.delete_subkey_all(format!(r"Software\Classes\{class}\shell\{VERB}"));
        for key in [format!(r"Software\Classes\{class}\shell"), format!(r"Software\Classes\{class}")] {
            let empty = root
                .open_subkey_with_flags(&key, KEY_READ)
                .ok()
                .and_then(|k| k.query_info().ok())
                .is_some_and(|i| i.sub_keys == 0 && i.values == 0);
            if empty {
                let _ = root.delete_subkey(&key);
            }
        }
    }
}

pub fn uninstall() -> Result<(), String> {
    let _ = set_default_handler(&install_dir().join(EXE_NAME), false);
    let root = hkcu();
    remove_verbs();
    for key in
        [format!(r"Software\{APP_NAME}"), format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{APP_NAME}")]
    {
        let _ = root.delete_subkey_all(key);
    }
    for lnk in [start_menu_link(), desktop_link()].into_iter().flatten() {
        let _ = std::fs::remove_file(lnk);
    }
    set_on_path(&install_dir(), false);
    notify_shell();
    remove_data();
    let dir = install_dir();
    if dir.join(EXE_NAME).is_file() {
        let cmd = format!("/C ping -n 3 127.0.0.1 >nul & rmdir /s /q \"{}\"", dir.display());
        std::process::Command::new("cmd")
            // cmd.exe needs raw quotes
            .raw_arg(&cmd)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map_err(|e| format!("Unregistered, but could not remove {}: {e}", dir.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_entries() {
        let d = r"C:\P\dvfiles";
        assert_eq!(path_with(r"C:\a;C:\b", d, true), r"C:\a;C:\b;C:\P\dvfiles");
        assert_eq!(path_with("", d, true), d);
        // already there (any case, a trailing slash): untouched, order kept
        assert_eq!(path_with(r"c:\p\DVFILES\;C:\a", d, true), r"c:\p\DVFILES\;C:\a");
        assert_eq!(path_with(r"C:\a;C:\P\dvfiles\;;C:\b", d, false), r"C:\a;C:\b");
        assert_eq!(path_with(r"C:\a;C:\b", d, false), r"C:\a;C:\b");
    }

    #[test]
    fn shortcut_points_at_its_exe() {
        let dir = std::env::temp_dir().join("dvfiles_lnktest");
        std::fs::create_dir_all(&dir).unwrap();
        let exe = std::env::current_exe().unwrap();
        let lnk = dir.join("test.lnk");
        write_shortcut(&lnk, &exe).unwrap();
        // read back through the shell's link object (the installer shares this file, so not dvfiles' own reader)
        let target = unsafe {
            use windows::core::{Interface, PCWSTR};
            use windows::Win32::System::Com::{
                CoCreateInstance, CoInitializeEx, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
                STGM_READ,
            };
            use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            let w: Vec<u16> = lnk.as_os_str().encode_wide().chain(Some(0)).collect();
            link.cast::<IPersistFile>().unwrap().Load(PCWSTR(w.as_ptr()), STGM_READ).unwrap();
            let mut buf = [0u16; 1024];
            link.GetPath(&mut buf, std::ptr::null_mut(), 0).unwrap();
            String::from_utf16_lossy(&buf[..buf.iter().position(|&c| c == 0).unwrap_or(0)])
        };
        let _ = std::fs::remove_dir_all(&dir);
        assert!(target.eq_ignore_ascii_case(&exe.display().to_string()), "{target}");
    }
}
