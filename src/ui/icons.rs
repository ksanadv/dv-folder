// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Type names ("JPG File") come from the shell, once per extension, on a thread. Icons are dvfiles' own (see `art`),
//! except for the files that carry their own: programs, icon files, shortcuts and Internet shortcuts. Those are read
//! from the files themselves on the same thread, at the size painting asks for: `PrivateExtractIconsW` reads a
//! program's icon resources directly, about three times quicker than asking the shell (measured: 0.34 against 1.2 ms
//! a program), and no system image list is kept. A shortcut shows the icon it names, else its target's, and dvfiles
//! draws the arrow; the shell is asked only about shortcuts to its own places (Control Panel and the like).
//!
//! A file type whose program gives it an icon of its own (dvnote's for .md, VLC's for .mp3, Word's for .docx) shows
//! that icon, as in Explorer, found once per type and size through the same association Windows uses. Types whose
//! icon is Windows' own (in the Windows folder, or one of Microsoft's own Store apps) keep dvfiles' icon.
use crate::ui::art::{self, Art};
use crate::ui::thumbs::Image;
use crate::ui::{post_box, wstr};
use dvfiles::fsx::{Entry, FA_DIRECTORY};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::mpsc::{channel, Sender};
use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, PrivateExtractIconsW, HICON, ICONINFO};

pub const WM_ICONS: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 10;

enum Req {
    Ext(Vec<String>),
    Own(Vec<(String, i32)>),
    Type(Vec<(String, i32)>),
}

/// what a file's own icon turned out to be: its picture, or (no icon in it) the art to show instead
pub enum Got {
    Image(Image),
    Art(Art),
}

pub enum Reply {
    Ext(Vec<(String, String)>),
    Own(Vec<((String, i32), Got)>),
    Type(Vec<((String, i32), Option<Image>)>),
}

/// a file's own icon as painting sees it
#[derive(Clone)]
pub enum Own {
    Pending,
    Image(Rc<Image>),
    Art(Art),
}

pub struct Icons {
    pub small_px: i32,
    pub large_px: i32,
    by_ext: HashMap<String, Rc<str>>,
    own: HashMap<(String, i32), Own>,
    pending_ext: HashSet<String>,
    pending_own: HashSet<(String, i32)>,
    queued_ext: Vec<String>,
    queued_own: Vec<(String, i32)>,
    // a file type's icon from its program, by extension and size: None when the type keeps dvfiles' icon
    by_type: HashMap<(String, i32), Option<Rc<Image>>>,
    pending_type: HashSet<(String, i32)>,
    queued_type: Vec<(String, i32)>,
    tx: Sender<Req>,
}

// extensions whose icon is inside each file
pub fn per_file(ext: &str) -> bool {
    matches!(ext, "exe" | "lnk" | "ico" | "cur" | "ani" | "url" | "scr" | "msc" | "cpl" | "appref-ms" | "website")
}

pub fn ext_key(e: &Entry) -> String {
    if e.is_dir() {
        "<dir>".into()
    } else {
        dvfiles::naming::ext_lower(&e.name)
    }
}

