// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Pictures decoded by dvfiles itself, for thumbnails, the preview pane and Properties: JPEG (baseline and
//! progressive, decoded straight at 1/2, 1/4 or 1/8 of its size), PNG (with its own inflate), GIF (the first frame)
//! and BMP. Each decoder only reads the bytes it is given, never panics on bad ones (it says None), and is checked
//! against Windows' own decoder in its tests. What they cannot read (CMYK or arithmetic-coded JPEG, HEIC, RAW, video)
//! is left to Windows' thumbnails.
pub mod bmp;
pub mod gif;
pub mod inflate;
pub mod jpeg;
pub mod jpegenc;
pub mod png;
pub mod pngenc;

#[cfg(test)]
mod testutil;

/// Decoded pixels: premultiplied BGRA packed in u32 as (a << 24) | (r << 16) | (g << 8) | b, top row first,
/// row-major, w * h values.
pub struct Decoded {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Jpeg,
    Png,
    Gif,
    Bmp,
}

/// the format, from the file's first bytes (the extension can lie)
pub fn sniff(data: &[u8]) -> Option<Format> {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Format::Jpeg)
    } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Format::Png)
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        Some(Format::Gif)
    } else if data.starts_with(b"BM") {
        Some(Format::Bmp)
    } else {
        None
    }
}

/// whether dvfiles reads pictures with this extension itself
pub fn readable(ext: &str) -> bool {
    matches!(ext, "jpg" | "jpeg" | "jpe" | "jfif" | "png" | "apng" | "gif" | "bmp" | "dib")
}

/// a picture's size, from its header
pub fn dimensions(data: &[u8]) -> Option<(u32, u32)> {
    match sniff(data)? {
        Format::Jpeg => jpeg::dimensions(data),
        Format::Png => png::dimensions(data),
        Format::Gif => gif::dimensions(data),
        Format::Bmp => bmp::dimensions(data),
    }
}

/// whether a JPEG is progressive (its frame header, before the first scan, is SOF2)
fn progressive(data: &[u8]) -> bool {
    let mut at = 2usize;
    while at + 4 <= data.len() {
        if data[at] != 0xFF {
            return false;
        }
        let marker = data[at + 1];
        if marker == 0xC2 {
            return true;
        }
        if marker == 0xDA || marker == 0xD9 {
            return false;
        }
        let len = u16::from_be_bytes([data[at + 2], data[at + 3]]) as usize;
        at += 2 + len;
    }
    false
}

/// About how much memory decoding a picture takes (its file's bytes included), from its header alone, so that
/// several decodes at once can be kept within a budget: PNG, GIF and BMP are decoded whole before they are shrunk;
/// a JPEG is decoded small, but a progressive one keeps its coefficients until its last scan.
pub fn decode_cost(head: &[u8], file_len: u64) -> Option<u64> {
    let (w, h) = dimensions(head)?;
    let px = w as u64 * h as u64;
    let work = match sniff(head)? {
        // the inflated rows (up to 8 bytes a pixel) and the pixels
        Format::Png => px * 12,
        Format::Gif => px * 5,
        Format::Bmp => px * 4,
        Format::Jpeg if progressive(head) => px * 3,
        Format::Jpeg => px / 4,
    };
    Some(work + file_len)
}

/// Decodes a picture at least `min_side` pixels on its longer side where the format allows decoding smaller (JPEG),
/// full size otherwise, upright (a JPEG's EXIF orientation applied).
pub fn decode(data: &[u8], min_side: u32) -> Option<Decoded> {
    match sniff(data)? {
        Format::Jpeg => jpeg::decode(data, min_side),
        Format::Png => png::decode(data),
        Format::Gif => gif::decode(data),
        Format::Bmp => bmp::decode(data),
    }
}

/// A picture for a box `bw` x `bh`: from a JPEG, the small picture a camera or phone stores in it when that is big
/// enough (decoding it takes a fraction of a millisecond), else the picture itself at the smallest scale that covers
/// the box; shrunk to fit.
pub fn thumbnail(data: &[u8], bw: u32, bh: u32) -> Option<Decoded> {
    if let Some(small) = stored_thumbnail(data, bw, bh) {
        return Some(small);
    }
    Some(fit(decode(data, bw.max(bh))?, bw, bh))
}

/// The small picture a camera stores in a JPEG's EXIF block, fitted to the box, when it is big enough for it. The block
/// (at most 64 KB) comes near the start of the file, so `data` may be just its start: a photo need not be read whole.
pub fn stored_thumbnail(data: &[u8], bw: u32, bh: u32) -> Option<Decoded> {
    let side = bw.max(bh);
    if sniff(data) != Some(Format::Jpeg) {
        return None;
    }
    let small = jpeg::exif_thumbnail(data).and_then(|t| jpeg::decode(t, side))?;
    if small.w.max(small.h) < side {
        return None;
    }
    let o = jpeg::exif(data).map_or(1, |e| e.orientation);
    Some(fit(orient(small, o), bw, bh))
}

/// A picture shrunk to fit inside `bw` x `bh`, keeping its shape, each new pixel the average of the ones it covers
/// (premultiplied, so edges against transparency stay clean). One no bigger than the box comes back as it is.
pub fn fit(d: Decoded, bw: u32, bh: u32) -> Decoded {
    if d.w == 0 || d.h == 0 || (d.w <= bw && d.h <= bh) {
        return d;
    }
    fit_ref(&d, bw, bh)
}

