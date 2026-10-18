// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Dragging files out to any program (Explorer, the desktop, mail, browsers) with dvfiles' own data object: the
//! `CF_HDROP` list Explorer itself reads, made in microseconds. The shell's data object needed an item id list for
//! every file first, parsed from its path: 1 s before a drag of 1,000 files could start, 7.6 s for System32's 4,825.
//!
//! The drag image is dvfiles' own picture of the item under the pointer, drawn as Windows 10 draws its own: the item's
//! picture, held at its bottom middle, and when more go along their count in a small blue square in its middle. The shell's drag image helper shows it and stores what it needs in the data object, as drop targets store
//! "Copy to Documents" and "Performed DropEffect", so every format given to it is kept and handed back as it came.
//! It also tells Explorer it may copy on a thread of its own (`IDataObjectAsyncCapability`), as the shell's data
//! object does; otherwise the drop, and the drag in dvfiles with it, would last as long as the copy.
use crate::ui::thumbs::Image;
use crate::ui::{clip, App};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use windows::core::{implement, Ref, BOOL, HRESULT};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Ole::*;
use windows::Win32::UI::Shell::{
    CLSID_DragDropHelper, IDataObjectAsyncCapability, IDataObjectAsyncCapability_Impl, IDragSourceHelper2,
    SHCreateMemStream, SHCreateStdEnumFmtEtc, SHDoDragDrop, DSH_ALLOWDROPDESCRIPTIONTEXT, SHDRAGIMAGE,
};

const CF_HDROP: u16 = 15;

/// One format's bytes and the medium they came in: global memory, or a stream (the shell keeps its drag context in
/// one, and the drag image helper fails with "Invalid tymed" if it cannot store it).
struct Format {
    cf: u16,
    tymed: TYMED,
    bytes: Vec<u8>,
}

#[implement(IDataObject, IDataObjectAsyncCapability)]
struct Data {
    // a lock: a target copying on its own thread reads them
    formats: Mutex<Vec<Format>>,
    asynchronous: AtomicBool,
    in_operation: AtomicBool,
}

/// a data object holding the paths as a file drop list
pub fn data(paths: &[String]) -> IDataObject {
    Data {
        formats: Mutex::new(vec![Format { cf: CF_HDROP, tymed: TYMED_HGLOBAL, bytes: clip::hdrop_bytes(paths) }]),
        asynchronous: AtomicBool::new(true),
        in_operation: AtomicBool::new(false),
    }
    .into()
}

impl Data {
    fn formats(&self) -> std::sync::MutexGuard<'_, Vec<Format>> {
        self.formats.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn etc(cf: u16, tymed: TYMED) -> FORMATETC {
    FORMATETC {
        cfFormat: cf,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: tymed.0 as u32,
    }
}

/// all of a stream, from its start
fn stream_bytes(s: &IStream) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    unsafe {
        let _ = s.Seek(0, STREAM_SEEK_SET, None);
        loop {
            let mut n = 0;
            if s.Read(buf.as_mut_ptr().cast(), buf.len() as u32, Some(&mut n)).is_err() || n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n as usize]);
        }
    }
    out
}

impl Data {
    fn find(&self, f: *const FORMATETC) -> Result<(TYMED, Vec<u8>), HRESULT> {
        let f = unsafe { f.as_ref() }.ok_or(E_POINTER)?;
        let v = self.formats();
        let e = v.iter().find(|e| e.cf == f.cfFormat).ok_or(DV_E_FORMATETC)?;
        if f.tymed & e.tymed.0 as u32 == 0 {
            return Err(DV_E_TYMED);
        }
        Ok((e.tymed, e.bytes.clone()))
    }
}

impl IDataObject_Impl for Data_Impl {
    fn GetData(&self, f: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
        let (tymed, bytes) = self.find(f)?;
        let mut m = STGMEDIUM { tymed: tymed.0 as u32, ..Default::default() };
        if tymed == TYMED_ISTREAM {
            // a stream of its own each time, so each reader has its own position
            let s = unsafe { SHCreateMemStream(Some(&bytes)) }.ok_or(E_OUTOFMEMORY)?;
            m.u.pstm = std::mem::ManuallyDrop::new(Some(s));
        } else {
            m.u.hGlobal = clip::global(&bytes).ok_or(E_OUTOFMEMORY)?;
        }
        Ok(m)
    }