fn type_name(path: &str, attrs: u32) -> String {
    let mut info = SHFILEINFOW::default();
    let w = wstr(path);
    let ok = unsafe {
        SHGetFileInfoW(
            PCWSTR(w.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(attrs),
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_TYPENAME | SHGFI_USEFILEATTRIBUTES,
        )
    };
    if ok == 0 {
        return String::new();
    }
    crate::ui::from_wide(&info.szTypeName)
}

/// a bitmap's pixels as 32-bit BGRA, top row first
unsafe fn dib(hbm: HBITMAP) -> Option<(i32, i32, Vec<u32>)> {
    let mut bm = BITMAP::default();
    if GetObjectW(hbm.into(), std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _)) == 0 {
        return None;
    }
    let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
    if w <= 0 || h <= 0 || w > 1024 || h > 1024 {
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
    let dc = CreateCompatibleDC(None);
    let got = GetDIBits(dc, hbm, 0, h as u32, Some(px.as_mut_ptr() as *mut _), &mut bi, DIB_RGB_COLORS);
    let _ = DeleteDC(dc);
    (got != 0).then_some((w, h, px))
}

/// An icon's picture, premultiplied: with its own alpha when it has one (premultiplied here, icons store it
/// straight), else made transparent where its mask says so.
unsafe fn icon_image(h: HICON) -> Option<Image> {
    let mut ii = ICONINFO::default();
    GetIconInfo(h, &mut ii).ok()?;
    let r = (|| {
        if ii.hbmColor.is_invalid() {
            return None;
        }
        let (w, hh, mut px) = dib(ii.hbmColor)?;
        if px.iter().any(|p| p >> 24 != 0) {
            for p in &mut px {
                let a = *p >> 24;
                let m = |c: u32| (c * a + 127) / 255;
                *p = a << 24 | m(*p >> 16 & 0xFF) << 16 | m(*p >> 8 & 0xFF) << 8 | m(*p & 0xFF);
            }
        } else {
            let (_, _, mask) = dib(ii.hbmMask)?;
            for (p, m) in px.iter_mut().zip(mask.iter()) {
                *p = if m & 0xFF_FFFF == 0 { 0xFF00_0000 | (*p & 0xFF_FFFF) } else { 0 };
            }
        }
        Some(Image { w, h: hh, px: px.into_boxed_slice() })
    })();
    let _ = DeleteObject(ii.hbmColor.into());
    let _ = DeleteObject(ii.hbmMask.into());
    r
}

/// the largest of an icon group's pictures (`GRPICONDIR` in a program, `ICONDIR` in an .ico file), in pixels: 256
/// for the big frame, written as 0
fn largest_in(dir: &[u8], entry: usize) -> Option<i32> {
    let count = u16::from_le_bytes([*dir.get(4)?, *dir.get(5)?]) as usize;
    (0..count).filter_map(|k| dir.get(6 + entry * k).map(|&w| if w == 0 { 256 } else { w as i32 })).max()
}

/// The largest picture icon `index` of a file holds, so that an icon without big pictures is shown at its own size
/// in the middle of its box, as Explorer does, rather than blown up and blurred.
fn largest_frame(file: &str, index: i32) -> Option<i32> {
    use windows::Win32::System::LibraryLoader::*;
    if dvfiles::naming::ext_lower(file) == "ico" {
        let mut head = [0u8; 6 + 16 * 32];
        let n = std::io::Read::read(&mut std::fs::File::open(file).ok()?, &mut head).ok()?;
        return largest_in(&head[..n], 16);
    }
    struct Find {
        left: i32,
        largest: Option<i32>,
    }
    unsafe fn group(m: windows::Win32::Foundation::HMODULE, name: PCWSTR) -> Option<i32> {
        let res = FindResourceW(Some(m), name, windows::Win32::UI::WindowsAndMessaging::RT_GROUP_ICON);
        if res.is_invalid() {
            return None;
        }
        let p = LockResource(LoadResource(Some(m), res).ok()?) as *const u8;
        if p.is_null() {
            return None;
        }
        largest_in(std::slice::from_raw_parts(p, SizeofResource(Some(m), res) as usize), 14)
    }
    unsafe extern "system" fn each(
        m: windows::Win32::Foundation::HMODULE,
        _: PCWSTR,
        name: PCWSTR,
        lp: isize,
    ) -> windows::core::BOOL {
        let f = &mut *(lp as *mut Find);
        if f.left > 0 {
            f.left -= 1;
            return true.into();
        }
        f.largest = group(m, name);
        false.into()
    }
    unsafe {
        let m = LoadLibraryExW(
            PCWSTR(wstr(file).as_ptr()),
            None,
            LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE,
        )
        .ok()?;
        let largest = if index < 0 {
            // a resource id
            group(m, PCWSTR((-index) as usize as *const u16))
        } else {
            let mut f = Find { left: index, largest: None };
            let _ = EnumResourceNamesW(
                Some(m),
                windows::Win32::UI::WindowsAndMessaging::RT_GROUP_ICON,
                Some(each),
                &mut f as *mut Find as isize,
            );
            f.largest
        };
        let _ = windows::Win32::Foundation::FreeLibrary(m);
        largest
    }
}

/// icon `index` of a program or icon file, at `size` or at its own largest size if that is smaller, read from the file
pub fn extract(file: &str, index: i32, size: i32) -> Option<Image> {
    let size = largest_frame(file, index).map_or(size, |l| size.min(l.max(16)));
    let units: Vec<u16> = file.encode_utf16().collect();
    if units.len() >= 260 {
        return None;
    }
    let mut name = [0u16; 260];
    name[..units.len()].copy_from_slice(&units);
    let mut icons = [HICON::default(); 1];
    let n = unsafe { PrivateExtractIconsW(&name, index, size, size, Some(&mut icons), None, 0) };
    if n == 0 || n == u32::MAX || icons[0].is_invalid() {
        return None;
    }
    let img = unsafe { icon_image(icons[0]) };
    unsafe {
        let _ = DestroyIcon(icons[0]);
    }
    img
}

/// the shell's icon for a path, for what dvfiles cannot read itself (shortcuts to Control Panel and such)
fn shell_icon(path: &str, size: i32) -> Option<Image> {
    let mut info = SHFILEINFOW::default();
    let w = wstr(path);
    let flags = SHGFI_ICON | if size <= 24 { SHGFI_SMALLICON } else { SHGFI_LARGEICON };
    let ok = unsafe {
        SHGetFileInfoW(
            PCWSTR(w.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            flags,
        )
    };
    if ok == 0 || info.hIcon.is_invalid() {
        return None;
    }
    let img = unsafe { icon_image(info.hIcon) };
    unsafe {
        let _ = DestroyIcon(info.hIcon);
    }
    img
}

/// what an Internet shortcut names as its icon: `IconFile=` and `IconIndex=` in its [InternetShortcut] section
fn url_icon(path: &str) -> Option<(String, i32)> {
    let text = std::fs::read(path).ok().filter(|b| b.len() < 64 * 1024)?;
    let text = String::from_utf8_lossy(&text);
    let (mut file, mut index) = (None, 0);
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("IconFile=") {
            file = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("IconIndex=") {
            index = v.trim().parse().unwrap_or(0);
        }
    }
    file.filter(|f| !f.is_empty() && !f.contains("://")).map(|f| (dvfiles::fsx::expand_env(&f), index))
}

/// the art for what a shortcut points at, when it names no icon and its target has none of its own
fn art_for_target(t: &str) -> Art {
    if t.trim_end_matches('\\').len() <= 2 {
        Art::Drive
    } else if dvfiles::fsx::is_dir(t) {
        Art::Folder
    } else {
        Art::File(art::kind_of(&dvfiles::naming::ext_lower(t)))
    }
}

/// what a file with its own icon shows
fn own_icon(path: &str, size: i32) -> Got {
    let ext = dvfiles::naming::ext_lower(path);
    let image = |x: Option<Image>, otherwise: Art| x.map_or(Got::Art(otherwise), Got::Image);
    match ext.as_str() {
        "lnk" => {
            if let Some(img) = dvfiles::lnk::icon(path).and_then(|(f, i)| extract(&f, i, size)) {
                return Got::Image(img);
            }
            match dvfiles::lnk::target(path) {
                Some(t) => {
                    let te = dvfiles::naming::ext_lower(&t);
                    if per_file(&te) && te != "lnk" && !dvfiles::fsx::is_dir(&t) {
                        image(extract(&t, 0, size), Art::File(art::Kind::App))
                    } else {
                        Got::Art(art_for_target(&t))
                    }
                }
                // a shortcut to one of the shell's own places
                None => image(shell_icon(path, size), Art::File(art::Kind::Generic)),
            }
        }
        "url" | "website" => image(url_icon(path).and_then(|(f, i)| extract(&f, i, size)), Art::File(art::Kind::Web)),
        "exe" | "scr" | "cpl" => image(extract(path, 0, size), Art::File(art::Kind::App)),
        "ico" | "cur" | "ani" => image(extract(path, 0, size), Art::File(art::Kind::Image)),
        _ => image(extract(path, 0, size).or_else(|| shell_icon(path, size)), Art::File(art::Kind::App)),
    }
}

/// the icon Windows names for a file type ("C:\\...\\vlc.exe",0), from its association, as Explorer finds it
fn assoc_icon(ext: &str) -> Option<String> {
    let dotted = wstr(&format!(".{ext}"));
    let mut buf = vec![0u16; 1024];
    let mut n = buf.len() as u32;
    unsafe {
        AssocQueryStringW(
            ASSOCF_NOTRUNCATE,
            ASSOCSTR_DEFAULTICON,
            PCWSTR(dotted.as_ptr()),
            PCWSTR::null(),
            Some(windows::core::PWSTR(buf.as_mut_ptr())),
            &mut n,
        )
        .ok()
        .ok()?;
    }
    Some(crate::ui::from_wide(&buf))
}

/// an icon's place as the registry writes it: `"C:\a b\app.exe",-101`, `C:\app.exe,3` or just a file
fn split_icon_spec(s: &str) -> (String, i32) {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            let index = rest[end + 1..].trim().trim_start_matches(',').trim().parse().unwrap_or(0);
            return (rest[..end].to_string(), index);
        }
    }
    match s.rsplit_once(',') {
        Some((f, i)) if i.trim().parse::<i32>().is_ok() => (f.trim().to_string(), i.trim().parse().unwrap_or(0)),
        _ => (s.to_string(), 0),
    }
}

