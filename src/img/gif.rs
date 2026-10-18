// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! GIF decoder (first image only) using only `std`: GIF87a/89a, global and local
//! color tables, LZW, interlacing, and transparency from the Graphic Control
//! Extension. The image is placed on a canvas of the logical screen size; pixels
//! outside it are transparent. If the image sticks out of the logical screen (a
//! broken but common case) the canvas grows to hold it, as browsers do.

use super::Decoded;

/// Largest canvas accepted, in pixels.
const MAX_PIXELS: u64 = 200_000_000;

/// Largest LZW code table (12-bit codes).
const MAX_CODES: usize = 4096;

/// What we need to know about the first image.
struct Frame<'a> {
    canvas_w: u32,
    canvas_h: u32,
    left: u32,
    top: u32,
    w: u32,
    h: u32,
    interlaced: bool,
    /// RGB triples of the color table in effect (local, else global; may be empty).
    palette: &'a [u8],
    /// Transparent color index from the Graphic Control Extension.
    transparent: Option<u8>,
    min_code_size: u8,
    /// Offset of the first LZW data sub-block.
    data_start: usize,
}

fn le16(data: &[u8], at: usize) -> Option<u32> {
    let b = data.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([b[0], b[1]]) as u32)
}

/// Reads the header and logical screen descriptor. Returns the screen size, the
/// offset after the global color table, and that table.
fn screen(data: &[u8]) -> Option<(u32, u32, usize, &[u8])> {
    let sig = data.get(..6)?;
    if sig != b"GIF87a" && sig != b"GIF89a" {
        return None;
    }
    let w = le16(data, 6)?;
    let h = le16(data, 8)?;
    let flags = *data.get(10)?;
    let mut pos = 13;
    let mut table: &[u8] = &[];
    if flags & 0x80 != 0 {
        let size = 3usize << ((flags & 7) + 1);
        table = data.get(pos..pos + size)?;
        pos += size;
    }
    Some((w, h, pos, table))
}

/// Skips a chain of data sub-blocks starting at `pos`; returns the offset after
/// the zero-length terminator.
fn skip_sub_blocks(data: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        let n = *data.get(pos)? as usize;
        pos += 1;
        if n == 0 {
            return Some(pos);
        }
        pos += n;
        if pos > data.len() {
            return None;
        }
    }
}

/// Walks the blocks up to the first image descriptor.
fn first_frame(data: &[u8]) -> Option<Frame<'_>> {
    let (screen_w, screen_h, mut pos, global) = screen(data)?;
    let mut transparent = None;
    loop {
        match *data.get(pos)? {
            // Extension: only the Graphic Control Extension (0xF9) matters here.
            0x21 => {
                if *data.get(pos + 1)? == 0xF9 && *data.get(pos + 2)? >= 4 {
                    let flags = *data.get(pos + 3)?;
                    let index = *data.get(pos + 6)?;
                    transparent = if flags & 1 != 0 { Some(index) } else { None };
                }
                pos = skip_sub_blocks(data, pos + 2)?;
            }
            // Image descriptor.
            0x2C => {
                let left = le16(data, pos + 1)?;
                let top = le16(data, pos + 3)?;
                let w = le16(data, pos + 5)?;
                let h = le16(data, pos + 7)?;
                let flags = *data.get(pos + 9)?;
                pos += 10;
                let mut palette = global;
                if flags & 0x80 != 0 {
                    let size = 3usize << ((flags & 7) + 1);
                    palette = data.get(pos..pos + size)?;
                    pos += size;
                }
                let min_code_size = *data.get(pos)?;
                if w == 0 || h == 0 {
                    return None;
                }
                let canvas_w = screen_w.max(left + w);
                let canvas_h = screen_h.max(top + h);
                if canvas_w as u64 * canvas_h as u64 > MAX_PIXELS {
                    return None;
                }
                return Some(Frame {
                    canvas_w,
                    canvas_h,
                    left,
                    top,
                    w,
                    h,
                    interlaced: flags & 0x40 != 0,
                    palette,
                    transparent,
                    min_code_size,
                    data_start: pos + 1,
                });
            }
            // Stray zero bytes between blocks are tolerated.
            0x00 => pos += 1,
            // Trailer (0x3B) before any image, or garbage.
            _ => return None,
        }
    }
}