    fn GetDataHere(&self, _: *const FORMATETC, _: *mut STGMEDIUM) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryGetData(&self, f: *const FORMATETC) -> HRESULT {
        self.find(f).map_or_else(|e| e, |_| S_OK)
    }

    fn GetCanonicalFormatEtc(&self, f: *const FORMATETC, out: *mut FORMATETC) -> HRESULT {
        unsafe {
            if let (Some(f), Some(out)) = (f.as_ref(), out.as_mut()) {
                *out = *f;
                out.ptd = std::ptr::null_mut();
            }
        }
        DATA_S_SAMEFORMATETC
    }

    fn SetData(&self, f: *const FORMATETC, m: *const STGMEDIUM, release: BOOL) -> windows::core::Result<()> {
        let (f, m) = unsafe { (f.as_ref().ok_or(E_POINTER)?, m.as_ref().ok_or(E_POINTER)?) };
        let (tymed, bytes) = match TYMED(m.tymed as i32) {
            TYMED_HGLOBAL => (TYMED_HGLOBAL, clip::bytes_of(unsafe { m.u.hGlobal })),
            TYMED_ISTREAM => {
                let s = unsafe { m.u.pstm.as_ref() }.ok_or(E_POINTER)?;
                (TYMED_ISTREAM, stream_bytes(s))
            }
            _ => return Err(DV_E_TYMED.into()),
        };
        if release.as_bool() {
            unsafe { ReleaseStgMedium(m as *const STGMEDIUM as *mut STGMEDIUM) };
        }
        let mut v = self.formats();
        match v.iter_mut().find(|e| e.cf == f.cfFormat) {
            Some(e) => (e.tymed, e.bytes) = (tymed, bytes),
            None => v.push(Format { cf: f.cfFormat, tymed, bytes }),
        }
        Ok(())
    }

    fn EnumFormatEtc(&self, dir: u32) -> windows::core::Result<IEnumFORMATETC> {
        if dir != DATADIR_GET.0 as u32 {
            return Err(E_NOTIMPL.into());
        }
        let list: Vec<FORMATETC> = self.formats().iter().map(|e| etc(e.cf, e.tymed)).collect();
        unsafe { SHCreateStdEnumFmtEtc(&list) }
    }

    fn DAdvise(&self, _: *const FORMATETC, _: u32, _: Ref<IAdviseSink>) -> windows::core::Result<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _: u32) -> windows::core::Result<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> windows::core::Result<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}

impl IDataObjectAsyncCapability_Impl for Data_Impl {
    fn SetAsyncMode(&self, on: BOOL) -> windows::core::Result<()> {
        self.asynchronous.store(on.as_bool(), Ordering::Relaxed);
        Ok(())
    }

    fn GetAsyncMode(&self) -> windows::core::Result<BOOL> {
        Ok(self.asynchronous.load(Ordering::Relaxed).into())
    }

