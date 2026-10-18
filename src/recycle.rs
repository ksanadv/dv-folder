// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Moving items to the Recycle Bin by writing the bin's own records.
//!
//! The shell recycles a folder by walking it and asking each item's handlers; for a folder of thousands of files that
//! takes seconds. On disk the result is simple (see `recyclefmt`): the item is renamed into
//! `<volume>\$Recycle.Bin\<user SID>` and a small `$I` record keeps its old path, size and deletion time. dvfiles
//! writes exactly that, so recycling costs one rename and one tiny file per selected item, whatever is inside, and
//! Explorer's Recycle Bin lists and restores the items as its own.
//!
//! The shell still does it (`SHFileOperationW`, which also asks before deleting anything for good) when:
//! - the item is in a OneDrive or other cloud folder, whose sync engine has to see the delete;
//! - the volume has no bin for this user yet, or is a removable or network drive;
//! - "Don't move files to the Recycle Bin" is set, or the item does not fit in the bin's size limit.
use crate::fsx::{self, FA_PINNED, FA_UNPINNED};
use crate::ops::Progress;
use crate::pathx;
use crate::recyclefmt::{self, Record, Rng};
use crate::wide::{long, pcwstr, wide};
use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use windows::Win32::Foundation::{LocalFree, HLOCAL, HWND};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_QUERY, TOKEN_USER};
use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVE_FILE_FLAGS};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// the window the shell's fallback dialogs belong to
pub static OWNER: AtomicIsize = AtomicIsize::new(0);

pub fn user_sid() -> Option<String> {
    unsafe {
        let mut token = Default::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let token = fsx::Handle(token);
        let mut buf = vec![0u64; 64];
        let mut len = 0u32;
        GetTokenInformation(token.0, TokenUser, Some(buf.as_mut_ptr() as *mut _), (buf.len() * 8) as u32, &mut len)
            .ok()?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut s = windows::core::PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut s).ok()?;
        let text = s.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(s.0 as *mut _)));
        text
    }
}

struct Bin {
    dir: String,
    // bytes still free under the user's size limit for this volume
    room: u64,
}

fn bucket_settings(root: &str) -> (Option<u64>, bool) {
    use winreg::enums::HKEY_CURRENT_USER;
    let Some(guid) = fsx::volume_guid(root) else { return (None, false) };
    let Some(id) = guid.find('{').map(|i| guid[i..].trim_end_matches('\\').to_string()) else { return (None, false) };
    let key = winreg::RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket\Volume\{id}"));
    match key {
        Ok(k) => {
            let max: Option<u32> = k.get_value("MaxCapacity").ok();
            let nuke: u32 = k.get_value("NukeOnDelete").unwrap_or(0);
            (max.map(|mb| mb as u64 * 1024 * 1024), nuke != 0)
        }
        Err(_) => (None, false),
    }
}

// what the bin holds now: $R files by their size, $R folders by the size their $I record gives
fn bin_usage(dir: &str) -> u64 {
    let Ok(entries) = fsx::list(dir) else { return 0 };
    let mut used = 0u64;
    for e in &entries {
        let Some(id) = e.name.strip_prefix("$R") else { continue };
        if e.is_dir() {
            if let Ok(b) = std::fs::read(pathx::join(dir, &format!("$I{id}"))) {
                used += recyclefmt::decode(&b).map_or(0, |r| r.size);
            }
        } else {
            used += e.size;
        }
    }
    used
}

fn open_bin(root: &str, sid: &str) -> Option<Bin> {
    if fsx::drive_kind(root) != fsx::DriveKind::Fixed {
        return None;
    }
    let dir =
        format!(r"{}$Recycle.Bin\{sid}", if root.ends_with('\\') { root.to_string() } else { format!("{root}\\") });
    if !fsx::is_dir(&dir) {
        return None;
    }
    let (max, nuke) = bucket_settings(root);
    if nuke {
        return None;
    }
    // without a stored limit Windows uses about 5% of the volume
    let capacity = max.unwrap_or_else(|| fsx::drive_info(root).total / 20);
    let room = capacity.saturating_sub(bin_usage(&dir));
    Some(Bin { dir, room })
}

fn cloud_roots() -> Vec<String> {
    ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .filter(|p| !p.is_empty())
        .collect()
}

fn in_cloud(path: &str, attrs: u32, roots: &[String]) -> bool {
    fsx::is_cloud_only(attrs)
        || attrs & (FA_PINNED | FA_UNPINNED) != 0
        || roots.iter().any(|r| pathx::is_within(path, r))
}

