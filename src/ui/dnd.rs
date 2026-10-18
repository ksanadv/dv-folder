// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Dropping files on the list and the navigation pane: the folder under the pointer lights up, the drag image says
//! "Move to Documents" or "Copy to Documents" as Explorer's does, and the drop runs as a dvfiles job.
//!
//! The drop target is registered after the first frame is on screen (OLE takes a few milliseconds to start); until
//! then, and if registering fails, plain WM_DROPFILES still works.
//!
//! A move is done by dvfiles itself, after the drop has returned. The program the files came from is told that
//! nothing is left for it to delete (Windows' "optimized move"), so it cannot remove the originals while the copy
//! across drives is still running.
use crate::ui::{from_wide, post_box, with_app};
use dvfiles::ops::Kind;
use std::cell::RefCell;
use windows::core::{implement, Ref, PCWSTR};
use windows::Win32::Foundation::{HWND, POINT, POINTL};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::Com::{
    CoCreateInstance, IDataObject, CLSCTX_INPROC_SERVER, DVASPECT_CONTENT, FORMATETC, STGMEDIUM, TYMED_HGLOBAL,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::*;
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{CLSID_DragDropHelper, IDropTargetHelper};
use windows::Win32::UI::WindowsAndMessaging::WM_APP;

/// posted to the main window with a `Dropped` once a drop has returned
pub const WM_DROPPED: u32 = WM_APP + 21;

pub struct Dropped {
    pub kind: Kind,
    pub paths: Vec<String>,
    pub target: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    List,
    Nav,
}

#[implement(IDropTarget)]
struct Target {
    pane: Pane,
    hwnd: HWND,
    // the shell's drag image helper, made on the first drag: it loads the shell and its image codecs (about 2 MB),
    // which a window nobody drags onto never needs
    helper: RefCell<Option<IDropTargetHelper>>,
    data: RefCell<Option<IDataObject>>,
    paths: RefCell<Vec<String>>,
    // what the drag image says now, so it is set only when it changes
    said: RefCell<(i32, String)>,
    // the folder held under the drag, and since when: held long enough, it opens
    spring: RefCell<Option<(usize, std::time::Instant)>>,
}

/// how long a folder is held under a drag before it opens
const SPRING_MS: u128 = 800;

// DROPIMAGETYPE: the symbol on the drag image
const IMAGE_INVALID: i32 = -1;
const IMAGE_NONE: i32 = 0;
const IMAGE_COPY: i32 = 1;
const IMAGE_MOVE: i32 = 2;
const IMAGE_LINK: i32 = 4;

const MK_SHIFT: u32 = 0x4;
const MK_CONTROL: u32 = 0x8;
const MK_ALT: u32 = 0x20;

fn hdrop_paths(data: &IDataObject) -> Vec<String> {
    let fmt = FORMATETC {
        cfFormat: 15,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    unsafe {
        let Ok(mut medium) = data.GetData(&fmt) else { return idlist_paths(data) };
        let (paths, _) = crate::ui::clip::read_hdrop(medium.u.hGlobal);
        ReleaseStgMedium(&mut medium);
        paths
    }
}

/// paths from CFSTR_SHELLIDLIST: a CIDA (count, then offsets to the parent's item id list and each child's)
fn idlist_paths(data: &IDataObject) -> Vec<String> {
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::{ILCombine, ILFree, SHGetPathFromIDListEx, GPFIDL_DEFAULT};
    let mut out = Vec::new();
    unsafe {
        let cf = RegisterClipboardFormatW(windows::core::w!("Shell IDList Array"));
        let fmt = FORMATETC {
            cfFormat: cf as u16,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        };
        let Ok(mut medium) = data.GetData(&fmt) else { return out };
        let base = GlobalLock(medium.u.hGlobal) as *const u8;
        if !base.is_null() {
            let n = (base as *const u32).read_unaligned() as usize;
            let offset = |k: usize| (base.add(4 + 4 * k) as *const u32).read_unaligned() as usize;
            let parent = base.add(offset(0)) as *const ITEMIDLIST;
            for k in 0..n.min(100_000) {
                let child = base.add(offset(k + 1)) as *const ITEMIDLIST;
                let full = ILCombine(Some(parent), Some(child));
                if !full.is_null() {
                    let mut buf = [0u16; 32768];
                    if SHGetPathFromIDListEx(full, &mut buf, GPFIDL_DEFAULT).as_bool() {
                        out.push(from_wide(&buf));
                    }
                    ILFree(Some(full));
                }
            }
            let _ = GlobalUnlock(medium.u.hGlobal);
        }
        ReleaseStgMedium(&mut medium);
    }
    out
}

/// Sets the line the drag image shows ("Move to %1"), as Explorer does; sources that do not show one ignore it.
fn describe(data: &IDataObject, image: i32, message: &str, insert: &str) {
    unsafe {
        let cf = RegisterClipboardFormatW(windows::core::w!("DropDescription"));
        // DROPDESCRIPTION: image type, then two 260-character strings
        let Ok(g) = GlobalAlloc(GMEM_MOVEABLE, 4 + 520 + 520) else { return };
        let p = GlobalLock(g) as *mut u8;
        if p.is_null() {
            return;
        }
        std::ptr::write_bytes(p, 0, 1044);
        (p as *mut i32).write(image);
        for (at, s) in [(4usize, message), (524usize, insert)] {
            let w: Vec<u16> = s.encode_utf16().take(259).collect();
            std::ptr::copy_nonoverlapping(w.as_ptr(), p.add(at) as *mut u16, w.len());
        }
        let _ = GlobalUnlock(g);
        let fmt = FORMATETC {
            cfFormat: cf as u16,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        };
        let mut medium = STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, ..Default::default() };
        medium.u.hGlobal = g;
        let _ = data.SetData(&fmt, &medium, true);
    }
}

/// a DWORD under a registered format, for the source to read after the drop
fn set_dword(data: &IDataObject, format: PCWSTR, v: u32) {
    unsafe {
        let cf = RegisterClipboardFormatW(format);
        let Ok(g) = GlobalAlloc(GMEM_MOVEABLE, 4) else { return };
        let p = GlobalLock(g) as *mut u32;
        if p.is_null() {
            return;
        }
        p.write(v);
        let _ = GlobalUnlock(g);
        let fmt = FORMATETC {
            cfFormat: cf as u16,
            ptd: std::ptr::null_mut(),
            dwAspect: DVASPECT_CONTENT.0,
            lindex: -1,
            tymed: TYMED_HGLOBAL.0 as u32,
        };
        let mut medium = STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, ..Default::default() };
        medium.u.hGlobal = g;
        let _ = data.SetData(&fmt, &medium, true);
    }
}