    fn StartOperation(&self, _: Ref<IBindCtx>) -> windows::core::Result<()> {
        self.in_operation.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn InOperation(&self) -> windows::core::Result<BOOL> {
        Ok(self.in_operation.load(Ordering::Relaxed).into())
    }

    fn EndOperation(&self, _: HRESULT, _: Ref<IBindCtx>, _: u32) -> windows::core::Result<()> {
        self.in_operation.store(false, Ordering::Relaxed);
        Ok(())
    }
}

/// Drags the paths out; returns when they have been dropped or the drag was cancelled. Nothing is deleted here
/// whatever the result: a target that moves files moves them itself.
pub fn start(hwnd: HWND, paths: &[String], image: Option<Image>) {
    crate::ui::shell::ole_init();
    let data = data(paths);
    if let Some(img) = image {
        let _ = set_image(&data, &img);
    }
    unsafe {
        // the shell's drop source: it shows "Move to ..." on the drag image and sets the cursor to match
        let _ = SHDoDragDrop(Some(hwnd), &data, None, DROPEFFECT_COPY | DROPEFFECT_MOVE | DROPEFFECT_LINK);
    }
}

fn set_image(data: &IDataObject, img: &Image) -> windows::core::Result<()> {
    unsafe {
        let helper: IDragSourceHelper2 = CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER)?;
        let _ = helper.SetFlags(DSH_ALLOWDROPDESCRIPTIONTEXT.0 as u32);
        let bmp = bitmap(img).ok_or(E_OUTOFMEMORY)?;
        let shdi = SHDRAGIMAGE {
            sizeDragImage: SIZE { cx: img.w, cy: img.h },
            // the pointer holds the picture at its bottom middle, as Windows' own image is held (60, 111 of 120)
            ptOffset: POINT { x: img.w / 2, y: img.h * 37 / 40 },
            hbmpDragImage: bmp,
            crColorKey: COLORREF(0xFFFF_FFFF),
        };
        // the helper owns the bitmap once it has taken it
        let r = helper.InitializeFromBitmap(&shdi, data);
        if r.is_err() {
            let _ = DeleteObject(bmp.into());
        }
        r
    }
}

/// a 32-bit bitmap of premultiplied pixels, which is what the helper's layered drag window shows
fn bitmap(img: &Image) -> Option<HBITMAP> {
    unsafe {
        let bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: img.w,
                biHeight: -img.h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bmp = CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        std::ptr::copy_nonoverlapping(img.px.as_ptr(), bits as *mut u32, img.px.len());
        Some(bmp)
    }
}

/// lays `src` over `dst` (both premultiplied) with its top-left at (x, y)
fn over(dst: &mut [u32], dw: i32, dh: i32, src: &Image, x: i32, y: i32) {
    for sy in 0..src.h {
        let yy = y + sy;
        if yy < 0 || yy >= dh {
            continue;
        }
        for sx in 0..src.w {
            let xx = x + sx;
            if xx < 0 || xx >= dw {
                continue;
            }
            let s = src.px[(sy * src.w + sx) as usize];
            let d = &mut dst[(yy * dw + xx) as usize];
            let inv = 255 - (s >> 24);
            let ch = |sh: u32| ((s >> sh & 0xFF) + ((*d >> sh & 0xFF) * inv + 127) / 255).min(255) << sh;
            *d = ch(24) | ch(16) | ch(8) | ch(0);
        }
    }
}

/// The drag image: the picture of the item at `pos` in the list as Large icons shows it, its shortcut arrow, and how
/// many items go along.
pub fn image(a: &mut App, pos: usize, count: usize) -> Option<Image> {
    let i = *a.model.order.get(pos)?;
    let e = a.model.entries.get(i as usize)?.clone();
    let path = a.model.path_of(i);
    picture_of(a, &e, &path, count)
}

/// the drag image for an item known by its entry and path (the navigation pane's folders too)
pub fn picture_of(a: &mut App, e: &dvfiles::fsx::Entry, path: &str, count: usize) -> Option<Image> {
    let side = a.s(96.0);
    let pic = crate::ui::list::picture(a, e, path, (side, side))?;
    let arrow =
        pic.arrow.then(|| a.art.get(crate::ui::art::Art::Arrow, (pic.img.w as f32 * 0.34).round() as i32, &a.theme));
    let badge = (count > 1).then(|| a.nf.group(count as u64));
    compose(&pic.img, arrow.as_deref(), badge.as_deref(), a.fonts.bold, a.scale, side)
}

/// Windows' count badge: a flat square of this blue (not the accent colour), white figures
const BADGE: u32 = 0x0074CC;