/// Canvas size (the logical screen, grown to fit the first image if needed).
/// If the first image descriptor cannot be read, the logical screen size.
pub fn dimensions(data: &[u8]) -> Option<(u32, u32)> {
    if let Some(f) = first_frame(data) {
        return Some((f.canvas_w, f.canvas_h));
    }
    let (w, h, _, _) = screen(data)?;
    if w == 0 || h == 0 || w as u64 * h as u64 > MAX_PIXELS {
        return None;
    }
    Some((w, h))
}

/// Decodes the first image onto its canvas, as premultiplied BGRA.
pub fn decode(data: &[u8]) -> Option<Decoded> {
    let f = first_frame(data)?;

    // Join the LZW data sub-blocks; a file cut off inside them is rejected.
    let mut lzw = Vec::new();
    let mut pos = f.data_start;
    loop {
        let n = *data.get(pos)? as usize;
        pos += 1;
        if n == 0 {
            break;
        }
        lzw.extend_from_slice(data.get(pos..pos + n)?);
        pos += n;
    }

    // Color of each index: opaque palette colors, black past the end of the
    // palette, and fully transparent (0) for the transparent index.
    let mut colors = [0xFF00_0000u32; 256];
    for (c, rgb) in colors.iter_mut().zip(f.palette.chunks_exact(3)) {
        *c = 0xFF00_0000 | (rgb[0] as u32) << 16 | (rgb[1] as u32) << 8 | rgb[2] as u32;
    }
    if let Some(t) = f.transparent {
        colors[t as usize] = 0;
    }

    // Canvas rows of the image in the order they are stored.
    let rows: Vec<u32> = if f.interlaced {
        [(0, 8), (4, 8), (2, 4), (1, 2)]
            .iter()
            .flat_map(|&(start, step)| (start..f.h).step_by(step))
            .map(|y| f.top + y)
            .collect()
    } else {
        (f.top..f.top + f.h).collect()
    };

    let cw = f.canvas_w as usize;
    let mut px = vec![0u32; cw * f.canvas_h as usize];
    let (mut x, mut r) = (0usize, 0usize);
    let mut row_start = rows.first().map_or(0, |&y| y as usize * cw + f.left as usize);
    lzw_decode(&lzw, f.min_code_size, |index| {
        if let Some(p) = px.get_mut(row_start + x) {
            *p = colors[index as usize];
        }
        x += 1;
        if x == f.w as usize {
            x = 0;
            r += 1;
            match rows.get(r) {
                Some(&y) => row_start = y as usize * cw + f.left as usize,
                None => return false, // image complete
            }
        }
        true
    })?;
    Some(Decoded { w: f.canvas_w, h: f.canvas_h, px })
}