/// Copy or move: Ctrl copies, Shift moves, otherwise a move on the same drive and a copy to another one, as in
/// Explorer. Links are not made: what would be a link is a copy. None when the source allows neither.
fn choose(keys: u32, allowed: DROPEFFECT, same_volume: bool) -> Option<Kind> {
    let can_copy = allowed.0 & DROPEFFECT_COPY.0 != 0;
    let can_move = allowed.0 & DROPEFFECT_MOVE.0 != 0;
    let want_move = if keys & MK_CONTROL != 0 || keys & MK_ALT != 0 {
        false
    } else if keys & MK_SHIFT != 0 {
        true
    } else {
        same_volume
    };
    match (want_move, can_copy, can_move) {
        (true, _, true) | (false, false, true) => Some(Kind::Move),
        (false, true, _) | (true, true, false) => Some(Kind::Copy),
        _ => None,
    }
}

impl Target {
    fn helper(&self) -> Option<IDropTargetHelper> {
        if self.helper.borrow().is_none() {
            *self.helper.borrow_mut() =
                unsafe { CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER).ok() };
        }
        self.helper.borrow().clone()
    }

    fn client(&self, pt: &POINTL) -> POINT {
        let mut p = POINT { x: pt.x, y: pt.y };
        unsafe {
            let _ = ScreenToClient(self.hwnd, &mut p);
        }
        p
    }

    /// works out the folder under the pointer and the effect, lights it up, and describes it on the drag image
    /// over the shelf's row: lit up, and the drop only gathers the paths there (shown as a link; nothing is copied)
    fn over_shelf(&self, y: i32, effect: *mut DROPEFFECT) -> bool {
        if self.pane != Pane::Nav {
            return false;
        }
        let Some(Some(i)) = with_app(|a| crate::ui::nav::shelf_at(a, y)) else { return false };
        with_app(|a| {
            if a.navs.drop_hot != Some(i) {
                a.navs.drop_hot = Some(i);
                crate::ui::invalidate(a.nav);
            }
        });
        *self.spring.borrow_mut() = None;
        let label = "the shelf".to_string();
        if *self.said.borrow() != (IMAGE_LINK, label.clone()) {
            if let Some(data) = self.data.borrow().as_ref() {
                describe(data, IMAGE_LINK, "Add to %1", &label);
            }
            *self.said.borrow_mut() = (IMAGE_LINK, label);
        }
        unsafe {
            let allowed = *effect;
            *effect = if allowed.0 & DROPEFFECT_LINK.0 != 0 {
                DROPEFFECT_LINK
            } else {
                DROPEFFECT(allowed.0 & DROPEFFECT_COPY.0)
            };
        }
        true
    }

    fn over(&self, keys: u32, pt: &POINTL, effect: *mut DROPEFFECT) {
        let p = self.client(pt);
        if self.over_shelf(p.y, effect) {
            return;
        }
        let paths = self.paths.borrow().clone();
        let found = with_app(|a| {
            let r = match self.pane {
                Pane::List => crate::ui::list::drop_target_at(a, p.x, p.y, &paths),
                Pane::Nav => crate::ui::nav::drop_target_at(a, p.x, p.y, &paths),
            };
            let hot = r.as_ref().and_then(|t| t.1);
            match self.pane {
                Pane::List => {
                    if a.lv.drop_hot != hot {
                        a.lv.drop_hot = hot;
                        crate::ui::invalidate(a.list);
                    }
                    crate::ui::list::drag_scroll(a, p.y);
                }
                Pane::Nav => {
                    if a.navs.drop_hot != hot {
                        a.navs.drop_hot = hot;
                        crate::ui::invalidate(a.nav);
                    }
                }
            }
            r.map(|(folder, _)| (folder.clone(), a.display_name(&folder), hot))
        })
        .flatten();
        // Windows asks about the drag every few dozen ms even while the pointer rests, so holding is timed here
        let held = found.as_ref().and_then(|f| f.2);
        let open = {
            let mut sp = self.spring.borrow_mut();
            match (*sp, held) {
                (Some((i, t)), Some(h)) if i == h => t.elapsed().as_millis() >= SPRING_MS,
                (_, Some(h)) => {
                    *sp = Some((h, std::time::Instant::now()));
                    false
                }
                _ => {
                    *sp = None;
                    false
                }
            }
        };
        if open && with_app(|a| a.settings.spring) == Some(true) {
            *self.spring.borrow_mut() = None;
            if let (Some((folder, _, Some(i))), Pane::List) = (&found, self.pane) {
                let _ = i;
                with_app(|a| a.lv.drop_hot = None);
                crate::ui::navigate(crate::ui::model::Loc::Dir(folder.clone()), true);
            } else if let Some((_, _, Some(i))) = &found {
                with_app(|a| crate::ui::nav::spring_open(a, *i));
            }
        }
        let found = found.map(|f| (f.0, f.1));
        let allowed = unsafe { *effect };
        let mut chosen = None;
        if let Some((folder, _)) = &found {
            if !paths.is_empty() {
                let same = dvfiles::fsx::same_volume(&paths[0], folder);
                chosen = choose(keys, allowed, same);
                // moving items to where they already are does nothing
                let all_here =
                    paths.iter().all(|p| dvfiles::pathx::parent(p).is_some_and(|d| dvfiles::pathx::same(&d, folder)));
                if chosen == Some(Kind::Move) && all_here {
                    chosen = None;
                }
            }
        }
        let (image, message) = match (chosen, &found) {
            (Some(Kind::Move), Some(_)) => (IMAGE_MOVE, "Move to %1"),
            (Some(_), Some(_)) => (IMAGE_COPY, "Copy to %1"),
            _ => (IMAGE_NONE, ""),
        };
        let label = found.as_ref().map(|f| f.1.clone()).unwrap_or_default();
        if *self.said.borrow() != (image, label.clone()) {
            if let Some(data) = self.data.borrow().as_ref() {
                describe(data, image, message, &label);
            }
            *self.said.borrow_mut() = (image, label);
        }
        let e = match chosen {
            Some(Kind::Move) => DROPEFFECT_MOVE,
            Some(_) => DROPEFFECT_COPY,
            None => DROPEFFECT_NONE,
        };
        unsafe {
            *effect = e;
        }
    }

    fn clear(&self) {
        *self.spring.borrow_mut() = None;
        with_app(|a| {
            if a.lv.drop_hot.take().is_some() {
                crate::ui::invalidate(a.list);
            }
            if a.navs.drop_hot.take().is_some() {
                crate::ui::invalidate(a.nav);
            }
        });
    }
}