/// moves one item into the bin; returns its `$R` entry and `$I` record
fn recycle_one(bin: &Bin, src: &str, size: u64, now: u64, rng: &mut Rng) -> std::io::Result<(String, String)> {
    let ext = recyclefmt::bin_ext(pathx::file_name(src));
    for _ in 0..32 {
        let id = recyclefmt::id(rng.next());
        let r = pathx::join(&bin.dir, &format!("$R{id}{ext}"));
        let i = pathx::join(&bin.dir, &format!("$I{id}{ext}"));
        if fsx::exists(&r) || fsx::exists(&i) {
            continue;
        }
        let record = recyclefmt::encode(&Record { size, deleted: now, path: src.to_string() });
        // another job may have taken this id between the check and now: then try the next one
        let mut f = match std::fs::OpenOptions::new().write(true).create_new(true).open(&i) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        };
        f.write_all(&record)?;
        drop(f);
        // a plain rename: it fails rather than copying if the bin were on another volume
        let moved = unsafe { MoveFileExW(pcwstr(&long(src)), pcwstr(&long(&r)), MOVE_FILE_FLAGS(0)) };
        if let Err(e) = moved {
            let _ = std::fs::remove_file(&i);
            return Err(fsx::win_err(e));
        }
        return Ok((r, i));
    }
    Err(std::io::Error::other("No free name in the Recycle Bin."))
}

fn notify_deleted(path: &str, is_dir: bool) {
    use windows::Win32::UI::Shell::{SHChangeNotify, SHCNE_DELETE, SHCNE_RMDIR, SHCNF_FLUSHNOWAIT, SHCNF_PATHW};
    let w = wide(path);
    unsafe {
        SHChangeNotify(
            if is_dir { SHCNE_RMDIR } else { SHCNE_DELETE },
            SHCNF_PATHW | SHCNF_FLUSHNOWAIT,
            Some(w.as_ptr() as *const _),
            None,
        );
    }
}

/// `SHFileOperationW` with undo: the shell recycles, or warns before deleting what cannot be recycled
pub fn shell_recycle(paths: &[String]) -> Result<bool, String> {
    use windows::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_WANTNUKEWARNING, FO_DELETE, SHFILEOPSTRUCTW,
    };
    let mut from: Vec<u16> = Vec::new();
    for p in paths {
        from.extend(p.encode_utf16());
        from.push(0);
    }
    from.push(0);
    let mut op = SHFILEOPSTRUCTW {
        hwnd: HWND(OWNER.load(Ordering::Relaxed) as *mut _),
        wFunc: FO_DELETE,
        pFrom: pcwstr(&from),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_WANTNUKEWARNING).0 as u16,
        ..Default::default()
    };
    let r = unsafe { SHFileOperationW(&mut op) };
    if r != 0 {
        return Err(format!("Windows could not recycle these items (error {r:#x})."));
    }
    Ok(!op.fAnyOperationsAborted.as_bool())
}

/// tells the shell the bin changed (its icon and any open Recycle Bin window)
pub fn refresh_bin() {
    update_bin_icon();
}

// the desktop's full/empty bin icon; exported by shell32 but not in the Windows metadata
fn update_bin_icon() {
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    unsafe {
        let Ok(m) = LoadLibraryW(windows::core::w!("shell32.dll")) else { return };
        if let Some(f) = GetProcAddress(m, windows::core::s!("SHUpdateRecycleBinIcon")) {
            let update: extern "system" fn() = std::mem::transmute(f);
            update();
        }
    }
}