/// the picture in the middle of a `side` square, its shortcut arrow, and the count badge with `text` in `font` in the
/// middle, as Windows' own drag image has it
fn compose(
    pic: &Image,
    arrow: Option<&Image>,
    badge: Option<&str>,
    font: HFONT,
    scale: f32,
    side: i32,
) -> Option<Image> {
    use crate::ui::gfx::{Rgb, Surface, R};
    let s = |v: f32| (v * scale).round() as i32;
    let (w, h) = (side, side);
    let mut px = vec![0u32; (w * h) as usize];
    let (ix, iy) = ((side - pic.w) / 2, (side - pic.h) / 2);
    over(&mut px, w, h, pic, ix, iy);
    if let Some(arrow) = arrow {
        over(&mut px, w, h, arrow, ix, iy + pic.h - arrow.h);
    }
    if let Some(text) = badge {
        // 18 px at 125%, as measured on Windows' own; wider for more figures
        let bh = s(14.5);
        let bw = (crate::ui::gfx::text_width(Surface::new(1, 1)?.dc, text, font) + s(7.0)).max(bh).min(side);
        let surf = Surface::new(bw, bh)?;
        surf.fill(R::new(0, 0, bw, bh), Rgb::hex(BADGE));
        surf.text(R::new(0, 0, bw, bh), text, font, Rgb::hex(0xFFFFFF), crate::ui::gfx::CENTER);
        let solid: Box<[u32]> = surf.pixels().iter().map(|&c| 0xFF00_0000 | (c & 0x00FF_FFFF)).collect();
        over(&mut px, w, h, &Image { w: bw, h: bh, px: solid }, (w - bw) / 2, (h - bh) / 2);
    }
    Some(Image { w, h, px: px.into_boxed_slice() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn get(d: &IDataObject, cf: u16) -> Option<Vec<u8>> {
        unsafe {
            let mut m = d.GetData(&etc(cf, TYMED_HGLOBAL)).ok()?;
            let b = clip::bytes_of(m.u.hGlobal);
            ReleaseStgMedium(&mut m);
            Some(b)
        }
    }

    #[test]
    fn holds_paths_and_what_others_store() {
        crate::ui::shell::ole_init();
        let paths = vec![r"C:\a\one.txt".to_string(), r"D:\two words\b".to_string()];
        let d = data(&paths);
        assert_eq!(clip::parse_hdrop(&get(&d, CF_HDROP).unwrap()), paths);
        assert!(get(&d, 49_999).is_none());
        // a format set by someone else comes back as it was, and is listed
        let h = clip::global(&[1, 2, 3, 4]).unwrap();
        let mut m = STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, ..Default::default() };
        m.u.hGlobal = h;
        unsafe { d.SetData(&etc(49_999, TYMED_HGLOBAL), &m, true).unwrap() };
        assert_eq!(get(&d, 49_999).unwrap()[..4], [1, 2, 3, 4]);
        let mut n = 0;
        unsafe {
            let e = d.EnumFormatEtc(DATADIR_GET.0 as u32).unwrap();
            let mut f = [FORMATETC::default(); 1];
            while e.Next(&mut f, None) == S_OK {
                n += 1;
            }
        }
        assert_eq!(n, 2);
    }

    /// the drag image helper takes dvfiles' picture: it stores the image in the data object through SetData
    #[test]
    fn the_helper_takes_the_picture() {
        crate::ui::shell::ole_init();
        let fonts = crate::ui::gfx::Fonts::new(1.25);
        let t = crate::ui::gfx::Theme::dark();
        let side = 120;
        let pic = crate::ui::art::render(crate::ui::art::Art::Folder, side, &t);
        let arrow = crate::ui::art::render(crate::ui::art::Art::Arrow, 41, &t);
        let img = compose(&pic, Some(&arrow), Some("1,234"), fonts.bold, 1.25, side).unwrap();
        assert_eq!((img.w, img.h), (side, side));
        // the badge sits in the middle, solid, in Windows' blue
        let mid = img.px[((img.h / 2 - 7) * img.w + img.w / 2 - 20) as usize];
        assert_eq!(mid, 0xFF00_0000 | BADGE, "{mid:08x}");
        if let Some(out) = std::env::var_os("DVFILES_DRAGSHEET") {
            let mut rgb = Vec::new();
            for bg in [[0x20u8, 0x20, 0x20], [0xF3, 0xF3, 0xF3]] {
                for p in img.px.iter() {
                    let a = (p >> 24) as f32 / 255.0;
                    let ch = |sh: u32, b: u8| ((p >> sh & 0xFF) as f32 + b as f32 * (1.0 - a)).round().min(255.0) as u8;
                    rgb.push([ch(16, bg[0]), ch(8, bg[1]), ch(0, bg[2])]);
                }
            }
            std::fs::write(out, crate::ui::art::tests::png(img.w as usize, img.h as usize * 2, &rgb)).unwrap();
        }
        let d = data(&[r"C:\a.txt".to_string()]);
        set_image(&d, &img).unwrap();
        let mut n = 0;
        unsafe {
            let e = d.EnumFormatEtc(DATADIR_GET.0 as u32).unwrap();
            let mut f = [FORMATETC::default(); 1];
            while e.Next(&mut f, None) == S_OK {
                n += 1;
            }
        }
        assert!(n > 1, "the helper stored nothing ({n} formats)");
    }

    #[test]
    fn large_selections_are_instant() {
        let paths: Vec<String> = (0..5000).map(|i| format!(r"C:\Windows\System32\file{i}.dll")).collect();
        let t = std::time::Instant::now();
        let d = data(&paths);
        assert_eq!(clip::parse_hdrop(&get(&d, CF_HDROP).unwrap()).len(), 5000);
        eprintln!("5,000 paths in and out: {:?}", t.elapsed());
        assert!(t.elapsed().as_millis() < 100, "{:?}", t.elapsed());
    }
}

/// DVFILES_SHELLTEST=1: hands a drag of dvfiles' data object to Explorer's own drop target for a folder, which is
/// what an Explorer window or the desktop does with a drop, and checks the files arrive (moved on the same drive by
/// default, copied with Ctrl), each once.
#[cfg(test)]
mod shell_tests {
    use super::*;
    use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::{IShellFolder, SHBindToParent, SHParseDisplayName};

    fn folder_target(dir: &str) -> IDropTarget {
        unsafe {
            let mut pidl = std::ptr::null_mut();
            SHParseDisplayName(windows::core::PCWSTR(crate::ui::wstr(dir).as_ptr()), None, &mut pidl, 0, None).unwrap();
            let mut last = std::ptr::null_mut();
            let parent: IShellFolder = SHBindToParent(pidl, Some(&mut last)).unwrap();
            let child = [last as *const ITEMIDLIST];
            parent.GetUIObjectOf(HWND::default(), &child, None).unwrap()
        }
    }

    fn drop_on(dir: &str, paths: &[String], keys: u32) -> DROPEFFECT {
        let t = folder_target(dir);
        let d = data(paths);
        let pt = POINTL { x: 0, y: 0 };
        let mut effect = DROPEFFECT(DROPEFFECT_COPY.0 | DROPEFFECT_MOVE.0);
        unsafe {
            t.DragEnter(&d, MODIFIERKEYS_FLAGS(keys | 1), pt, &mut effect).unwrap();
            let mut e2 = DROPEFFECT(DROPEFFECT_COPY.0 | DROPEFFECT_MOVE.0);
            t.DragOver(MODIFIERKEYS_FLAGS(keys | 1), pt, &mut e2).unwrap();
            let mut e3 = DROPEFFECT(DROPEFFECT_COPY.0 | DROPEFFECT_MOVE.0);
            t.Drop(&d, MODIFIERKEYS_FLAGS(keys), pt, &mut e3).unwrap();
            e3
        }
    }

    /// a drop target's helper shows dvfiles' picture in the shell's drag window (it showed none while the data object
    /// refused the stream the helper keeps its drag context in)
    #[test]
    fn the_image_shows_over_a_target() {
        if std::env::var_os("DVFILES_SHELLTEST").is_none() {
            return;
        }
        use windows::Win32::UI::Shell::IDropTargetHelper;
        use windows::Win32::UI::WindowsAndMessaging::*;
        unsafe extern "system" fn each(w: HWND, found: LPARAM) -> BOOL {
            let mut pid = 0;
            let mut c = [0u16; 32];
            unsafe {
                GetWindowThreadProcessId(w, Some(&mut pid));
                let n = GetClassNameW(w, &mut c) as usize;
                if pid == std::process::id()
                    && IsWindowVisible(w).as_bool()
                    && String::from_utf16_lossy(&c[..n]) == "SysDragImage"
                {
                    *(found.0 as *mut bool) = true;
                }
            }
            true.into()
        }
        crate::ui::shell::ole_init();
        let fonts = crate::ui::gfx::Fonts::new(1.25);
        let pic = crate::ui::art::render(crate::ui::art::Art::Folder, 120, &crate::ui::gfx::Theme::dark());
        let img = compose(&pic, None, Some("3"), fonts.bold, 1.25, 120).unwrap();
        let d = data(&[r"C:\a.txt".to_string()]);
        set_image(&d, &img).unwrap();
        let mut shown = false;
        unsafe {
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                windows::core::w!("STATIC"),
                None,
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let h: IDropTargetHelper = CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER).unwrap();
            h.DragEnter(hwnd, &d, &POINT { x: 300, y: 300 }, DROPEFFECT_COPY).unwrap();
            super::pump();
            let _ = EnumWindows(Some(each), LPARAM(&mut shown as *mut bool as isize));
            let _ = h.DragLeave();
            let _ = DestroyWindow(hwnd);
        }
        assert!(shown, "no drag image window");
    }

    #[test]
    fn explorer_takes_the_files() {
        if std::env::var_os("DVFILES_SHELLTEST").is_none() {
            return;
        }
        crate::ui::shell::ole_init();
        let base = std::env::temp_dir().join(format!("dvfiles_dragtest_{}", std::process::id()));
        let (src, dst) = (base.join("src"), base.join("dst"));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        let names = ["one.txt", "two words.txt", "three.bin"];
        let paths: Vec<String> = names
            .iter()
            .map(|n| {
                let p = src.join(n);
                std::fs::write(&p, n.as_bytes()).unwrap();
                p.to_string_lossy().into_owned()
            })
            .collect();
        let dst_s = dst.to_string_lossy().into_owned();
        // Explorer copies on a thread of its own: wait for it, answering its calls meanwhile
        let settle = |done: &dyn Fn() -> bool| {
            let t = std::time::Instant::now();
            while !done() && t.elapsed().as_secs() < 20 {
                super::pump();
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            // and a moment more, so a file that arrives twice would show
            for _ in 0..30 {
                super::pump();
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        // Ctrl: a copy, the originals stay
        let e = drop_on(&dst_s, &paths, 0x8);
        eprintln!("copy effect {}", e.0);
        settle(&|| names.iter().all(|n| std::fs::read(dst.join(n)).is_ok_and(|b| b == n.as_bytes())));
        for n in names {
            assert_eq!(std::fs::read(dst.join(n)).unwrap(), n.as_bytes(), "{n} copied");
            assert!(src.join(n).exists(), "{n} still there after a copy");
        }
        for n in names {
            std::fs::remove_file(dst.join(n)).unwrap();
        }
        // no key on the same drive: a move, done by Explorer
        let e = drop_on(&dst_s, &paths, 0);
        eprintln!("move effect {}", e.0);
        settle(&|| names.iter().all(|n| dst.join(n).exists() && !src.join(n).exists()));
        for n in names {
            assert_eq!(std::fs::read(dst.join(n)).unwrap(), n.as_bytes(), "{n} moved");
            assert!(!src.join(n).exists(), "{n} left behind after a move");
        }
        assert_eq!(std::fs::read_dir(&dst).unwrap().count(), 3);
        std::fs::remove_dir_all(&base).unwrap();
    }
}

#[cfg(test)]
fn pump() {
    use windows::Win32::UI::WindowsAndMessaging::{DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE};
    unsafe {
        let mut m = MSG::default();
        while PeekMessageW(&mut m, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&m);
            DispatchMessageW(&m);
        }
    }
}