/// Decodes GIF LZW data, calling `emit` with each color index until it returns
/// false or the data ends. An end code, the end of the data or an invalid code stops
/// decoding early; the pixels already produced are kept (the rest stay transparent).
fn lzw_decode(data: &[u8], min_size: u8, mut emit: impl FnMut(u8) -> bool) -> Option<()> {
    if !(1..=8).contains(&min_size) {
        return None;
    }
    let clear = 1usize << min_size;
    let end = clear + 1;

    // The string table: each code is a previous code plus one byte.
    let mut prefix = [0u16; MAX_CODES];
    let mut suffix = [0u8; MAX_CODES];
    let mut first = [0u8; MAX_CODES]; // first byte of each string
    let mut length = [0u16; MAX_CODES];
    for i in 0..clear {
        suffix[i] = i as u8;
        first[i] = i as u8;
        length[i] = 1;
    }
    let mut size = min_size as u32 + 1;
    let mut next = clear + 2;
    let mut prev: Option<usize> = None;
    let mut stack = [0u8; MAX_CODES];

    // LSB-first bit reader.
    let (mut bits, mut nbits, mut pos) = (0u32, 0u32, 0usize);
    loop {
        while nbits < size {
            let Some(&b) = data.get(pos) else { return Some(()) };
            bits |= (b as u32) << nbits;
            nbits += 8;
            pos += 1;
        }
        let code = (bits & ((1 << size) - 1)) as usize; // < 4096
        bits >>= size;
        nbits -= size;

        if code == clear {
            size = min_size as u32 + 1;
            next = clear + 2;
            prev = None;
            continue;
        }
        if code == end {
            return Some(());
        }
        match prev {
            // After a clear code only plain color indexes are valid.
            None if code >= clear => return Some(()),
            None => {}
            Some(p) => {
                // `code == next` is the "string + its own first byte" case.
                if code > next || (code == next && next >= MAX_CODES) {
                    return Some(());
                }
                if next < MAX_CODES {
                    prefix[next] = p as u16;
                    suffix[next] = if code < next { first[code] } else { first[p] };
                    first[next] = first[p];
                    length[next] = length[p] + 1;
                    next += 1;
                    if next == 1 << size && size < 12 {
                        size += 1;
                    }
                }
            }
        }

        // Write the string for `code` back to front, then emit it.
        let n = length[code] as usize;
        let mut c = code;
        for slot in stack[..n].iter_mut().rev() {
            *slot = suffix[c];
            c = prefix[c] as usize;
        }
        for &b in &stack[..n] {
            if !emit(b) {
                return Some(());
            }
        }
        prev = Some(code);
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil as tu;
    use super::*;
    use std::collections::HashMap;
    use windows::Win32::Graphics::Imaging::*;

    // ------------------------------------------------------------ hand-made GIF writer

    /// A real LZW encoder (GIF flavor). With `clear_when_full` it emits a clear code
    /// when the table fills up; otherwise it keeps going with the full table.
    fn lzw_encode(indices: &[u8], min: u8, clear_when_full: bool) -> Vec<u8> {
        let clear = 1u16 << min;
        let end = clear + 1;
        let mut out = Vec::new();
        let (mut acc, mut n) = (0u32, 0u32);
        let mut put = |code: u16, size: u32, out: &mut Vec<u8>| {
            acc |= (code as u32) << n;
            n += size;
            while n >= 8 {
                out.push(acc as u8);
                acc >>= 8;
                n -= 8;
            }
        };
        let mut dict: HashMap<(u16, u8), u16> = HashMap::new();
        let mut next = end + 1;
        let mut size = min as u32 + 1;
        put(clear, size, &mut out);
        let mut cur: Option<u16> = None;
        for &k in indices {
            let Some(c) = cur else {
                cur = Some(k as u16);
                continue;
            };
            if let Some(&code) = dict.get(&(c, k)) {
                cur = Some(code);
                continue;
            }
            put(c, size, &mut out);
            if next < 4096 {
                dict.insert((c, k), next);
                next += 1;
                if next as u32 > (1 << size) && size < 12 {
                    size += 1;
                }
            } else if clear_when_full {
                put(clear, size, &mut out);
                dict.clear();
                next = end + 1;
                size = min as u32 + 1;
            }
            cur = Some(k as u16);
        }
        if let Some(c) = cur {
            put(c, size, &mut out);
        }
        put(end, size, &mut out);
        if n > 0 {
            out.push(acc as u8);
        }
        out
    }

    /// "Uncompressed" LZW: only literal codes, with a clear code often enough that the
    /// code size never grows.
    fn lzw_literal(indices: &[u8], min: u8) -> Vec<u8> {
        let clear = 1u32 << min;
        let size = min as u32 + 1;
        let per_run = (1usize << size) - clear as usize - 3;
        let mut codes = vec![clear];
        for (i, &k) in indices.iter().enumerate() {
            if i > 0 && i % per_run == 0 {
                codes.push(clear);
            }
            codes.push(k as u32);
        }
        codes.push(clear + 1);
        let mut out = Vec::new();
        let (mut acc, mut n) = (0u64, 0u32);
        for c in codes {
            acc |= (c as u64) << n;
            n += size;
            while n >= 8 {
                out.push(acc as u8);
                acc >>= 8;
                n -= 8;
            }
        }
        if n > 0 {
            out.push(acc as u8);
        }
        out
    }

    struct Image {
        left: u16,
        top: u16,
        w: u16,
        h: u16,
        local: Option<Vec<[u8; 3]>>,
        interlaced: bool,
        transparent: Option<u8>,
        /// Color indexes, row-major in display order.
        indices: Vec<u8>,
        min_code: u8,
        /// 0: real LZW clearing when full, 1: real LZW never clearing, 2: literal codes.
        coder: u8,
    }

    fn table_bits(n: usize) -> u8 {
        (0..8).find(|&b| 2usize << b >= n).unwrap_or(7) as u8
    }

    fn write_gif(version: &[u8; 6], screen: (u16, u16), global: Option<&[[u8; 3]]>, images: &[Image]) -> Vec<u8> {
        let mut out = version.to_vec();
        out.extend_from_slice(&screen.0.to_le_bytes());
        out.extend_from_slice(&screen.1.to_le_bytes());
        let push_table = |out: &mut Vec<u8>, t: &[[u8; 3]]| {
            let bits = table_bits(t.len());
            for i in 0..(2usize << bits) {
                out.extend_from_slice(&t.get(i).copied().unwrap_or([0, 0, 0]));
            }
        };
        match global {
            Some(t) => {
                out.extend_from_slice(&[0x80 | 0x70 | table_bits(t.len()), 0, 0]);
                push_table(&mut out, t);
            }
            None => out.extend_from_slice(&[0, 0, 0]),
        }
        // An application extension and a comment before the image, to be skipped.
        out.extend_from_slice(&[0x21, 0xFF, 11]);
        out.extend_from_slice(b"NETSCAPE2.0");
        out.extend_from_slice(&[3, 1, 0, 0, 0]);
        out.extend_from_slice(&[0x21, 0xFE, 5]);
        out.extend_from_slice(b"hello");
        out.push(0);
        for im in images {
            if let Some(t) = im.transparent {
                out.extend_from_slice(&[0x21, 0xF9, 4, 0x01, 10, 0, t, 0]);
            }
            out.push(0x2C);
            for v in [im.left, im.top, im.w, im.h] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            let mut flags = if im.interlaced { 0x40 } else { 0 };
            if let Some(t) = &im.local {
                flags |= 0x80 | table_bits(t.len());
            }
            out.push(flags);
            if let Some(t) = &im.local {
                push_table(&mut out, t);
            }
            // Rows in stored order.
            let w = im.w as usize;
            let order: Vec<usize> = if im.interlaced {
                [(0, 8), (4, 8), (2, 4), (1, 2)].iter().flat_map(|&(s, st)| (s..im.h as usize).step_by(st)).collect()
            } else {
                (0..im.h as usize).collect()
            };
            let stored: Vec<u8> = order.iter().flat_map(|&y| im.indices[y * w..y * w + w].iter().copied()).collect();
            let data = match im.coder {
                0 => lzw_encode(&stored, im.min_code, true),
                1 => lzw_encode(&stored, im.min_code, false),
                _ => lzw_literal(&stored, im.min_code),
            };
            out.push(im.min_code);
            for chunk in data.chunks(255) {
                out.push(chunk.len() as u8);
                out.extend_from_slice(chunk);
            }
            out.push(0);
        }
        out.push(0x3B);
        out
    }

    /// The canvas the decoder must produce for the first image.
    fn expected(screen: (u16, u16), global: Option<&[[u8; 3]]>, im: &Image) -> (u32, u32, Vec<u32>) {
        let cw = (screen.0 as u32).max(im.left as u32 + im.w as u32);
        let ch = (screen.1 as u32).max(im.top as u32 + im.h as u32);
        let table = im.local.as_deref().or(global).unwrap_or(&[]);
        let mut px = vec![0u32; (cw * ch) as usize];
        for y in 0..im.h as u32 {
            for x in 0..im.w as u32 {
                let k = im.indices[(y * im.w as u32 + x) as usize];
                let c = if Some(k) == im.transparent {
                    0
                } else {
                    let rgb = table.get(k as usize).copied().unwrap_or([0, 0, 0]);
                    0xFF00_0000 | (rgb[0] as u32) << 16 | (rgb[1] as u32) << 8 | rgb[2] as u32
                };
                px[((im.top as u32 + y) * cw + im.left as u32 + x) as usize] = c;
            }
        }
        (cw, ch, px)
    }

    fn colors(n: usize, seed: u64) -> Vec<[u8; 3]> {
        let mut rng = tu::Rng(seed);
        (0..n)
            .map(|_| {
                let r = rng.next();
                [r as u8, (r >> 8) as u8, (r >> 16) as u8]
            })
            .collect()
    }

    /// Hand-made GIFs covering the features: (name, screen, global table, images).
    fn hand_made() -> Vec<(String, (u16, u16), Option<Vec<[u8; 3]>>, Vec<Image>)> {
        let mut out = Vec::new();
        let mut rng = tu::Rng(31);
        let mut pattern = |w: u16, h: u16, colors: usize, runs: bool| -> Vec<u8> {
            (0..w as usize * h as usize)
                .map(|i| {
                    if runs {
                        // Long runs and repeats: exercises the "KwKwK" case and table growth.
                        ((i / 7 + (i / w as usize) / 3) % colors) as u8
                    } else {
                        (rng.next() % colors as u64) as u8
                    }
                })
                .collect()
        };
        let cases: [(&str, (u16, u16), u16, u16, u16, u16, usize, bool, bool, Option<u8>, bool, u8, u8); 13] = [
            // name, screen, left, top, w, h, colors, local table, interlaced, transparent, runs, min code, coder
            ("plain_256", (40, 30), 0, 0, 40, 30, 256, false, false, None, false, 8, 0),
            ("runs_256_big", (300, 200), 0, 0, 300, 200, 256, false, false, None, true, 8, 0),
            ("runs_noclear", (300, 200), 0, 0, 300, 200, 200, false, false, None, true, 8, 1),
            ("noise_noclear", (200, 150), 0, 0, 200, 150, 256, false, false, None, false, 8, 1),
            ("four_colors", (33, 17), 0, 0, 33, 17, 4, false, false, None, false, 2, 0),
            ("two_colors_runs", (64, 9), 0, 0, 64, 9, 2, false, false, None, true, 2, 0),
            ("interlaced", (37, 29), 0, 0, 37, 29, 64, false, true, None, false, 6, 0),
            ("interlaced_tiny", (5, 3), 0, 0, 5, 3, 16, false, true, None, false, 4, 0),
            ("transparent", (40, 30), 0, 0, 40, 30, 16, false, false, Some(3), false, 4, 0),
            ("local_table_offset", (60, 50), 7, 11, 30, 20, 32, true, false, Some(0), false, 5, 0),
            ("beyond_screen", (20, 10), 15, 5, 30, 20, 16, false, false, None, false, 4, 0),
            ("literal_codes", (50, 40), 3, 2, 41, 33, 128, false, true, Some(5), false, 7, 2),
            ("short_palette", (20, 20), 0, 0, 20, 20, 200, false, false, None, false, 8, 0),
        ];
        for (name, screen, left, top, w, h, ncolors, local, interlaced, transparent, runs, min_code, coder) in cases {
            let table = colors(if name == "short_palette" { 100 } else { ncolors }, w as u64 * 7 + h as u64);
            let image = Image {
                left,
                top,
                w,
                h,
                local: if local { Some(table.clone()) } else { None },
                interlaced,
                transparent,
                indices: pattern(w, h, ncolors, runs),
                min_code,
                coder,
            };
            // A second image that must be ignored.
            let second = Image {
                left: 0,
                top: 0,
                w: 2,
                h: 2,
                local: None,
                interlaced: false,
                transparent: None,
                indices: vec![1; 4],
                min_code: 2,
                coder: 0,
            };
            let global = if local { Some(colors(4, 5)) } else { Some(table) };
            out.push((name.to_string(), screen, global, vec![image, second]));
        }
        out
    }

    /// Frame rectangle of the first image (for comparing with WIC, whose frame 0 is
    /// just that rectangle).
    fn frame_rect(data: &[u8]) -> Option<(u32, u32, u32, u32)> {
        first_frame(data).map(|f| (f.left, f.top, f.w, f.h))
    }

    fn compare_with_wic(name: &str, data: &[u8], tally: &mut tu::Tally) {
        let ours = decode(data);
        let wic = tu::wic_decode(data);
        match (ours, wic, frame_rect(data)) {
            (Some(o), Ok(w), Some((left, top, fw, fh))) if (w.w, w.h) == (fw, fh) => {
                let d = tu::compare_region(&o.px, o.w, left, top, &w.px, fw, fh);
                tally.add_diff(name, &d, &format!("frame {}x{} at {},{} on {}x{}", fw, fh, left, top, o.w, o.h));
            }
            (Some(o), Ok(w), r) => tally
                .size_mismatch
                .push(format!("{}: ours canvas {}x{} frame {:?}, WIC {}x{}", name, o.w, o.h, r, w.w, w.h)),
            (None, Ok(_), _) => tally.ours_failed.push(name.to_string()),
            (Some(_), Err(e), _) => tally.wic_failed.push(format!("{}: {}", name, e)),
            (None, Err(e), _) => tally.wic_failed.push(format!("{}: both failed ({})", name, e)),
        }
    }

    #[test]
    fn hand_made_vs_expected_and_wic() {
        let mut tally = tu::Tally::default();
        for (name, screen, global, images) in hand_made() {
            for version in [b"GIF89a", b"GIF87a"] {
                if version == b"GIF87a" && images[0].transparent.is_some() {
                    continue;
                }
                let file = write_gif(version, screen, global.as_deref(), &images);
                let label = format!("{}_{}", name, std::str::from_utf8(&version[3..]).unwrap());
                let (cw, ch, want) = expected(screen, global.as_deref(), &images[0]);
                let ours = decode(&file).unwrap_or_else(|| panic!("{}: decode failed", label));
                assert_eq!((ours.w, ours.h), (cw, ch), "{}", label);
                assert_eq!(dimensions(&file), Some((cw, ch)), "{}", label);
                let d = tu::compare(&ours.px, &want, cw, ch);
                assert!(d.max == 0, "{}: differs from expected: {:?}", label, d);
                compare_with_wic(&label, &file, &mut tally);
            }
        }
        tally.print("hand-made GIFs vs WIC (frame rectangle)");
        assert!(tally.differ.is_empty() && tally.wic_failed.is_empty());
        assert!(tally.ours_failed.is_empty() && tally.size_mismatch.is_empty());
    }

    /// Writes GIFs with WIC's encoder (8bppIndexed) into `folder`.
    fn wic_variants(folder: &str) -> Vec<(String, std::path::PathBuf)> {
        let dir = tu::testdata_dir(folder);
        let mut made = Vec::new();
        for (i, (w, h)) in [(67u32, 45u32), (300, 200), (1, 1)].into_iter().enumerate() {
            let mut rng = tu::Rng(i as u64 + 3);
            let px: Vec<u8> = (0..w * h).map(|k| ((k % w) / 3 + (k / w) / 2) as u8 ^ (rng.next() as u8 & 3)).collect();
            let pal: Vec<u32> = (0..256u32).map(|c| 0xFF00_0000 | c.wrapping_mul(0x010307)).collect();
            let path = dir.join(format!("wic_{}x{}.gif", w, h));
            match tu::wic_encode(
                &path,
                &GUID_ContainerFormatGif,
                &GUID_WICPixelFormat8bppIndexed,
                w,
                h,
                w,
                &px,
                Some(&pal),
                &[],
            ) {
                Ok(()) => made.push((format!("wic_{}x{}", w, h), path)),
                Err(e) => println!("could not produce GIF {}x{}: {}", w, h, e),
            }
        }
        made
    }

    #[test]
    fn wic_encoded_vs_wic() {
        let mut tally = tu::Tally::default();
        for (name, path) in wic_variants("gif_wic") {
            compare_with_wic(&name, &std::fs::read(path).unwrap(), &mut tally);
        }
        tally.print("WIC-encoded GIFs");
        assert!(tally.differ.is_empty() && tally.wic_failed.is_empty());
        assert!(tally.ours_failed.is_empty() && tally.size_mismatch.is_empty());
    }

    #[test]
    fn windows_samples_vs_wic() {
        // reads samples from all over C:\Windows: run with DVFILES_SHELLTEST=1
        if std::env::var_os("DVFILES_SHELLTEST").is_none() {
            return;
        }
        let files: Vec<(String, Vec<u8>)> =
            tu::samples().gif.iter().filter_map(|p| std::fs::read(p).ok().map(|d| (tu::short(p), d))).collect();
        let mut tally = tu::Tally::default();
        let mut features = (0, 0, 0, 0); // interlaced, transparent, local table, offset/grown canvas
        for (name, data) in &files {
            if let Some(f) = first_frame(data) {
                features.0 += f.interlaced as usize;
                features.1 += f.transparent.is_some() as usize;
                features.2 += (data.get(10).map_or(true, |b| b & 0x80 == 0)
                    || f.palette.as_ptr() != screen(data).unwrap().3.as_ptr()) as usize;
                features.3 += (f.left > 0 || f.top > 0 || f.w != f.canvas_w || f.h != f.canvas_h) as usize;
            }
            compare_with_wic(name, data, &mut tally);
            // Outside the frame the canvas must be transparent.
            if let (Some(img), Some((l, t, w, h))) = (decode(data), frame_rect(data)) {
                for y in 0..img.h {
                    for x in 0..img.w {
                        if x < l || y < t || x >= l + w || y >= t + h {
                            assert_eq!(img.px[(y * img.w + x) as usize], 0, "{}: outside pixel", name);
                        }
                    }
                }
            }
        }
        println!(
            "Windows GIF samples: {} files (interlaced {}, transparent {}, local/no global table {}, frame smaller than canvas {})",
            files.len(),
            features.0,
            features.1,
            features.2,
            features.3
        );
        tally.print("Windows GIF samples (first frame rectangle)");
        assert!(tally.differ.is_empty() && tally.wic_failed.is_empty());
        assert!(tally.ours_failed.is_empty() && tally.size_mismatch.is_empty());
    }

    #[test]
    fn errors_and_limits() {
        let (name, screen, global, images) = hand_made().remove(0);
        let good = write_gif(b"GIF89a", screen, global.as_deref(), &images);
        assert!(decode(&good).is_some(), "{}", name);
        let mut bad = good.clone();
        bad[4] = b'8';
        assert!(decode(&bad).is_none() && dimensions(&bad).is_none(), "bad signature");
        // No image before the trailer.
        let mut no_image = good[..13 + 768].to_vec();
        no_image.push(0x3B);
        assert!(decode(&no_image).is_none());
        assert_eq!(dimensions(&no_image), Some((40, 30)), "falls back to the screen size");
        // Truncated inside the image data.
        assert!(decode(&good[..good.len() - 40]).is_none());
        // Huge canvas (65535 x 65535) is rejected.
        let mut huge = good.clone();
        huge[6..10].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
        assert!(decode(&huge).is_none() && dimensions(&huge).is_none());
        // Invalid minimum code size.
        let pos = first_frame(&good).unwrap().data_start - 1;
        let mut bad_code = good.clone();
        bad_code[pos] = 12;
        assert!(decode(&bad_code).is_none());
    }

    #[test]
    #[cfg_attr(not(debug_assertions), ignore = "fuzz: run in a debug build")]
    fn fuzz_no_panic() {
        let mut files: Vec<(String, Vec<u8>)> =
            hand_made().into_iter().map(|(n, s, g, im)| (n, write_gif(b"GIF89a", s, g.as_deref(), &im))).collect();
        for (n, p) in wic_variants("gif_wic_fuzz") {
            files.push((n, std::fs::read(p).unwrap()));
        }
        for p in &tu::samples().gif {
            if let Ok(d) = std::fs::read(p) {
                files.push((tu::short(p), d));
            }
        }
        let f = |d: &[u8]| {
            let _ = dimensions(d);
            let _ = decode(d);
        };
        let (n, cases, fails) = tu::fuzz_files(&files, &f);
        println!("GIF fuzz: {} files, {} cases, {} panics", n, cases, fails.len());
        assert!(fails.is_empty(), "{:#?}", fails);
    }
}