impl IDropTarget_Impl for Target_Impl {
    fn DragEnter(
        &self,
        data: Ref<IDataObject>,
        keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let data: Option<IDataObject> = data.cloned();
        *self.paths.borrow_mut() = data.as_ref().map(hdrop_paths).unwrap_or_default();
        *self.data.borrow_mut() = data.clone();
        *self.said.borrow_mut() = (i32::MIN, String::new());
        self.over(keys.0, pt, effect);
        if let (Some(h), Some(d)) = (&self.helper(), &data) {
            let p = POINT { x: pt.x, y: pt.y };
            unsafe {
                let _ = h.DragEnter(self.hwnd, d, &p, *effect);
            }
        }
        Ok(())
    }

    fn DragOver(&self, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        self.over(keys.0, pt, effect);
        if let Some(h) = self.helper.borrow().as_ref() {
            let p = POINT { x: pt.x, y: pt.y };
            unsafe {
                let _ = h.DragOver(&p, *effect);
            }
        }
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        self.clear();
        if let Some(d) = self.data.borrow().as_ref() {
            describe(d, IMAGE_INVALID, "", "");
        }
        if let Some(h) = self.helper.borrow().as_ref() {
            unsafe {
                let _ = h.DragLeave();
            }
        }
        *self.data.borrow_mut() = None;
        self.paths.borrow_mut().clear();
        Ok(())
    }

