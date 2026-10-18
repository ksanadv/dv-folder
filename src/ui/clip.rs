// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Files on the clipboard the way Explorer puts them there: a `CF_HDROP` list plus "Preferred DropEffect" (copy or
//! move), so cut, copy and paste work between dvfiles, Explorer and other programs in both directions. This is the
//! plain clipboard API; no COM is needed.
use crate::ui::wstr;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};

const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;
const DROPEFFECT_COPY: u32 = 1;
const DROPEFFECT_MOVE: u32 = 2;

fn effect_format() -> u32 {
    unsafe { RegisterClipboardFormatW(windows::core::w!("Preferred DropEffect")) }
}

pub fn global(bytes: &[u8]) -> Option<HGLOBAL> {
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).ok()?;
        let p = GlobalLock(h) as *mut u8;
        if p.is_null() {
            let _ = GlobalFree(Some(h));
            return None;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        let _ = GlobalUnlock(h);
        Some(h)
    }
}

unsafe fn put(format: u32, bytes: &[u8]) {
    if let Some(h) = global(bytes) {
        if SetClipboardData(format, Some(HANDLE(h.0))).is_err() {
            let _ = GlobalFree(Some(h));
        }
    }
}

/// what a block of global memory holds
pub fn bytes_of(h: HGLOBAL) -> Vec<u8> {
    unsafe {
        let p = GlobalLock(h) as *const u8;
        if p.is_null() {
            return Vec::new();
        }
        let v = std::slice::from_raw_parts(p, GlobalSize(h)).to_vec();
        let _ = GlobalUnlock(h);
        v
    }
}

/// DROPFILES header (20 bytes) followed by the paths, each NUL-terminated, and a final NUL
pub fn hdrop_bytes(paths: &[String]) -> Vec<u8> {
    let mut b = Vec::with_capacity(20 + paths.iter().map(|p| p.len() * 2 + 2).sum::<usize>() + 2);
    b.extend_from_slice(&20u32.to_le_bytes());
    b.extend_from_slice(&[0u8; 8]);
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&1u32.to_le_bytes());
    for p in paths {
        for u in p.encode_utf16().chain(Some(0)) {
            b.extend_from_slice(&u.to_le_bytes());
        }
    }
    b.extend_from_slice(&[0, 0]);
    b
}

pub fn parse_hdrop(b: &[u8]) -> Vec<String> {
    if b.len() < 20 {
        return Vec::new();
    }
    let off = u32::from_le_bytes(b[0..4].try_into().unwrap()) as usize;
    let wide = u32::from_le_bytes(b[16..20].try_into().unwrap()) != 0;
    let mut out = Vec::new();
    let body = b.get(off..).unwrap_or(&[]);
    if wide {
        let units: Vec<u16> = body.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        for s in units.split(|&u| u == 0) {
            if s.is_empty() {
                break;
            }
            out.push(String::from_utf16_lossy(s));
        }
    } else {
        for s in body.split(|&c| c == 0) {
            if s.is_empty() {
                break;
            }
            out.push(String::from_utf8_lossy(s).into_owned());
        }
    }
    out
}

/// The paths in a DROPFILES block held in global memory (what CF_HDROP and WM_DROPFILES carry), and the point it
/// was dropped at: read directly, where DragQueryFileW and DragQueryPoint would load the shell for it.
pub fn read_hdrop(h: HGLOBAL) -> (Vec<String>, (i32, i32)) {
    unsafe {
        let p = GlobalLock(h) as *const u8;
        if p.is_null() {
            return (Vec::new(), (0, 0));
        }
        let b = std::slice::from_raw_parts(p, GlobalSize(h));
        let pt = if b.len() >= 12 {
            (i32::from_le_bytes(b[4..8].try_into().unwrap()), i32::from_le_bytes(b[8..12].try_into().unwrap()))
        } else {
            (0, 0)
        };
        let paths = parse_hdrop(b);
        let _ = GlobalUnlock(h);
        (paths, pt)
    }
}

