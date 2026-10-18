// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! BMP decoder using only `std`: BITMAPCOREHEADER (OS/2 1.x), BITMAPINFOHEADER and
//! its V2-V5 extensions (and OS/2 2.x headers), bottom-up and top-down rows, 1/2/4/8-bit
//! palettes, 16- and 32-bit pixels with default or BI_BITFIELDS masks, 24-bit, and
//! RLE8/RLE4 compression.
//!
//! Alpha: 32-bit BI_RGB images are opaque unless the header is V4/V5 with an alpha
//! mask; with BI_BITFIELDS an alpha mask is used when the header has one. If every
//! alpha value is 0 the image is treated as opaque. Pixels skipped by RLE escapes are
//! transparent.

use super::Decoded;

/// Largest image accepted, in pixels.
const MAX_PIXELS: u64 = 200_000_000;

const BI_RGB: u32 = 0;
const BI_RLE8: u32 = 1;
const BI_RLE4: u32 = 2;
const BI_BITFIELDS: u32 = 3;
const BI_ALPHABITFIELDS: u32 = 6;

fn le16(data: &[u8], at: usize) -> Option<u32> {
    let b = data.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([b[0], b[1]]) as u32)
}

fn le32(data: &[u8], at: usize) -> Option<u32> {
    let b = data.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// The header fields we use.
struct Header {
    w: u32,
    h: u32,
    top_down: bool,
    bpp: u32,
    compression: u32,
    /// Offset of the pixel data in the file.
    data_offset: usize,
    /// Palette as opaque packed colors (1-8 bit images).
    palette: Vec<u32>,
    /// Red, green, blue and alpha masks (16- and 32-bit images).
    masks: [u32; 4],
}

/// Reads the file header, the bitmap header, the masks and the palette.
fn parse(data: &[u8]) -> Option<Header> {
    if data.get(..2)? != b"BM" {
        return None;
    }
    let off_bits = le32(data, 10)? as usize;
    let hsize = le32(data, 14)? as usize;
    let windows_header = matches!(hsize, 40 | 52 | 56 | 108 | 124);
    let os2_v2_header = (16..=64).contains(&hsize) && !windows_header;

    let (w, h, top_down, bpp, compression, colors_used, entry_size);
    if hsize == 12 {
        // BITMAPCOREHEADER: 16-bit unsigned size, always bottom-up, 3-byte palette entries.
        w = le16(data, 18)?;
        h = le16(data, 20)?;
        top_down = false;
        bpp = le16(data, 24)?;
        compression = BI_RGB;
        colors_used = 0;
        entry_size = 3;
    } else if windows_header || os2_v2_header {
        // BITMAPINFOHEADER and later (OS/2 2.x headers may be cut short; missing
        // fields are zero). A negative height means top-down rows.
        let field = |at: usize| if at + 4 <= 14 + hsize { le32(data, at) } else { Some(0) };
        let wi = field(18)? as i32;
        let hi = field(22)? as i32;
        if wi <= 0 || hi == 0 {
            return None;
        }
        w = wi as u32;
        h = hi.unsigned_abs();
        top_down = hi < 0;
        bpp = le16(data, 28)?;
        compression = field(30)?;
        colors_used = field(46)? as usize;
        entry_size = 4;
        // In OS/2 2.x headers, compression 3 and 4 mean Huffman 1D and RLE24.
        if os2_v2_header && compression > BI_RLE4 {
            return None;
        }
    } else {
        return None;
    }
    if w == 0 || h == 0 || w as u64 * h as u64 > MAX_PIXELS {
        return None;
    }

    // Masks. With a plain 40-byte header the BI_BITFIELDS masks follow it, which puts
    // them at the same offsets as inside the larger headers.
    let mut masks = [0u32; 4];
    let mut after_header = 14 + hsize;
    match (compression, bpp) {
        (BI_BITFIELDS | BI_ALPHABITFIELDS, 16 | 32) => {
            masks = [le32(data, 54)?, le32(data, 58)?, le32(data, 62)?, 0];
            if hsize >= 56 || compression == BI_ALPHABITFIELDS {
                masks[3] = le32(data, 66)?;
            }
            if hsize == 40 {
                after_header += if compression == BI_ALPHABITFIELDS { 16 } else { 12 };
            }
        }
        (BI_RGB, 16) => masks = [0x7C00, 0x03E0, 0x001F, 0],
        (BI_RGB, 24) => masks = [0xFF_0000, 0xFF00, 0xFF, 0],
        (BI_RGB, 32) => {
            masks = [0xFF_0000, 0xFF00, 0xFF, 0];
            // V4/V5 headers may declare an alpha channel even without BI_BITFIELDS.
            if hsize >= 108 {
                masks[3] = le32(data, 66)?;
            }
        }
        (BI_RGB, 1 | 2 | 4 | 8) | (BI_RLE8, 8) | (BI_RLE4, 4) => {}
        _ => return None, // other depths, BI_JPEG, BI_PNG, ...
    }

    // Palette for 1-8 bit images: `colors_used` entries (0 = all 2^bpp), never more
    // than fit before the pixel data.
    let mut palette = Vec::new();
    if bpp <= 8 {
        let mut count = if colors_used == 0 || colors_used > 256 { 1 << bpp } else { colors_used };
        if off_bits > after_header {
            count = count.min((off_bits - after_header) / entry_size);
        }
        for i in 0..count {
            let Some(e) = data.get(after_header + i * entry_size..after_header + i * entry_size + 3) else { break };
            palette.push(0xFF00_0000 | (e[2] as u32) << 16 | (e[1] as u32) << 8 | e[0] as u32);
        }
    }

    // Pixel data offset; a missing or impossible value falls back to "right after the palette".
    let data_offset = if off_bits >= after_header && off_bits < data.len() {
        off_bits
    } else {
        after_header + palette.len() * entry_size
    };
    Some(Header { w, h, top_down, bpp, compression, data_offset, palette, masks })
}

/// Width and height from the header, without decoding.
pub fn dimensions(data: &[u8]) -> Option<(u32, u32)> {
    let h = parse(data)?;
    Some((h.w, h.h))
}

/// Decodes the whole image to premultiplied BGRA.
pub fn decode(data: &[u8]) -> Option<Decoded> {
    let hdr = parse(data)?;
    let pixels = data.get(hdr.data_offset..)?;
    let px = match hdr.compression {
        BI_RLE8 | BI_RLE4 => decode_rle(&hdr, pixels)?,
        _ => decode_plain(&hdr, pixels)?,
    };
    Some(Decoded { w: hdr.w, h: hdr.h, px })
}

/// One color channel of a 16/32-bit pixel, described by its mask.
struct Channel {
    mask: u32,
    shift: u32,
    /// Width of the mask in bits (0 = channel absent).
    bits: u32,
    /// For channels narrower than 8 bits: the 8-bit value of each raw value.
    scale: [u8; 256],
}

impl Channel {
    fn new(mask: u32) -> Channel {
        let mut c = Channel { mask, shift: 0, bits: 0, scale: [0; 256] };
        if mask == 0 {
            return c;
        }
        c.shift = mask.trailing_zeros();
        c.bits = 32 - (mask >> c.shift).leading_zeros();
        if c.bits < 8 {
            // Widen by repeating the bit pattern (5 bits abcde -> abcdeabc).
            let b = c.bits as i32;
            for v in 0..(1u32 << b) {
                let mut out = 0u32;
                let mut s = 8 - b;
                while s > -b {
                    out |= if s >= 0 { v << s } else { v >> -s };
                    s -= b;
                }
                c.scale[v as usize] = out as u8;
            }
        }
        c
    }

    #[inline]
    fn get(&self, px: u32) -> u8 {
        let v = (px & self.mask) >> self.shift;
        match self.bits {
            0 => 0,
            1..=7 => self.scale[(v & 0xFF) as usize],
            b => (v >> (b - 8)) as u8,
        }
    }
}

/// Uncompressed rows: palette, 16-, 24- and 32-bit.
fn decode_plain(hdr: &Header, pixels: &[u8]) -> Option<Vec<u32>> {
    let (w, h) = (hdr.w as usize, hdr.h as usize);
    let row_bytes = (w as u64 * hdr.bpp as u64).div_ceil(8);
    let stride = (w as u64 * hdr.bpp as u64).div_ceil(32) * 4;
    // All rows must be present (the last one may lack its padding).
    let needed = stride.checked_mul(h as u64 - 1)?.checked_add(row_bytes)?;
    if needed > pixels.len() as u64 {
        return None;
    }
    let (row_bytes, stride) = (row_bytes as usize, stride as usize);

    let [r, g, b, a] = hdr.masks.map(Channel::new);
    let has_alpha = a.bits > 0;
    let mut px = vec![0u32; w * h];
    for (y, out) in px.chunks_exact_mut(w).enumerate() {
        let file_row = if hdr.top_down { y } else { h - 1 - y };
        let src = pixels.get(file_row * stride..file_row * stride + row_bytes)?;
        match hdr.bpp {
            1 | 2 | 4 | 8 => {
                // Indexes, most significant bits first; unknown indexes are black.
                let bits = hdr.bpp as usize;
                let per_byte = 8 / bits;
                let mask = (1u32 << bits) - 1;
                for (x, o) in out.iter_mut().enumerate() {
                    let byte = src.get(x / per_byte).copied().unwrap_or(0) as u32;
                    let index = (byte >> (8 - bits - (x % per_byte) * bits)) & mask;
                    *o = hdr.palette.get(index as usize).copied().unwrap_or(0xFF00_0000);
                }
            }
            24 => {
                for (o, s) in out.iter_mut().zip(src.chunks_exact(3)) {
                    *o = 0xFF00_0000 | (s[2] as u32) << 16 | (s[1] as u32) << 8 | s[0] as u32;
                }
            }
            16 | 32 => {
                // Straight (not yet premultiplied) alpha; fixed up below.
                let size = hdr.bpp as usize / 8;
                for (o, s) in out.iter_mut().zip(src.chunks_exact(size)) {
                    let v = if size == 2 {
                        u16::from_le_bytes([s[0], s[1]]) as u32
                    } else {
                        u32::from_le_bytes([s[0], s[1], s[2], s[3]])
                    };
                    let alpha = if has_alpha { a.get(v) } else { 255 };
                    *o = (alpha as u32) << 24 | (r.get(v) as u32) << 16 | (g.get(v) as u32) << 8 | b.get(v) as u32;
                }
            }
            _ => return None,
        }
    }

    if has_alpha {
        if px.iter().all(|p| p >> 24 == 0) {
            // An alpha channel that is 0 everywhere is unused: show the image opaque.
            for p in px.iter_mut() {
                *p |= 0xFF00_0000;
            }
        } else {
            for p in px.iter_mut() {
                *p = premultiply(*p);
            }
        }
    }
    Some(px)
}

/// Premultiplies a packed straight-alpha BGRA pixel (rounding c * a / 255).
fn premultiply(p: u32) -> u32 {
    let a = p >> 24;
    match a {
        255 => p,
        0 => 0,
        _ => {
            let m = |c: u32| {
                let t = c * a + 128;
                (t + (t >> 8)) >> 8
            };
            a << 24 | m((p >> 16) & 255) << 16 | m((p >> 8) & 255) << 8 | m(p & 255)
        }
    }
}

/// RLE8 / RLE4 data. Rows are stored bottom-up (top-down if the height is negative).
fn decode_rle(hdr: &Header, data: &[u8]) -> Option<Vec<u32>> {
    let (w, h) = (hdr.w as usize, hdr.h as usize);
    let rle4 = hdr.compression == BI_RLE4;
    let color = |index: u8| hdr.palette.get(index as usize).copied().unwrap_or(0xFF00_0000);
    let mut px = vec![0u32; w * h];
    // Position: x in pixels, y in stored row order.
    let (mut x, mut y) = (0usize, 0usize);
    let mut put = |x: usize, y: usize, index: u8| {
        if x < w && y < h {
            let row = if hdr.top_down { y } else { h - 1 - y };
            px[row * w + x] = color(index);
        }
    };
    let mut pos = 0usize;
    while y < h {
        // Data that ends before the end-of-bitmap marker is truncated.
        let (&count, &value) = (data.get(pos)?, data.get(pos + 1)?);
        pos += 2;
        if count > 0 {
            // A run of `count` pixels: one index, or two alternating 4-bit indexes.
            for i in 0..count as usize {
                let index = if !rle4 {
                    value
                } else if i % 2 == 0 {
                    value >> 4
                } else {
                    value & 15
                };
                put(x, y, index);
                x += 1;
            }
            continue;
        }
        match value {
            0 => {
                // End of line.
                x = 0;
                y += 1;
            }
            1 => break, // end of bitmap
            2 => {
                // Delta: move right and up (in stored order); skipped pixels stay transparent.
                let (&dx, &dy) = (data.get(pos)?, data.get(pos + 1)?);
                pos += 2;
                x += dx as usize;
                y += dy as usize;
            }
            n => {
                // Absolute mode: `n` literal indexes, padded to a 16-bit boundary.
                let n = n as usize;
                let bytes = if rle4 { n.div_ceil(2) } else { n };
                let src = data.get(pos..pos + bytes)?;
                for i in 0..n {
                    let index = if !rle4 {
                        src[i]
                    } else if i % 2 == 0 {
                        src[i / 2] >> 4
                    } else {
                        src[i / 2] & 15
                    };
                    put(x, y, index);
                    x += 1;
                }
                pos += bytes + (bytes & 1);
            }
        }
    }
    Some(px)
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{self as tu, Opt};
    use super::*;
    use windows::core::GUID;
    use windows::Win32::Graphics::Imaging::*;

    // ------------------------------------------------------------ hand-made BMP writer

    /// RLE operations for hand-made RLE8/RLE4 streams.
    #[derive(Clone)]
    enum Op {
        Run(u8, u8),
        Abs(Vec<u8>),
        Eol,
        Delta(u8, u8),
        Eob,
    }

    #[derive(Clone)]
    struct Bmp {
        hsize: u32,
        w: i32,
        /// Negative for top-down.
        h: i32,
        bpp: u16,
        compression: u32,
        masks: [u32; 4],
        palette: Vec<[u8; 3]>,
        clr_used: u32,
        /// Raw values in display order: palette index, 16/32-bit word, or 0xRRGGBB.
        values: Vec<u32>,
        rle: Vec<Op>,
    }

    fn rle_bytes(ops: &[Op], rle4: bool) -> Vec<u8> {
        let mut out = Vec::new();
        for op in ops {
            match op {
                Op::Run(n, v) => out.extend_from_slice(&[*n, *v]),
                Op::Abs(ix) => {
                    out.extend_from_slice(&[0, ix.len() as u8]);
                    let bytes: Vec<u8> = if rle4 {
                        ix.chunks(2).map(|c| c[0] << 4 | c.get(1).copied().unwrap_or(0)).collect()
                    } else {
                        ix.clone()
                    };
                    out.extend_from_slice(&bytes);
                    if bytes.len() % 2 == 1 {
                        out.push(0);
                    }
                }
                Op::Eol => out.extend_from_slice(&[0, 0]),
                Op::Delta(dx, dy) => out.extend_from_slice(&[0, 2, *dx, *dy]),
                Op::Eob => out.extend_from_slice(&[0, 1]),
            }
        }
        out
    }

    fn write_bmp(b: &Bmp) -> Vec<u8> {
        let mut head = Vec::new();
        let entry = if b.hsize == 12 { 3 } else { 4 };
        if b.hsize == 12 {
            head.extend_from_slice(&12u32.to_le_bytes());
            head.extend_from_slice(&(b.w as u16).to_le_bytes());
            head.extend_from_slice(&(b.h as u16).to_le_bytes());
            head.extend_from_slice(&1u16.to_le_bytes());
            head.extend_from_slice(&b.bpp.to_le_bytes());
        } else {
            for v in [b.hsize, b.w as u32, b.h as u32] {
                head.extend_from_slice(&v.to_le_bytes());
            }
            head.extend_from_slice(&1u16.to_le_bytes());
            head.extend_from_slice(&b.bpp.to_le_bytes());
            for v in [b.compression, 0, 2835, 2835, b.clr_used, 0] {
                head.extend_from_slice(&v.to_le_bytes());
            }
            if b.hsize >= 52 {
                for m in &b.masks[..3] {
                    head.extend_from_slice(&m.to_le_bytes());
                }
            }
            if b.hsize >= 56 {
                head.extend_from_slice(&b.masks[3].to_le_bytes());
            }
            if b.hsize >= 108 {
                head.extend_from_slice(&0x7352_4742u32.to_le_bytes()); // LCS_sRGB
            }
            head.resize(b.hsize as usize, 0);
            if b.hsize == 40 && b.compression == BI_BITFIELDS {
                for m in &b.masks[..3] {
                    head.extend_from_slice(&m.to_le_bytes());
                }
            }
            if b.hsize == 40 && b.compression == BI_ALPHABITFIELDS {
                for m in &b.masks {
                    head.extend_from_slice(&m.to_le_bytes());
                }
            }
        }
        for c in &b.palette {
            head.extend_from_slice(&[c[2], c[1], c[0]]);
            if entry == 4 {
                head.push(0);
            }
        }
        let pixels = if b.compression == BI_RLE8 || b.compression == BI_RLE4 {
            rle_bytes(&b.rle, b.compression == BI_RLE4)
        } else {
            let (w, h) = (b.w as usize, b.h.unsigned_abs() as usize);
            let bpp = b.bpp as usize;
            let stride = (w * bpp + 31) / 32 * 4;
            let mut out = vec![0u8; stride * h];
            for y in 0..h {
                let file_row = if b.h > 0 { h - 1 - y } else { y };
                let row = &mut out[file_row * stride..file_row * stride + stride];
                for x in 0..w {
                    let v = b.values[y * w + x];
                    match bpp {
                        1 | 2 | 4 => row[x * bpp / 8] |= (v as u8) << (8 - bpp - (x * bpp) % 8),
                        8 => row[x] = v as u8,
                        16 => row[x * 2..x * 2 + 2].copy_from_slice(&(v as u16).to_le_bytes()),
                        24 => row[x * 3..x * 3 + 3].copy_from_slice(&[v as u8, (v >> 8) as u8, (v >> 16) as u8]),
                        _ => row[x * 4..x * 4 + 4].copy_from_slice(&v.to_le_bytes()),
                    }
                }
            }
            out
        };
        let off = 14 + head.len() as u32;
        let mut file = b"BM".to_vec();
        file.extend_from_slice(&(off + pixels.len() as u32).to_le_bytes());
        file.extend_from_slice(&[0, 0, 0, 0]);
        file.extend_from_slice(&off.to_le_bytes());
        file.extend_from_slice(&head);
        file.extend_from_slice(&pixels);
        file
    }

    /// Reference result for `b` (straightforward per-pixel rules, rounding to nearest
    /// for narrow mask channels, so those may differ from the decoder by 1).
    fn expected(b: &Bmp) -> Vec<u32> {
        let (w, h) = (b.w as usize, b.h.unsigned_abs() as usize);
        let pal = |i: u32| {
            b.palette
                .get(i as usize)
                .map_or(0xFF00_0000, |c| 0xFF00_0000 | (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32)
        };
        if b.compression == BI_RLE8 || b.compression == BI_RLE4 {
            let mut px = vec![0u32; w * h];
            let (mut x, mut y) = (0usize, 0usize);
            let mut put = |x: usize, y: usize, i: u8| {
                if x < w && y < h {
                    px[(h - 1 - y) * w + x] = pal(i as u32);
                }
            };
            for op in &b.rle {
                match op {
                    Op::Run(n, v) => {
                        for i in 0..*n as usize {
                            let ix = if b.compression == BI_RLE8 {
                                *v
                            } else if i % 2 == 0 {
                                v >> 4
                            } else {
                                v & 15
                            };
                            put(x, y, ix);
                            x += 1;
                        }
                    }
                    Op::Abs(ix) => {
                        for &i in ix {
                            put(x, y, i);
                            x += 1;
                        }
                    }
                    Op::Eol => {
                        x = 0;
                        y += 1;
                    }
                    Op::Delta(dx, dy) => {
                        x += *dx as usize;
                        y += *dy as usize;
                    }
                    Op::Eob => break,
                }
            }
            return px;
        }
        let (masks, alpha_used) = match (b.compression, b.bpp) {
            (BI_RGB, 16) => ([0x7C00, 0x3E0, 0x1F, 0], false),
            (BI_RGB, 32) => {
                let a = if b.hsize >= 108 { b.masks[3] } else { 0 };
                ([0xFF_0000, 0xFF00, 0xFF, a], a != 0)
            }
            (BI_BITFIELDS, _) => {
                let a = if b.hsize >= 56 { b.masks[3] } else { 0 };
                ([b.masks[0], b.masks[1], b.masks[2], a], a != 0)
            }
            (BI_ALPHABITFIELDS, _) => (b.masks, b.masks[3] != 0),
            _ => ([0; 4], false),
        };
        let chan = |v: u32, m: u32| -> u32 {
            if m == 0 {
                return 0;
            }
            let raw = (v & m) >> m.trailing_zeros();
            let max = m >> m.trailing_zeros();
            if max >= 255 {
                raw >> (32 - max.leading_zeros() - 8)
            } else {
                (raw as f64 * 255.0 / max as f64).round() as u32
            }
        };
        let mut px: Vec<u32> = b
            .values
            .iter()
            .map(|&v| match b.bpp {
                1 | 2 | 4 | 8 => pal(v),
                24 => 0xFF00_0000 | v,
                _ => {
                    let a = if alpha_used { chan(v, masks[3]) } else { 255 };
                    a << 24 | chan(v, masks[0]) << 16 | chan(v, masks[1]) << 8 | chan(v, masks[2])
                }
            })
            .collect();
        if alpha_used {
            if px.iter().all(|p| p >> 24 == 0) {
                px.iter_mut().for_each(|p| *p |= 0xFF00_0000);
            } else {
                for p in px.iter_mut() {
                    let a = *p >> 24;
                    let m = |c: u32| ((c as f64) * (a as f64) / 255.0).round() as u32;
                    *p = a << 24 | m((*p >> 16) & 255) << 16 | m((*p >> 8) & 255) << 8 | m(*p & 255);
                }
            }
        }
        px
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

    /// RLE ops for a w x h image: runs, odd/even absolute runs, early end-of-line,
    /// a delta jump, and an end-of-bitmap that leaves the top rows unset.
    fn rle_ops(w: usize, h: usize, rle4: bool, seed: u64) -> Vec<Op> {
        let mut rng = tu::Rng(seed);
        let max = if rle4 { 16 } else { 256 };
        let mut ops = Vec::new();
        let mut y = 0;
        while y < h - 2 {
            let mut x = 0;
            if y == 3 {
                ops.push(Op::Delta(5, 2)); // skip right 5 and up two rows
                y += 2;
                x = 5;
            }
            while x < w {
                let left = w - x;
                if y % 5 == 4 && x > w / 2 {
                    break; // end the line early: rest of the row is skipped
                }
                if rng.below(2) == 0 || left < 3 {
                    let n = (1 + rng.below(left.min(40))) as u8;
                    let v = rng.below(256) as u8;
                    ops.push(Op::Run(n, v));
                    x += n as usize;
                } else {
                    let n = 3 + rng.below((left - 2).min(30));
                    ops.push(Op::Abs((0..n).map(|_| rng.below(max) as u8).collect()));
                    x += n;
                }
            }
            ops.push(Op::Eol);
            y += 1;
        }
        ops.push(Op::Eob);
        ops
    }

    /// Hand-made BMPs for all header kinds, depths and compressions.
    fn hand_made() -> Vec<(String, Bmp)> {
        let mut out = Vec::new();
        let mut rng = tu::Rng(77);
        let (w, h) = (23i32, 13i32);
        let n = (w * h) as usize;
        let base = Bmp {
            hsize: 40,
            w,
            h,
            bpp: 8,
            compression: BI_RGB,
            masks: [0; 4],
            palette: vec![],
            clr_used: 0,
            values: vec![],
            rle: vec![],
        };
        // Palette images with every header kind, both row orders.
        for &hsize in &[12u32, 40, 52, 56, 108, 124, 64] {
            for &bpp in &[1u16, 2, 4, 8] {
                for top_down in [false, true] {
                    if top_down && (hsize == 12 || hsize == 64) {
                        continue;
                    }
                    let colors_n = 1usize << bpp;
                    let mut b = base.clone();
                    b.hsize = hsize;
                    b.bpp = bpp;
                    b.h = if top_down { -h } else { h };
                    b.palette = colors(colors_n, bpp as u64 + hsize as u64);
                    b.values = (0..n).map(|_| rng.below(colors_n) as u32).collect();
                    out.push((format!("h{}_{}bpp{}", hsize, bpp, if top_down { "_topdown" } else { "" }), b));
                }
            }
        }
        // A short palette (clrUsed) with indexes past its end.
        let mut b = base.clone();
        b.palette = colors(40, 1);
        b.clr_used = 40;
        b.values = (0..n).map(|_| rng.below(60) as u32).collect();
        out.push(("8bpp_short_palette".into(), b));
        // 24-bit, both orders, core and info headers.
        for (hsize, hh) in [(40, h), (40, -h), (12, h), (124, h)] {
            let mut b = base.clone();
            b.hsize = hsize;
            b.h = hh;
            b.bpp = 24;
            b.values = (0..n).map(|_| rng.next() as u32 & 0xFF_FFFF).collect();
            out.push((format!("h{}_24bpp{}", hsize, if hh < 0 { "_topdown" } else { "" }), b));
        }
        // 16-bit: default 5-5-5, BI_BITFIELDS 5-6-5 (40-byte header and V5), 4-4-4-4 with alpha (V5).
        let words16 = |rng: &mut tu::Rng| (0..n).map(|_| rng.next() as u32 & 0xFFFF).collect::<Vec<u32>>();
        let mut b = base.clone();
        b.bpp = 16;
        b.values = words16(&mut rng);
        out.push(("16bpp_555".into(), b.clone()));
        b.h = -h;
        out.push(("16bpp_555_topdown".into(), b.clone()));
        b.h = h;
        b.compression = BI_BITFIELDS;
        b.masks = [0xF800, 0x07E0, 0x001F, 0];
        out.push(("16bpp_565_bitfields_h40".into(), b.clone()));
        b.hsize = 124;
        out.push(("16bpp_565_bitfields_v5".into(), b.clone()));
        b.masks = [0x0F00, 0x00F0, 0x000F, 0xF000];
        out.push(("16bpp_4444_alpha_v5".into(), b.clone()));
        b.masks = [0x7C00, 0x03E0, 0x001F, 0x8000];
        out.push(("16bpp_1555_alpha_v5".into(), b.clone()));
        // 32-bit.
        let mut b = base.clone();
        b.bpp = 32;
        b.values = (0..n)
            .map(|i| {
                let a = match i % 7 {
                    0 => 0,
                    1 => 255,
                    _ => rng.below(256) as u32,
                };
                a << 24 | (rng.next() as u32 & 0xFF_FFFF)
            })
            .collect();
        out.push(("32bpp_rgb_h40_alpha_ignored".into(), b.clone()));
        b.h = -h;
        out.push(("32bpp_rgb_h40_topdown".into(), b.clone()));
        b.h = h;
        b.hsize = 124;
        b.masks = [0, 0, 0, 0xFF00_0000];
        out.push(("32bpp_rgb_v5_alpha_mask".into(), b.clone()));
        b.masks = [0, 0, 0, 0];
        out.push(("32bpp_rgb_v5_no_alpha_mask".into(), b.clone()));
        b.compression = BI_BITFIELDS;
        b.masks = [0xFF_0000, 0xFF00, 0xFF, 0xFF00_0000];
        out.push(("32bpp_bitfields_v5_alpha".into(), b.clone()));
        b.hsize = 108;
        out.push(("32bpp_bitfields_v4_alpha".into(), b.clone()));
        b.hsize = 56;
        out.push(("32bpp_bitfields_v3_alpha".into(), b.clone()));
        b.hsize = 40;
        b.masks = [0xFF, 0xFF00, 0xFF_0000, 0];
        out.push(("32bpp_bitfields_h40_rgb_swapped".into(), b.clone()));
        b.masks = [0x3FF0_0000, 0x000F_FC00, 0x0000_03FF, 0]; // 10-10-10
        out.push(("32bpp_bitfields_h40_101010".into(), b.clone()));
        b.hsize = 124;
        b.masks = [0xFF_0000, 0xFF00, 0xFF, 0xFF00_0000];
        b.values.iter_mut().for_each(|v| *v &= 0xFF_FFFF); // alpha all zero -> opaque
        out.push(("32bpp_bitfields_v5_alpha_all_zero".into(), b.clone()));
        // RLE8 / RLE4 (bottom-up).
        for (bpp, comp) in [(8u16, BI_RLE8), (4, BI_RLE4)] {
            let mut b = base.clone();
            b.w = 37;
            b.h = 15;
            b.bpp = bpp;
            b.compression = comp;
            b.palette = colors(1 << bpp, bpp as u64);
            b.rle = rle_ops(37, 15, comp == BI_RLE4, bpp as u64 * 3);
            out.push((format!("rle{}", bpp), b));
        }
        out
    }

    fn compare_with_wic(name: &str, data: &[u8], tally: &mut tu::Tally) {
        let ours = decode(data);
        let wic = tu::wic_decode(data);
        match (ours, wic) {
            (Some(o), Ok(w)) if (o.w, o.h) == (w.w, w.h) => {
                let d = tu::compare(&o.px, &w.px, o.w, o.h);
                tally.add_diff(name, &d, &format!("WIC native {}", tu::format_name(&w.native)));
            }
            (Some(o), Ok(w)) => tally.size_mismatch.push(format!("{}: ours {}x{} WIC {}x{}", name, o.w, o.h, w.w, w.h)),
            (None, Ok(w)) => tally.ours_failed.push(format!("{} [WIC native {}]", name, tu::format_name(&w.native))),
            (Some(_), Err(e)) => tally.wic_failed.push(format!("{}: {}", name, e)),
            (None, Err(e)) => tally.wic_failed.push(format!("{}: both failed ({})", name, e)),
        }
    }

    #[test]
    fn hand_made_vs_expected_and_wic() {
        let mut tally = tu::Tally::default();
        let dir = tu::testdata_dir("bmp_hand");
        for (name, b) in hand_made() {
            let file = write_bmp(&b);
            let _ = std::fs::write(dir.join(format!("{}.bmp", name)), &file);
            let ours = decode(&file).unwrap_or_else(|| panic!("{}: decode failed", name));
            let (w, h) = (b.w as u32, b.h.unsigned_abs());
            assert_eq!((ours.w, ours.h), (w, h), "{}", name);
            assert_eq!(dimensions(&file), Some((w, h)), "{}", name);
            let d = tu::compare(&ours.px, &expected(&b), w, h);
            // Exact, except rounding of narrow (< 8 bit) mask channels.
            let narrow = b.bpp == 16 || name.contains("101010");
            assert!(d.max == 0 || (narrow && d.max <= 1), "{}: differs from expected: {:?}", name, d);
            compare_with_wic(&name, &file, &mut tally);
        }
        tally.print("hand-made BMPs vs WIC");
        // Known, deliberate differences from WIC (see the module notes):
        // - WIC ignores alpha masks in 16-bit and V3 bitfields and in V5 BI_RGB files,
        // - WIC shows an all-zero alpha channel as transparent (we show it opaque),
        // - WIC paints pixels skipped by RLE escapes black (we leave them transparent).
        let known_differ = [
            "16bpp_4444_alpha_v5",
            "16bpp_1555_alpha_v5",
            "32bpp_rgb_v5_alpha_mask",
            "32bpp_bitfields_v3_alpha",
            "32bpp_bitfields_v5_alpha_all_zero",
            "rle8",
            "rle4",
        ];
        for d in &tally.differ {
            assert!(known_differ.iter().any(|k| d.starts_with(&format!("{}:", k))), "unexpected difference: {}", d);
        }
        // WIC cannot read 2-bit BMPs, 52-byte (V2) headers or OS/2 2.x (64-byte) headers.
        for f in &tally.wic_failed {
            assert!(
                f.contains("2bpp") || f.starts_with("h52_") || f.starts_with("h64_"),
                "unexpected WIC failure: {}",
                f
            );
        }
        assert!(tally.ours_failed.is_empty() && tally.size_mismatch.is_empty());
    }

    /// Writes BMP variants with WIC's encoder into `folder`; returns (name, path) of
    /// those produced and a list of variants that could not be produced.
    fn wic_variants(folder: &str) -> (Vec<(String, std::path::PathBuf)>, Vec<String>) {
        let dir = tu::testdata_dir(folder);
        let (w, h) = (67u32, 45u32);
        let formats: [(&str, GUID, u32, usize); 9] = [
            ("1bppIndexed", GUID_WICPixelFormat1bppIndexed, 1, 2),
            ("2bppIndexed", GUID_WICPixelFormat2bppIndexed, 2, 4),
            ("4bppIndexed", GUID_WICPixelFormat4bppIndexed, 4, 16),
            ("8bppIndexed", GUID_WICPixelFormat8bppIndexed, 8, 256),
            ("16bppBGR555", GUID_WICPixelFormat16bppBGR555, 16, 0),
            ("16bppBGR565", GUID_WICPixelFormat16bppBGR565, 16, 0),
            ("24bppBGR", GUID_WICPixelFormat24bppBGR, 24, 0),
            ("32bppBGR", GUID_WICPixelFormat32bppBGR, 32, 0),
            ("32bppBGRA", GUID_WICPixelFormat32bppBGRA, 32, 0),
        ];
        let (mut made, mut failed) = (Vec::new(), Vec::new());
        for (i, (name, fmt, bits, pal_n)) in formats.iter().enumerate() {
            let stride = (w * bits).div_ceil(32) * 4;
            let mut rng = tu::Rng(i as u64 + 11);
            let pixels: Vec<u8> =
                (0..stride * h).map(|k| ((k % stride) as u8).wrapping_mul(3) ^ (rng.next() as u8 & 0x3F)).collect();
            let pal: Option<Vec<u32>> = (*pal_n > 0).then(|| {
                colors(*pal_n, i as u64)
                    .iter()
                    .map(|c| 0xFF00_0000 | (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32)
                    .collect()
            });
            let mut variants: Vec<(String, Vec<(&str, Opt)>)> = vec![(name.to_string(), vec![])];
            if *name == "32bppBGRA" {
                variants.push((format!("{}_v5", name), vec![("EnableV5Header32bppBGRA", Opt::Bool(true))]));
            }
            for (vname, opts) in variants {
                let path = dir.join(format!("{}.bmp", vname));
                match tu::wic_encode(&path, &GUID_ContainerFormatBmp, fmt, w, h, stride, &pixels, pal.as_deref(), &opts)
                {
                    Ok(()) => made.push((vname, path)),
                    Err(e) => failed.push(format!("{}: {}", vname, e)),
                }
            }
        }
        (made, failed)
    }

    #[test]
    fn wic_encoded_vs_wic() {
        let (made, failed) = wic_variants("bmp_wic");
        for f in &failed {
            println!("could not produce BMP {}", f);
        }
        let mut tally = tu::Tally::default();
        for (vname, path) in &made {
            let data = std::fs::read(path).unwrap();
            let note = parse(&data).map_or("unparsed".into(), |p| {
                format!("bpp {} compression {} masks {:08X?}", p.bpp, p.compression, p.masks)
            });
            compare_with_wic(&format!("{} [{}]", vname, note), &data, &mut tally);
        }
        tally.print("WIC-encoded BMPs");
        assert!(tally.differ.is_empty() && tally.wic_failed.is_empty());
        assert!(tally.ours_failed.is_empty() && tally.size_mismatch.is_empty());
    }

    #[test]
    fn windows_samples_vs_wic() {
        // reads samples from all over C:\Windows: run with DVFILES_SHELLTEST=1
        if std::env::var_os("DVFILES_SHELLTEST").is_none() {
            return;
        }
        let mut tally = tu::Tally::default();
        let files = &tu::samples().bmp;
        let mut not_bmp = 0;
        for p in files {
            let Ok(data) = std::fs::read(p) else { continue };
            if !data.starts_with(b"BM") {
                // e.g. WinSxS delta-compressed files ("DCS" signature) named *.bmp.
                println!("not a BMP file (starts with {:02X?}): {}", &data[..data.len().min(4)], tu::short(p));
                assert!(decode(&data).is_none());
                not_bmp += 1;
                continue;
            }
            let note = parse(&data).map_or("unparsed".into(), |h| {
                format!(
                    "hdr {} bpp {} comp {}{}",
                    le32(&data, 14).unwrap_or(0),
                    h.bpp,
                    h.compression,
                    if h.top_down { " top-down" } else { "" }
                )
            });
            compare_with_wic(&format!("{} [{}]", tu::short(p), note), &data, &mut tally);
        }
        println!("Windows BMP samples: {} unique files ({} of them not BMP data)", files.len(), not_bmp);
        tally.print("Windows BMP samples");
        assert!(tally.differ.is_empty() && tally.wic_failed.is_empty());
        assert!(tally.ours_failed.is_empty() && tally.size_mismatch.is_empty());
    }

    #[test]
    fn errors_and_limits() {
        let (_, b) = hand_made().into_iter().find(|(n, _)| n == "h40_24bpp").unwrap();
        let good = write_bmp(&b);
        assert!(decode(&good).is_some());
        let patch = |at: usize, bytes: &[u8]| {
            let mut f = good.clone();
            f[at..at + bytes.len()].copy_from_slice(bytes);
            f
        };
        assert!(decode(&patch(0, b"XX")).is_none(), "signature");
        assert!(decode(&patch(14, &[200, 0, 0, 0])).is_none(), "unknown header size");
        assert!(decode(&patch(14, &[11, 0, 0, 0])).is_none(), "unknown header size");
        assert!(decode(&patch(18, &[0, 0, 0, 0])).is_none(), "zero width");
        assert!(decode(&patch(22, &[0, 0, 0, 0x80])).is_none(), "height i32::MIN");
        assert!(decode(&patch(18, &[0x10, 0x27, 0, 0, 0x10, 0x27, 0, 0])).is_none(), "100 MP but no data");
        assert!(dimensions(&patch(18, &[0x10, 0x27, 0, 0, 0x10, 0x27, 0, 0])) == Some((10000, 10000)));
        assert!(dimensions(&patch(18, &[0x20, 0x4E, 0, 0, 0x20, 0x4E, 0, 0])).is_none(), "400 MP");
        assert!(decode(&patch(28, &[64, 0])).is_none(), "64 bpp");
        assert!(decode(&patch(30, &[4, 0, 0, 0])).is_none(), "BI_JPEG");
        assert!(decode(&patch(30, &[5, 0, 0, 0])).is_none(), "BI_PNG");
        assert!(decode(&good[..good.len() - 10]).is_none(), "truncated rows");
        // Truncated RLE (no end-of-bitmap) fails; skipped pixels are transparent.
        let (_, rle) = hand_made().into_iter().find(|(n, _)| n == "rle8").unwrap();
        let file = write_bmp(&rle);
        assert!(decode(&file[..file.len() - 40]).is_none());
        let img = decode(&file).unwrap();
        assert_eq!(img.px[0], 0, "top rows are never written by the RLE stream");
    }

    #[test]
    #[cfg_attr(not(debug_assertions), ignore = "fuzz: run in a debug build")]
    fn fuzz_no_panic() {
        let mut files: Vec<(String, Vec<u8>)> = hand_made().into_iter().map(|(n, b)| (n, write_bmp(&b))).collect();
        for (n, p) in wic_variants("bmp_wic_fuzz").0 {
            files.push((n, std::fs::read(p).unwrap()));
        }
        for p in &tu::samples().bmp {
            if let Ok(d) = std::fs::read(p) {
                files.push((tu::short(p), d));
            }
        }
        let f = |d: &[u8]| {
            let _ = dimensions(d);
            let _ = decode(d);
        };
        let (n, cases, fails) = tu::fuzz_files(&files, &f);
        println!("BMP fuzz: {} files, {} cases, {} panics", n, cases, fails.len());
        assert!(fails.is_empty(), "{:#?}", fails);
    }
}