pub fn recycle(sources: &[String], p: &Progress) {
    let sid = user_sid();
    let roots = cloud_roots();
    let now = fsx::filetime_now();
    // the clock alone repeats when two jobs start in the same millisecond, so a process-wide counter is mixed in
    static RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let run = RUNS.fetch_add(1, Ordering::Relaxed);
    let mut rng = Rng::new(now ^ ((std::process::id() as u64) << 32) ^ run.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    let mut bins: HashMap<String, Option<Bin>> = HashMap::new();
    let mut by_shell: Vec<String> = Vec::new();
    let mut any = false;
    for src in sources {
        if p.cancelled() {
            break;
        }
        p.total_items.fetch_add(1, Ordering::Relaxed);
        let Some(attrs) = fsx::attributes(src) else {
            p.errors.lock().unwrap().push((src.clone(), "The item could not be found.".into()));
            p.done_items.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        if let Ok(mut c) = p.current.try_lock() {
            *c = pathx::file_name(src).to_string();
        }
        let root = fsx::volume_root(src).unwrap_or_default();
        let bin = match (&sid, in_cloud(src, attrs, &roots)) {
            (Some(sid), false) => bins.entry(root.to_lowercase()).or_insert_with(|| open_bin(&root, sid)),
            _ => {
                by_shell.push(src.clone());
                continue;
            }
        };
        let Some(bin) = bin.as_mut() else {
            by_shell.push(src.clone());
            continue;
        };
        let is_dir = attrs & fsx::FA_DIRECTORY != 0;
        let size = if is_dir && attrs & fsx::FA_REPARSE == 0 {
            let live = crate::walker::LiveTotals::default();
            crate::walker::measure(&[src.clone()], 4096, &AtomicBool::new(false), &live);
            live.get().bytes
        } else {
            fsx::stat(src).map_or(0, |e| e.size)
        };
        if size > bin.room {
            by_shell.push(src.clone());
            continue;
        }
        match recycle_one(bin, src, size, now, &mut rng) {
            Ok((r, i)) => {
                p.undo.lock().unwrap().recycled.push((src.clone(), r, i));
                bin.room -= size;
                any = true;
                notify_deleted(src, is_dir);
                p.results.lock().unwrap().push(src.clone());
            }
            Err(e) => p.errors.lock().unwrap().push((src.clone(), e.to_string())),
        }
        p.done_items.fetch_add(1, Ordering::Relaxed);
    }
    if !by_shell.is_empty() && !p.cancelled() {
        // the shell's part cannot be undone by dvfiles
        p.undo.lock().unwrap().inexact = true;
        if let Err(e) = shell_recycle(&by_shell) {
            p.errors.lock().unwrap().push((by_shell[0].clone(), e));
        }
        p.done_items.fetch_add(by_shell.len() as u64, Ordering::Relaxed);
        any = true;
    }
    if any {
        update_bin_icon();
    }
}

/// Where a recycled item went, for the tests: the `$I` records in a bin whose path is `original`.
#[cfg(test)]
pub fn find_records(original: &str) -> Vec<(String, Record)> {
    let (Some(sid), Some(root)) = (user_sid(), fsx::volume_root(original)) else { return Vec::new() };
    let dir = format!(r"{}$Recycle.Bin\{sid}", root);
    let Ok(entries) = fsx::list(&dir) else { return Vec::new() };
    entries
        .iter()
        .filter(|e| e.name.starts_with("$I"))
        .filter_map(|e| {
            let path = pathx::join(&dir, &e.name);
            let rec = recyclefmt::decode(&std::fs::read(&path).ok()?)?;
            pathx::same(&rec.path, original).then_some((path, rec))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn sid_looks_right() {
        let s = user_sid().unwrap();
        assert!(s.starts_with("S-1-5-"), "{s}");
    }

    // Recycles a scratch file and folder into the real bin of the temp folder's volume, checks the records, then
    // removes exactly those entries from the bin again.
    #[test]
    fn recycles_into_the_bin_and_cleans_up() {
        let base = std::env::temp_dir().join(format!("dvfiles_recycle_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("folder.v2").join("deep")).unwrap();
        std::fs::write(base.join("folder.v2").join("deep").join("a.bin"), vec![1u8; 5000]).unwrap();
        std::fs::write(base.join("note.txt"), vec![2u8; 1234]).unwrap();
        let base_s = base.to_str().unwrap().to_string();
        let roots = cloud_roots();
        if roots.iter().any(|r| pathx::is_within(&base_s, r)) {
            eprintln!("temp folder is inside OneDrive; skipped");
            return;
        }
        let items = vec![format!(r"{base_s}\note.txt"), format!(r"{base_s}\folder.v2")];
        let p = Progress::default();
        recycle(&items, &p);
        assert!(p.errors.lock().unwrap().is_empty(), "{:?}", p.errors.lock().unwrap());
        assert_eq!(p.done_items.load(Ordering::Relaxed), 2);
        for (item, size) in items.iter().zip([1234u64, 5000]) {
            assert!(!fsx::exists(item), "{item} is still there");
            let recs = find_records(item);
            if recs.is_empty() {
                // the shell took it (no bin on this volume): nothing of ours to check or clean
                continue;
            }
            let (ipath, rec) = &recs[0];
            assert_eq!(rec.size, size);
            let rpath = ipath.replacen(r"\$I", r"\$R", 1);
            assert!(fsx::exists(&rpath));
            if fsx::is_dir(&rpath) {
                std::fs::remove_dir_all(&rpath).unwrap();
            } else {
                std::fs::remove_file(&rpath).unwrap();
            }
            std::fs::remove_file(ipath).unwrap();
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}