    fn Drop(
        &self,
        data: Ref<IDataObject>,
        keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let data: Option<IDataObject> = data.cloned();
        if let Some(d) = &data {
            *self.paths.borrow_mut() = hdrop_paths(d);
        }
        self.over(keys.0, pt, effect);
        let chosen = unsafe { *effect };
        let p = self.client(pt);
        let paths = self.paths.borrow().clone();
        let shelf = self.pane == Pane::Nav && with_app(|a| crate::ui::nav::shelf_at(a, p.y)).flatten().is_some();
        let target = with_app(|a| match self.pane {
            Pane::List => crate::ui::list::drop_target_at(a, p.x, p.y, &paths),
            Pane::Nav => crate::ui::nav::drop_target_at(a, p.x, p.y, &paths),
        })
        .flatten()
        .map(|t| t.0);
        self.clear();
        if let Some(h) = self.helper.borrow().as_ref() {
            let pp = POINT { x: pt.x, y: pt.y };
            unsafe {
                let _ = h.Drop(data.as_ref(), &pp, chosen);
            }
        }
        if shelf {
            with_app(|a| {
                for p in paths {
                    if !a.shelf.iter().any(|s| dvfiles::pathx::same(s, &p)) {
                        a.shelf.push(p);
                    }
                }
                crate::ui::invalidate(a.nav);
                crate::ui::invalidate(a.main);
            });
            *self.data.borrow_mut() = None;
            return Ok(());
        }
        let kind = if chosen == DROPEFFECT_MOVE {
            Some(Kind::Move)
        } else if chosen == DROPEFFECT_COPY {
            Some(Kind::Copy)
        } else {
            None
        };
        if let (Some(kind), Some(target), false) = (kind, target, paths.is_empty()) {
            if kind == Kind::Move {
                // dvfiles moves the files itself: the source has nothing left to delete
                if let Some(d) = &data {
                    set_dword(d, windows::core::w!("Performed DropEffect"), DROPEFFECT_NONE.0);
                    set_dword(d, windows::core::w!("Logical Performed DropEffect"), DROPEFFECT_MOVE.0);
                }
                unsafe {
                    *effect = DROPEFFECT_NONE;
                }
            }
            if let Some(main) = with_app(|a| a.main) {
                // started after the drop returns, so the program dragged from is not held up
                post_box(main, WM_DROPPED, Dropped { kind, paths, target });
            }
        } else {
            unsafe {
                *effect = DROPEFFECT_NONE;
            }
        }
        *self.data.borrow_mut() = None;
        Ok(())
    }
}

thread_local! {
    static TARGETS: RefCell<Vec<(HWND, IDropTarget)>> = const { RefCell::new(Vec::new()) };
}

