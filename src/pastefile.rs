// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! What Ctrl+V makes in a folder when the clipboard holds no files but something else: a picture (a screenshot)
//! becomes a PNG, a single web address an Internet shortcut, any other text a .txt file. This part only turns the
//! clipboard's bytes into a file's name and contents; reading the clipboard and writing the file is the window's.
//! No Windows dependencies.

/// what was on the clipboard
pub enum Clip {
    /// a PNG as another program put it there ("PNG" format), kept as it is
    Png(Vec<u8>),
    /// a device-independent bitmap (`CF_DIB` / `CF_DIBV5`): its header and pixels, without a file header
    Dib(Vec<u8>),
    Text(String),
}

/// a file to make: its name's start (made unique by the caller), its extension and its bytes
#[derive(Debug, PartialEq)]
pub struct Made {
    pub base: String,
    pub ext: &'static str,
    pub bytes: Vec<u8>,
}

/// a lone web address (one line, no spaces), else None
pub fn link(text: &str) -> Option<&str> {
    let t = text.trim();
    let lower = t.to_ascii_lowercase();
    let ok = (lower.starts_with("https://") || lower.starts_with("http://"))
        && !t.contains(char::is_whitespace)
        && t.len() > 8
        && t.len() < 2048;
    ok.then_some(t)
}

/// a shortcut's name from its address: the site, without "www." ("github.com")
pub fn link_name(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    let host = host.strip_prefix("www.").unwrap_or(host);
    let clean: String =
        host.chars().filter(|c| !matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')).collect();
    if clean.is_empty() {
        "Link".into()
    } else {
        clean
    }
}

/// a `CF_DIB` as a .bmp file's bytes: the 14-byte file header Windows leaves off in front
pub fn bmp_from_dib(dib: &[u8]) -> Option<Vec<u8>> {
    let u32_at = |o: usize| Some(u32::from_le_bytes(dib.get(o..o + 4)?.try_into().ok()?));
    let header = u32_at(0)? as usize;
    if !(12..=124).contains(&header) || dib.len() < header {
        return None;
    }
    let (bits, compression, used) = if header == 12 {
        (u16::from_le_bytes([*dib.get(10)?, *dib.get(11)?]) as u32, 0, 0)
    } else {
        (u16::from_le_bytes([*dib.get(14)?, *dib.get(15)?]) as u32, u32_at(16)?, u32_at(32)?)
    };
    // the colour masks follow a plain 40-byte header with BI_BITFIELDS (3) or BI_ALPHABITFIELDS (6)
    let masks = match (header, compression) {
        (40, 3) => 12,
        (40, 6) => 16,
        _ => 0,
    };
    let entry = if header == 12 { 3 } else { 4 };
    let palette = if bits <= 8 { (if used == 0 { 1 << bits } else { used }) * entry } else { used * entry };
    let offset = 14 + header + masks + palette as usize;
    let mut out = Vec::with_capacity(14 + dib.len());
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&((14 + dib.len()) as u32).to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(offset as u32).to_le_bytes());
    out.extend_from_slice(dib);
    Some(out)
}

/// The file the clipboard's contents make, named with `stamp` ("2026-10-16 101530"): a picture as PNG (a bitmap is
/// decoded by dvfiles' own BMP decoder and encoded by its own PNG encoder), a web address as an Internet shortcut,
/// text as UTF-8. None for an empty or unreadable clipboard.
pub fn make(clip: &Clip, stamp: &str) -> Option<Made> {
    match clip {
        Clip::Png(png) => (crate::img::sniff(png) == Some(crate::img::Format::Png)).then(|| Made {
            base: format!("Pasted picture {stamp}"),
            ext: ".png",
            bytes: png.clone(),
        }),
        Clip::Dib(dib) => {
            let d = crate::img::bmp::decode(&bmp_from_dib(dib)?)?;
            Some(Made {
                base: format!("Pasted picture {stamp}"),
                ext: ".png",
                bytes: crate::img::pngenc::encode(&d.px, d.w, d.h),
            })
        }
        Clip::Text(text) => {
            if text.trim().is_empty() {
                return None;
            }
            match link(text) {
                Some(url) => Some(Made {
                    base: link_name(url),
                    ext: ".url",
                    bytes: format!("[InternetShortcut]\r\nURL={url}\r\n").into_bytes(),
                }),
                None => {
                    Some(Made { base: format!("Pasted text {stamp}"), ext: ".txt", bytes: text.as_bytes().to_vec() })
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_addresses_and_text() {
        assert_eq!(link("  https://github.com/ksanadv/dvfiles \r\n"), Some("https://github.com/ksanadv/dvfiles"));
        assert_eq!(link("see https://x.org now"), None);
        assert_eq!(link("ftp://x.org"), None);
        assert_eq!(link_name("https://www.github.com/a/b?c=d"), "github.com");
        assert_eq!(link_name("http://user@example.org:8080/x"), "example.org8080");
        let m = make(&Clip::Text("https://example.org/page".into()), "s").unwrap();
        assert_eq!((m.base.as_str(), m.ext), ("example.org", ".url"));
        assert_eq!(String::from_utf8(m.bytes).unwrap(), "[InternetShortcut]\r\nURL=https://example.org/page\r\n");
        let m = make(&Clip::Text("hello\nworld".into()), "2026-10-16 101530").unwrap();
        assert_eq!(
            (m.base.as_str(), m.ext, m.bytes.as_slice()),
            ("Pasted text 2026-10-16 101530", ".txt", b"hello\nworld".as_slice())
        );
        assert!(make(&Clip::Text("   ".into()), "s").is_none());
    }

    /// a 32-bit screenshot as Windows puts it on the clipboard (bottom-up, alpha all zero) becomes an opaque PNG
    #[test]
    fn a_screenshot_bitmap_becomes_a_png() {
        let (w, h) = (5i32, 3i32);
        let mut dib = Vec::new();
        dib.extend_from_slice(&40u32.to_le_bytes());
        dib.extend_from_slice(&w.to_le_bytes());
        dib.extend_from_slice(&h.to_le_bytes());
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&32u16.to_le_bytes());
        dib.extend_from_slice(&[0u8; 24]);
        // rows bottom-up: the bottom row red, the rest blue, alpha 0 as screenshots have it
        for y in 0..h {
            for _ in 0..w {
                dib.extend_from_slice(if y == 0 { &[0, 0, 255, 0] } else { &[255, 0, 0, 0] });
            }
        }
        let m = make(&Clip::Dib(dib), "s").expect("a picture");
        assert_eq!(m.ext, ".png");
        let d = crate::img::png::decode(&m.bytes).expect("a PNG");
        assert_eq!((d.w, d.h), (5, 3));
        assert_eq!(d.px[0], 0xFF00_00FF, "top row blue");
        assert_eq!(d.px[(2 * w) as usize], 0xFFFF_0000, "bottom row red");
        // a PNG on the clipboard is kept as it was; something else under that name is not
        let png = m.bytes.clone();
        assert_eq!(make(&Clip::Png(png.clone()), "t").unwrap().bytes, png);
        assert!(make(&Clip::Png(b"not a png".to_vec()), "t").is_none());
        assert!(bmp_from_dib(&[1, 2, 3]).is_none());
    }
}