/// Whether a type's icon is a program's own rather than Windows': not in the Windows folder, not one of Microsoft's
/// own Store apps (Photos, Notepad), and not drawn per file (`%1`).
fn programs_icon(spec: &str, windir: &str) -> bool {
    let spec = spec.trim();
    if let Some(rest) = spec.strip_prefix("@{") {
        return !rest.starts_with("Microsoft.");
    }
    if spec.is_empty() || spec.contains("%1") {
        return false;
    }
    let file = dvfiles::fsx::expand_env(&split_icon_spec(spec).0);
    !dvfiles::pathx::is_within(&file, windir)
}

/// The icon a program gives a file type, at `size`, or None when the type keeps dvfiles' own icon. A negative index
/// is a resource id, which `PrivateExtractIconsW` takes as it is.
fn type_icon(ext: &str, size: i32) -> Option<Image> {
    if ext.is_empty() || per_file(ext) {
        return None;
    }
    let spec = assoc_icon(ext)?;
    let windir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    if !programs_icon(&spec, &windir) {
        return None;
    }
    if spec.trim().starts_with("@{") {
        return crate::ui::openwith::store_icon(spec.trim(), size);
    }
    let (file, index) = split_icon_spec(&spec);
    extract(&dvfiles::fsx::expand_env(&file), index, size)
}