/// as `fit`, keeping the picture: a copy when it already fits
pub fn fit_ref(d: &Decoded, bw: u32, bh: u32) -> Decoded {
    if d.w == 0 || d.h == 0 || (d.w <= bw && d.h <= bh) {
        return Decoded { w: d.w, h: d.h, px: d.px.clone() };
    }
    let k = (bw as f64 / d.w as f64).min(bh as f64 / d.h as f64);
    let (nw, nh) = (((d.w as f64 * k).round() as u32).max(1), ((d.h as f64 * k).round() as u32).max(1));
    let mut out = Vec::with_capacity((nw * nh) as usize);
    // each new column's and row's span of old ones
    let span = |i: u32, n: u32, old: u32| -> (u32, u32) {
        let a = (i as u64 * old as u64 / n as u64) as u32;
        let b = (((i + 1) as u64 * old as u64).div_ceil(n as u64) as u32).clamp(a + 1, old);
        (a, b)
    };
    let cols: Vec<(u32, u32)> = (0..nw).map(|x| span(x, nw, d.w)).collect();
    for y in 0..nh {
        let (y0, y1) = span(y, nh, d.h);
        for &(x0, x1) in &cols {
            let mut acc = [0u32; 4];
            for yy in y0..y1 {
                let row = &d.px[(yy * d.w) as usize..][..d.w as usize];
                for &p in &row[x0 as usize..x1 as usize] {
                    acc[0] += p >> 24;
                    acc[1] += p >> 16 & 0xFF;
                    acc[2] += p >> 8 & 0xFF;
                    acc[3] += p & 0xFF;
                }
            }
            let n = (y1 - y0) * (x1 - x0);
            let c = |v: u32| (v + n / 2) / n;
            out.push(c(acc[0]) << 24 | c(acc[1]) << 16 | c(acc[2]) << 8 | c(acc[3]));
        }
    }
    Decoded { w: nw, h: nh, px: out }
}

/// turns a picture as an EXIF orientation (1..8) says, for the small thumbnail a camera stores, which has none of its own
pub fn orient(d: Decoded, orientation: u16) -> Decoded {
    if !(2..=8).contains(&orientation) {
        return d;
    }
    let (w, h) = (d.w as usize, d.h as usize);
    let swap = orientation >= 5;
    let (nw, nh) = if swap { (h, w) } else { (w, h) };
    let mut out = vec![0u32; w * h];
    for y in 0..h {
        for x in 0..w {
            // where pixel (x, y) of the stored picture goes in the upright one
            let (nx, ny) = match orientation {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (h - 1 - y, x),
                7 => (h - 1 - y, w - 1 - x),
                _ => (y, w - 1 - x),
            };
            out[ny * nw + nx] = d.px[y * w + x];
        }
    }
    Decoded { w: nw as u32, h: nh as u32, px: out }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pic(w: u32, h: u32) -> Decoded {
        Decoded { w, h, px: (0..w * h).map(|i| 0xFF00_0000 | i).collect() }
    }

    #[test]
    fn fitting_keeps_the_shape_and_averages() {
        let d = Decoded {
            w: 4,
            h: 2,
            px: vec![
                0xFF00_0000,
                0xFF00_00FF,
                0xFF00_0000,
                0xFF00_00FF,
                0xFF00_0000,
                0xFF00_00FF,
                0xFF00_0000,
                0xFF00_00FF,
            ],
        };
        let f = fit(d, 2, 2);
        assert_eq!((f.w, f.h), (2, 1));
        // black and blue averaged
        assert_eq!(f.px[0] & 0xFF, 128);
        assert_eq!(f.px[0] >> 24, 255);
        let big = fit(pic(1000, 500), 96, 96);
        assert_eq!((big.w, big.h), (96, 48));
        let small = fit(pic(10, 10), 96, 96);
        assert_eq!((small.w, small.h), (10, 10));
    }

    #[test]
    fn orientations() {
        // 3x2: values 0..6
        let o6 = orient(pic(3, 2), 6);
        assert_eq!((o6.w, o6.h), (2, 3));
        // turned clockwise: the old bottom-left (3) is now the top-left
        assert_eq!(o6.px[0] & 0xFF, 3);
        let o3 = orient(pic(3, 2), 3);
        assert_eq!(o3.px[0] & 0xFF, 5);
        let o1 = orient(pic(3, 2), 1);
        assert_eq!(o1.px[0] & 0xFF, 0);
    }

    #[test]
    fn costs() {
        // a 4000x3000 PNG costs about 144 MB to decode whole; a baseline JPEG of that size little
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&4000u32.to_be_bytes());
        png.extend_from_slice(&3000u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        if let Some(c) = decode_cost(&png, 1000) {
            assert_eq!(c, 4000 * 3000 * 12 + 1000);
        }
        assert!(!progressive(&[0xFF, 0xD8, 0xFF, 0xC0, 0, 2]));
        assert!(progressive(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 2, 0xFF, 0xC2, 0, 2]));
        assert!(!progressive(&[0xFF, 0xD8, 0xFF, 0xDA, 0, 2, 0xFF, 0xC2]));
    }

    #[test]
    fn formats_by_their_first_bytes() {
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(Format::Jpeg));
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Some(Format::Png));
        assert_eq!(sniff(b"GIF89a"), Some(Format::Gif));
        assert_eq!(sniff(b"BM......"), Some(Format::Bmp));
        assert_eq!(sniff(b"RIFF"), None);
    }
}