/// Registers the list and the navigation pane as drop targets (then WM_DROPFILES is no longer needed).
pub fn register(list: HWND, nav: HWND) {
    crate::ui::shell::ole_init();
    for (hwnd, pane) in [(list, Pane::List), (nav, Pane::Nav)] {
        let t: IDropTarget = Target {
            pane,
            hwnd,
            helper: RefCell::new(None),
            data: RefCell::new(None),
            paths: RefCell::new(Vec::new()),
            said: RefCell::new((i32::MIN, String::new())),
            spring: RefCell::new(None),
        }
        .into();
        let ok = unsafe { RegisterDragDrop(hwnd, &t) }.is_ok();
        crate::trace::log(|| format!("drop target {:?}: {ok}", hwnd.0));
        if ok {
            crate::ui::accept_files(hwnd, false);
            TARGETS.with(|v| v.borrow_mut().push((hwnd, t)));
        }
    }
}

/// For testing without the real mouse: DVFILES_DNDTEST=<item>;<x>;<y> drags the item over the list's point (x, y)
/// and holds it there, and `self_test_drop` then drops it, through the same calls a drag makes.
pub fn self_test(list: HWND) {
    let Ok(spec) = std::env::var("DVFILES_DNDTEST") else { return };
    let parts: Vec<&str> = spec.split(';').collect();
    let (Some(item), Some(x), Some(y)) =
        (parts.first(), parts.get(1).and_then(|v| v.parse().ok()), parts.get(2).and_then(|v| v.parse().ok()))
    else {
        return;
    };
    let data = crate::ui::drag::data(&[item.to_string()]);
    let Some(t) = TARGETS.with(|v| v.borrow().iter().find(|(h, _)| *h == list).map(|(_, t)| t.clone())) else { return };
    let mut p = POINT { x, y };
    unsafe {
        let _ = windows::Win32::Graphics::Gdi::ClientToScreen(list, &mut p);
        let pt = POINTL { x: p.x, y: p.y };
        let mut effect = DROPEFFECT(DROPEFFECT_COPY.0 | DROPEFFECT_MOVE.0);
        let _ = t.DragEnter(&data, MODIFIERKEYS_FLAGS(0), pt, &mut effect);
        let mut effect = DROPEFFECT(DROPEFFECT_COPY.0 | DROPEFFECT_MOVE.0);
        let _ = t.DragOver(MODIFIERKEYS_FLAGS(0), pt, &mut effect);
        crate::trace::log(|| format!("dnd test: over ({x},{y}) effect {}", effect.0));
    }
    TEST.with(|c| *c.borrow_mut() = Some((t, data, POINTL { x: p.x, y: p.y })));
}

pub fn self_test_drop() {
    if let Some((t, data, pt)) = TEST.with(|c| c.borrow_mut().take()) {
        let mut effect = DROPEFFECT(DROPEFFECT_COPY.0 | DROPEFFECT_MOVE.0);
        unsafe {
            let _ = t.Drop(&data, MODIFIERKEYS_FLAGS(0), pt, &mut effect);
        }
        crate::trace::log(|| format!("dnd test: dropped, effect {}", effect.0));
    }
}

thread_local! {
    static TEST: RefCell<Option<(IDropTarget, IDataObject, POINTL)>> = const { RefCell::new(None) };
}

pub fn revoke() {
    TARGETS.with(|v| {
        for (h, _) in v.borrow_mut().drain(..) {
            unsafe {
                let _ = RevokeDragDrop(h);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dragged_paths_are_read_back() {
        crate::ui::shell::ole_init();
        // items from two folders (search results)
        let paths = vec![r"C:\Users\one.txt".to_string(), r"D:\other folder\two.txt".to_string()];
        assert_eq!(hdrop_paths(&crate::ui::drag::data(&paths)), paths);
    }

    #[test]
    fn effects_follow_explorer() {
        let both = DROPEFFECT(DROPEFFECT_COPY.0 | DROPEFFECT_MOVE.0);
        assert_eq!(choose(0, both, true), Some(Kind::Move));
        assert_eq!(choose(0, both, false), Some(Kind::Copy));
        assert_eq!(choose(MK_CONTROL, both, true), Some(Kind::Copy));
        assert_eq!(choose(MK_SHIFT, both, false), Some(Kind::Move));
        assert_eq!(choose(0, DROPEFFECT_COPY, true), Some(Kind::Copy));
        assert_eq!(choose(MK_SHIFT, DROPEFFECT_COPY, true), Some(Kind::Copy));
        assert_eq!(choose(0, DROPEFFECT_LINK, true), None);
    }
}