fn worker(hwnd: isize, rx: std::sync::mpsc::Receiver<Req>) {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    while let Ok(req) = rx.recv() {
        let reply = match req {
            Req::Ext(exts) => Reply::Ext(
                exts.into_iter()
                    .map(|ext| {
                        let ty = match ext.as_str() {
                            "<dir>" => type_name("folder", FA_DIRECTORY),
                            "" => type_name("file", 0x80),
                            e => type_name(&format!("file.{e}"), 0x80),
                        };
                        (ext, ty)
                    })
                    .collect(),
            ),
            Req::Own(paths) => Reply::Own(
                paths
                    .into_iter()
                    .map(|(p, size)| {
                        let got = own_icon(&p, size);
                        ((p, size), got)
                    })
                    .collect(),
            ),
            Req::Type(types) => Reply::Type(
                types
                    .into_iter()
                    .map(|(ext, size)| {
                        let img = type_icon(&ext, size);
                        ((ext, size), img)
                    })
                    .collect(),
            ),
        };
        post_box(HWND(hwnd as *mut _), WM_ICONS, reply);
    }
}

impl Icons {
    /// `scale` is the window's DPI over 96: small icons are 16 px at 100%, as Windows' own
    pub fn start(hwnd: HWND, scale: f32) -> Icons {
        let (tx, rx) = channel();
        let h = hwnd.0 as isize;
        std::thread::spawn(move || worker(h, rx));
        Icons {
            small_px: (16.0 * scale).round() as i32,
            large_px: (48.0 * scale).round() as i32,
            by_ext: HashMap::new(),
            own: HashMap::new(),
            pending_ext: HashSet::new(),
            pending_own: HashSet::new(),
            queued_ext: Vec::new(),
            queued_own: Vec::new(),
            by_type: HashMap::new(),
            pending_type: HashSet::new(),
            queued_type: Vec::new(),
            tx,
        }
    }

    pub fn apply(&mut self, r: Reply) {
        match r {
            Reply::Ext(v) => {
                for (ext, ty) in v {
                    self.pending_ext.remove(&ext);
                    let ty: Rc<str> = if ty.is_empty() { fallback_type(&ext).into() } else { ty.into() };
                    self.by_ext.insert(ext, ty);
                }
            }
            Reply::Own(v) => {
                for (key, got) in v {
                    self.pending_own.remove(&key);
                    let own = match got {
                        Got::Image(i) => Own::Image(Rc::new(i)),
                        Got::Art(a) => Own::Art(a),
                    };
                    self.own.insert(key, own);
                }
            }
            Reply::Type(v) => {
                for (key, img) in v {
                    self.pending_type.remove(&key);
                    self.by_type.insert(key, img.map(Rc::new));
                }
            }
        }
    }

    /// The icon a file type's program gives it, at `size` (asked for if not known yet); None while it is on its way
    /// and when the type keeps dvfiles' own icon.
    pub fn of_type(&mut self, ext: &str, size: i32) -> Option<Rc<Image>> {
        let key = (ext.to_string(), size);
        if let Some(v) = self.by_type.get(&key) {
            return v.clone();
        }
        if self.pending_type.insert(key.clone()) {
            self.queued_type.push(key);
        }
        None
    }