pub fn set_files(hwnd: HWND, paths: &[String], cut: bool) -> bool {
    unsafe {
        if OpenClipboard(Some(hwnd)).is_err() {
            return false;
        }
        let _ = EmptyClipboard();
        put(CF_HDROP, &hdrop_bytes(paths));
        let effect = if cut { DROPEFFECT_MOVE } else { DROPEFFECT_COPY };
        put(effect_format(), &effect.to_le_bytes());
        let _ = CloseClipboard();
    }
    true
}

unsafe fn read(format: u32) -> Option<Vec<u8>> {
    let h = GetClipboardData(format).ok()?;
    Some(bytes_of(HGLOBAL(h.0))).filter(|v| !v.is_empty())
}

/// the files on the clipboard and whether they were cut
pub fn get_files(hwnd: HWND) -> Option<(Vec<String>, bool)> {
    unsafe {
        if !has_files() || OpenClipboard(Some(hwnd)).is_err() {
            return None;
        }
        let files = read(CF_HDROP).map(|b| parse_hdrop(&b));
        let effect = read(effect_format())
            .and_then(|b| b.get(0..4).map(|x| u32::from_le_bytes(x.try_into().unwrap())))
            .unwrap_or(DROPEFFECT_COPY);
        let _ = CloseClipboard();
        let files = files.filter(|f| !f.is_empty())?;
        Some((files, effect & DROPEFFECT_MOVE != 0 && effect & DROPEFFECT_COPY == 0))
    }
}

pub fn has_files() -> bool {
    unsafe { IsClipboardFormatAvailable(CF_HDROP).is_ok() }
}

const CF_DIB: u32 = 8;
const CF_DIBV5: u32 = 17;

fn png_format() -> u32 {
    unsafe { RegisterClipboardFormatW(windows::core::w!("PNG")) }
}

/// whether the clipboard holds something dvfiles can paste as a new file: a picture or text
pub fn has_pasteable() -> bool {
    [png_format(), CF_UNICODETEXT, CF_DIBV5, CF_DIB]
        .into_iter()
        .any(|f| unsafe { IsClipboardFormatAvailable(f).is_ok() })
}

/// What can be pasted as a new file when there are no files on the clipboard: a PNG as another program put it there,
/// else text (a table copied from a spreadsheet comes with a picture of itself too, and the text is what is meant),
/// else a bitmap (a screenshot).
pub fn get_pasteable(hwnd: HWND) -> Option<dvfiles::pastefile::Clip> {
    use dvfiles::pastefile::Clip;
    unsafe {
        if OpenClipboard(Some(hwnd)).is_err() {
            return None;
        }
        let got = read(png_format())
            .map(Clip::Png)
            .or_else(|| {
                read(CF_UNICODETEXT).map(|b| {
                    let units: Vec<u16> =
                        b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
                    Clip::Text(String::from_utf16_lossy(&units))
                })
            })
            .or_else(|| read(CF_DIBV5).map(Clip::Dib))
            .or_else(|| read(CF_DIB).map(Clip::Dib));
        let _ = CloseClipboard();
        got
    }
}

pub fn sequence() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

/// after a cut and paste the old paths are gone, so the clipboard is cleared as Explorer does
pub fn clear(hwnd: HWND) {
    unsafe {
        if OpenClipboard(Some(hwnd)).is_ok() {
            let _ = EmptyClipboard();
            let _ = CloseClipboard();
        }
    }
}

pub fn set_text(hwnd: HWND, text: &str) {
    let w = wstr(text);
    let bytes: Vec<u8> = w.iter().flat_map(|u| u.to_le_bytes()).collect();
    unsafe {
        if OpenClipboard(Some(hwnd)).is_ok() {
            let _ = EmptyClipboard();
            put(CF_UNICODETEXT, &bytes);
            let _ = CloseClipboard();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hdrop_round_trip() {
        let paths = vec![r"C:\a b\c.txt".to_string(), r"D:\日本\x".to_string()];
        assert_eq!(parse_hdrop(&hdrop_bytes(&paths)), paths);
        assert!(parse_hdrop(&[1, 2]).is_empty());
        assert!(parse_hdrop(&hdrop_bytes(&[])).is_empty());
    }
}