    /// a program's, shortcut's or icon file's own icon at `size` (asked for if not known yet)
    pub fn own(&mut self, path: &str, size: i32) -> Own {
        let key = (path.to_string(), size);
        if let Some(o) = self.own.get(&key) {
            return o.clone();
        }
        if self.pending_own.insert(key.clone()) {
            self.queued_own.push(key);
        }
        Own::Pending
    }

    pub fn type_name(&mut self, e: &Entry) -> Rc<str> {
        let key = ext_key(e);
        if let Some(t) = self.by_ext.get(&key) {
            return t.clone();
        }
        if self.pending_ext.insert(key.clone()) {
            self.queued_ext.push(key.clone());
        }
        fallback_type(&key).into()
    }

    /// sends what painting asked for, in one message per kind
    pub fn flush(&mut self) {
        if !self.queued_ext.is_empty() {
            let _ = self.tx.send(Req::Ext(std::mem::take(&mut self.queued_ext)));
        }
        if !self.queued_own.is_empty() {
            let _ = self.tx.send(Req::Own(std::mem::take(&mut self.queued_own)));
        }
        if !self.queued_type.is_empty() {
            let _ = self.tx.send(Req::Type(std::mem::take(&mut self.queued_type)));
        }
    }

    /// a folder's contents changed on disk: its files' own icons may have changed
    pub fn forget_paths_under(&mut self, dir: &str) {
        self.own.retain(|(p, _), _| !dvfiles::pathx::is_within(p, dir));
    }

    /// lets go of the files' own icons and the types' icons (idle)
    pub fn clear_own(&mut self) {
        self.own.clear();
        self.by_type.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_places_as_the_registry_writes_them() {
        assert_eq!(
            split_icon_spec(r#""C:\Program Files\VideoLAN\VLC\vlc.exe",0"#),
            (r"C:\Program Files\VideoLAN\VLC\vlc.exe".to_string(), 0)
        );
        assert_eq!(
            split_icon_spec(r"C:\Windows\system32\wmploc.dll,-730"),
            (r"C:\Windows\system32\wmploc.dll".to_string(), -730)
        );
        assert_eq!(
            split_icon_spec(r"C:\Windows\system32\zipfldr.dll"),
            (r"C:\Windows\system32\zipfldr.dll".to_string(), 0)
        );
        assert_eq!(split_icon_spec(r#""C:\a,b\x.exe""#), (r"C:\a,b\x.exe".to_string(), 0));
    }

    #[test]
    fn icon_groups_tell_their_largest_picture() {
        // three entries of 14 bytes: 16, 48 and the big one written as 0
        let mut g = vec![0u8, 0, 1, 0, 3, 0];
        for w in [16u8, 48, 0] {
            let mut e = [0u8; 14];
            e[0] = w;
            e[1] = w;
            g.extend_from_slice(&e);
        }
        assert_eq!(largest_in(&g, 14), Some(256));
        assert_eq!(largest_in(&g[..6 + 28], 14), Some(48));
        assert_eq!(largest_in(&g[..4], 14), None);
        // Windows' own programs carry the big picture; a missing file has none
        if let Ok(root) = std::env::var("SystemRoot") {
            assert_eq!(largest_frame(&format!(r"{root}\explorer.exe"), 0), Some(256));
        }
        assert_eq!(largest_frame(r"C:\no\such\file.exe", 0), None);
    }

    #[test]
    fn whose_icon_a_type_shows() {
        let win = r"C:\Windows";
        // programs' icons are shown
        assert!(programs_icon(r#""C:\Users\me\AppData\Local\Programs\dvnote\dvnote.exe",0"#, win));
        assert!(programs_icon(r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe,10", win));
        assert!(programs_icon("@{Spotify.Spotify_1.2_x64__abc?ms-resource://Spotify/Files/Assets/x.png}", win));
        // Windows' own keep dvfiles' icon
        assert!(!programs_icon(r"C:\Windows\System32\shell32.dll,0", win));
        assert!(!programs_icon(r"%SystemRoot%\system32\imageres.dll,-102", win));
        assert!(!programs_icon(r"C:\Windows\system32\wmploc.dll,-730", win));
        assert!(!programs_icon(
            "@{Microsoft.Windows.Photos_2026_x64__8we?ms-resource://Microsoft.Windows.Photos/Files/Assets/x.png}",
            win
        ));
        assert!(!programs_icon("%1", win));
        assert!(!programs_icon("", win));
    }
}

fn fallback_type(key: &str) -> String {
    match key {
        "<dir>" => "File folder".into(),
        "" => "File".into(),
        e => format!("{} File", e.to_uppercase()),
    }
}
