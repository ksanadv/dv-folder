// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! PNG decoder using only `std`: color types 0/2/3/4/6, bit depths 1-16, Adam7
//! interlacing, all five filters, tRNS transparency. Gamma, ICC and other ancillary
//! chunks are ignored; chunk CRCs and the zlib Adler-32 are not checked (browsers and
//! WIC accept files where they are wrong); for APNG only the default image (the IDAT
//! data) is decoded. 16-bit samples keep their high byte.

use super::inflate::zlib_decompress_known_size;
use super::Decoded;

/// Largest image accepted, in pixels.
const MAX_PIXELS: u64 = 200_000_000;

const SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];

/// Adam7 passes: (x start, y start, x step, y step).
const ADAM7: [(u32, u32, u32, u32); 7] =
    [(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)];

/// The IHDR fields.
#[derive(Clone, Copy)]
struct Header {
    w: u32,
    h: u32,
    depth: u8,
    color: u8,
    interlaced: bool,
}

impl Header {
    fn channels(&self) -> u32 {
        match self.color {
            2 => 3,
            4 => 2,
            6 => 4,
            _ => 1, // gray and palette
        }
    }

    /// Bytes in one scanline of `w` pixels, without the filter byte.
    fn stride(&self, w: u32) -> u64 {
        (w as u64 * self.channels() as u64 * self.depth as u64).div_ceil(8)
    }

    /// Distance in bytes to the corresponding byte of the previous pixel (at least 1).
    fn filter_bpp(&self) -> usize {
        ((self.channels() * self.depth as u32 / 8) as usize).max(1)
    }
}

/// One pass of the image data: its size in pixels and where its pixels go.
#[derive(Clone, Copy)]
struct Pass {
    w: u32,
    h: u32,
    x0: u32,
    y0: u32,
    dx: u32,
    dy: u32,
}

fn be32(data: &[u8], at: usize) -> Option<u32> {
    let b = data.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// Reads and validates the signature and the IHDR chunk.
fn parse_header(data: &[u8]) -> Option<Header> {
    if data.get(..8)? != SIGNATURE || be32(data, 8)? != 13 || data.get(12..16)? != b"IHDR" {
        return None;
    }
    let w = be32(data, 16)?;
    let h = be32(data, 20)?;
    let f = data.get(24..29)?;
    let (depth, color, compression, filter, interlace) = (f[0], f[1], f[2], f[3], f[4]);
    if w == 0 || h == 0 || w > 0x7FFF_FFFF || h > 0x7FFF_FFFF {
        return None;
    }
    if w as u64 * h as u64 > MAX_PIXELS {
        return None;
    }
    let depth_ok = match color {
        0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
        3 => matches!(depth, 1 | 2 | 4 | 8),
        2 | 4 | 6 => matches!(depth, 8 | 16),
        _ => false,
    };
    if !depth_ok || compression != 0 || filter != 0 || interlace > 1 {
        return None;
    }
    Some(Header { w, h, depth, color, interlaced: interlace == 1 })
}

/// Width and height from the header, without decoding.
pub fn dimensions(data: &[u8]) -> Option<(u32, u32)> {
    let h = parse_header(data)?;
    Some((h.w, h.h))
}

/// The passes making up the image data (one for non-interlaced images; empty
/// Adam7 passes are left out because they contribute no bytes).
fn passes(hdr: &Header) -> Vec<Pass> {
    if !hdr.interlaced {
        return vec![Pass { w: hdr.w, h: hdr.h, x0: 0, y0: 0, dx: 1, dy: 1 }];
    }
    let count = |full: u32, start: u32, step: u32| if full > start { (full - start).div_ceil(step) } else { 0 };
    ADAM7
        .iter()
        .map(|&(x0, y0, dx, dy)| Pass { w: count(hdr.w, x0, dx), h: count(hdr.h, y0, dy), x0, y0, dx, dy })
        .filter(|p| p.w > 0 && p.h > 0)
        .collect()
}

/// Decodes the whole image to premultiplied BGRA.
pub fn decode(data: &[u8]) -> Option<Decoded> {
    let hdr = parse_header(data)?;

    // Walk the chunks, keeping PLTE, tRNS and the IDAT data. CRCs are not checked.
    let mut palette: &[u8] = &[];
    let mut trns: &[u8] = &[];
    let mut idat: Vec<u8> = Vec::new();
    let mut pos = 8usize;
    while let (Some(len), Some(kind)) = (be32(data, pos), data.get(pos + 4..pos + 8)) {
        let start = pos + 8;
        let end = start.checked_add(len as usize)?;
        let Some(body) = data.get(start..end) else {
            // Truncated chunk: keep any partial image data and try with what we have.
            if kind == b"IDAT" {
                idat.extend_from_slice(data.get(start..).unwrap_or(&[]));
            }
            break;
        };
        match kind {
            b"PLTE" => palette = body,
            b"tRNS" => trns = body,
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        pos = end.checked_add(4)?;
    }
    if idat.is_empty() {
        return None;
    }

    // Size of the filtered data the header implies; DEFLATE cannot expand more than
    // about 1032:1, so reject sizes the compressed data cannot possibly produce.
    let passes = passes(&hdr);
    let mut expected = 0u64;
    for p in &passes {
        expected = expected.checked_add(p.h as u64 * (1 + hdr.stride(p.w)))?;
    }
    if expected > (idat.len() as u64).saturating_mul(1032).saturating_add(1024) {
        return None;
    }
    let expected = usize::try_from(expected).ok()?;
    let mut raw = zlib_decompress_known_size(&idat, expected)?;
    drop(idat);
    if raw.len() != expected {
        return None;
    }

    let conv = Converter::new(&hdr, palette, trns)?;
    let w = hdr.w as usize;
    let mut px = vec![0u32; w * hdr.h as usize];
    let bpp = hdr.filter_bpp();
    let mut row_px = vec![0u32; if hdr.interlaced { w } else { 0 }];
    let mut off = 0usize;
    for p in &passes {
        let stride = hdr.stride(p.w) as usize;
        let row_len = stride + 1;
        // Destination rows of this pass: every dy-th row starting at y0.
        let mut dst_rows = px.chunks_exact_mut(w).skip(p.y0 as usize).step_by(p.dy as usize);
        for y in 0..p.h as usize {
            // Undo the filter in place, using the previous row of the same pass.
            let (done, rest) = raw.split_at_mut(off);
            let (filter, cur) = rest.get_mut(..row_len)?.split_first_mut()?;
            let prev = if y > 0 { done.get(off - row_len + 1..off) } else { None };
            unfilter(*filter, cur, prev, bpp)?;

            let dst = dst_rows.next()?;
            if hdr.interlaced {
                let tmp = row_px.get_mut(..p.w as usize)?;
                conv.row(cur, tmp);
                for (d, &s) in dst.iter_mut().skip(p.x0 as usize).step_by(p.dx as usize).zip(tmp.iter()) {
                    *d = s;
                }
            } else {
                conv.row(cur, dst);
            }
            off += row_len;
        }
    }
    Some(Decoded { w: hdr.w, h: hdr.h, px })
}

// ---------------------------------------------------------------- filters

/// Reverses one scanline filter in place. `prev` is the unfiltered previous row of
/// the same pass (`None` for the first row, which acts as a row of zeros).
fn unfilter(filter: u8, cur: &mut [u8], prev: Option<&[u8]>, bpp: usize) -> Option<()> {
    match bpp {
        1 => unfilter_n::<1>(filter, cur, prev),
        2 => unfilter_n::<2>(filter, cur, prev),
        3 => unfilter_n::<3>(filter, cur, prev),
        4 => unfilter_n::<4>(filter, cur, prev),
        6 => unfilter_n::<6>(filter, cur, prev),
        8 => unfilter_n::<8>(filter, cur, prev),
        _ => None,
    }
}

/// Filter reversal for N bytes per pixel (a constant, so the loops optimize well).
fn unfilter_n<const N: usize>(filter: u8, cur: &mut [u8], prev: Option<&[u8]>) -> Option<()> {
    match (filter, prev) {
        (0, _) | (2, None) => {}
        // Sub, and Paeth on the first row (the predictor is then always "left").
        (1, _) | (4, None) => {
            for i in N..cur.len() {
                cur[i] = cur[i].wrapping_add(cur[i - N]);
            }
        }
        (2, Some(up)) => {
            for (c, &u) in cur.iter_mut().zip(up) {
                *c = c.wrapping_add(u);
            }
        }
        (3, None) => {
            for i in N..cur.len() {
                cur[i] = cur[i].wrapping_add(cur[i - N] >> 1);
            }
        }
        (3, Some(up)) => {
            let mut left = [0u8; N];
            for (c, u) in cur.chunks_exact_mut(N).zip(up.chunks_exact(N)) {
                for k in 0..N {
                    // floor((a + b) / 2) without widening: common bits plus half the rest.
                    let (a, b) = (left[k], u[k]);
                    let v = c[k].wrapping_add((a & b) + ((a ^ b) >> 1));
                    c[k] = v;
                    left[k] = v;
                }
            }
        }
        (4, Some(up)) => {
            let mut left = [0u8; N];
            let mut up_left = [0u8; N];
            for (c, u) in cur.chunks_exact_mut(N).zip(up.chunks_exact(N)) {
                for k in 0..N {
                    let v = c[k].wrapping_add(paeth(left[k], u[k], up_left[k]));
                    c[k] = v;
                    left[k] = v;
                    up_left[k] = u[k];
                }
            }
        }
        _ => return None,
    }
    Some(())
}

/// The Paeth predictor: whichever of left, up, up-left is closest to left + up - up-left.
#[inline]
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (a16, b16, c16) = (a as i16, b as i16, c as i16);
    let pa = (b16 - c16).abs();
    let pb = (a16 - c16).abs();
    let pc = (a16 + b16 - 2 * c16).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

// ---------------------------------------------------------------- pixel conversion

/// Premultiplies and packs one pixel as BGRA in a u32.
#[inline]
fn premul(r: u8, g: u8, b: u8, a: u8) -> u32 {
    match a {
        255 => 0xFF00_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32,
        0 => 0,
        _ => {
            // Exact rounding of c * a / 255.
            let m = |c: u8| {
                let t = c as u32 * a as u32 + 128;
                (t + (t >> 8)) >> 8
            };
            (a as u32) << 24 | m(r) << 16 | m(g) << 8 | m(b)
        }
    }
}

/// Turns unfiltered scanlines into premultiplied BGRA pixels.
struct Converter {
    color: u8,
    depth: u8,
    /// For palette images and gray images up to 8 bits: color of each sample value.
    lut: Vec<u32>,
    /// tRNS for gray (one value) or RGB (three values), compared at full sample precision.
    key: Option<[u16; 3]>,
}

impl Converter {
    fn new(hdr: &Header, palette: &[u8], trns: &[u8]) -> Option<Converter> {
        let be16 = |i: usize| trns.get(i..i + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
        let mut c = Converter { color: hdr.color, depth: hdr.depth, lut: Vec::new(), key: None };
        match hdr.color {
            0 => {
                c.key = be16(0).map(|g| [g, g, g]);
                if hdr.depth <= 8 {
                    // Scale 1/2/4-bit gray up to 8 bits (0..max -> 0..255).
                    let max = (1u32 << hdr.depth) - 1;
                    c.lut = (0..256u32)
                        .map(|v| {
                            let g = (v.min(max) * 255 / max) as u8;
                            if c.key.map(|k| k[0] as u32) == Some(v) {
                                0
                            } else {
                                premul(g, g, g, 255)
                            }
                        })
                        .collect();
                }
            }
            2 => {
                if let (Some(r), Some(g), Some(b)) = (be16(0), be16(2), be16(4)) {
                    c.key = Some([r, g, b]);
                }
            }
            3 => {
                if palette.is_empty() {
                    return None;
                }
                // Indexes past the palette show as opaque black.
                c.lut = vec![0xFF00_0000; 256];
                for (i, rgb) in palette.chunks_exact(3).take(256).enumerate() {
                    let a = trns.get(i).copied().unwrap_or(255);
                    c.lut[i] = premul(rgb[0], rgb[1], rgb[2], a);
                }
            }
            _ => {}
        }
        Some(c)
    }

    /// Converts one unfiltered scanline into `dst.len()` pixels.
    fn row(&self, src: &[u8], dst: &mut [u32]) {
        let lut = &self.lut;
        let key = self.key;
        match (self.color, self.depth) {
            (0 | 3, 8) => {
                for (d, &v) in dst.iter_mut().zip(src) {
                    *d = lut[v as usize];
                }
            }
            (0 | 3, depth) if depth < 8 => {
                // Several samples per byte, most significant bits first.
                let depth = depth as usize;
                let per_byte = 8 / depth;
                let mask = (1u8 << depth) - 1;
                for (i, d) in dst.iter_mut().enumerate() {
                    let byte = src.get(i / per_byte).copied().unwrap_or(0);
                    let shift = 8 - depth - (i % per_byte) * depth;
                    *d = lut[((byte >> shift) & mask) as usize];
                }
            }
            (0, _) => {
                for (d, s) in dst.iter_mut().zip(src.chunks_exact(2)) {
                    let v = u16::from_be_bytes([s[0], s[1]]);
                    *d = if key.map(|k| k[0]) == Some(v) { 0 } else { premul(s[0], s[0], s[0], 255) };
                }
            }
            (2, 8) => match key {
                None => {
                    for (d, s) in dst.iter_mut().zip(src.chunks_exact(3)) {
                        *d = premul(s[0], s[1], s[2], 255);
                    }
                }
                Some(k) => {
                    for (d, s) in dst.iter_mut().zip(src.chunks_exact(3)) {
                        let hit = k == [s[0] as u16, s[1] as u16, s[2] as u16];
                        *d = if hit { 0 } else { premul(s[0], s[1], s[2], 255) };
                    }
                }
            },
            (2, _) => {
                for (d, s) in dst.iter_mut().zip(src.chunks_exact(6)) {
                    let rgb = [
                        u16::from_be_bytes([s[0], s[1]]),
                        u16::from_be_bytes([s[2], s[3]]),
                        u16::from_be_bytes([s[4], s[5]]),
                    ];
                    *d = if key == Some(rgb) { 0 } else { premul(s[0], s[2], s[4], 255) };
                }
            }
            (4, 8) => {
                for (d, s) in dst.iter_mut().zip(src.chunks_exact(2)) {
                    *d = premul(s[0], s[0], s[0], s[1]);
                }
            }
            (4, _) => {
                for (d, s) in dst.iter_mut().zip(src.chunks_exact(4)) {
                    *d = premul(s[0], s[0], s[0], s[2]);
                }
            }
            (6, 8) => {
                for (d, s) in dst.iter_mut().zip(src.chunks_exact(4)) {
                    *d = premul(s[0], s[1], s[2], s[3]);
                }
            }
            (6, _) => {
                for (d, s) in dst.iter_mut().zip(src.chunks_exact(8)) {
                    *d = premul(s[0], s[2], s[4], s[6]);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{self as tu, Opt};
    use super::*;
    use windows::core::GUID;
    use windows::Win32::Graphics::Imaging::*;

    // ------------------------------------------------------------ hand-made PNG writer

    /// A source image for the hand-made PNG writer: raw sample values per channel.
    struct Src {
        w: u32,
        h: u32,
        color: u8,
        depth: u8,
        samples: Vec<u16>,
        /// RGBA palette entries (color type 3).
        palette: Vec<[u8; 4]>,
        /// Number of palette alphas written to tRNS (may be fewer than the palette).
        palette_alpha: usize,
        /// tRNS color key for gray / RGB.
        key: Option<[u16; 3]>,
    }

    fn channels(color: u8) -> usize {
        match color {
            2 => 3,
            4 => 2,
            6 => 4,
            _ => 1,
        }
    }

    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        let mut c = kind.to_vec();
        c.extend_from_slice(body);
        out.extend_from_slice(&c);
        out.extend_from_slice(&tu::crc32(&c).to_be_bytes());
    }

    /// Reference Paeth predictor written straight from the PNG specification.
    fn paeth_ref(a: u8, b: u8, c: u8) -> u8 {
        let p = a as i32 + b as i32 - c as i32;
        let (pa, pb, pc) = ((p - a as i32).abs(), (p - b as i32).abs(), (p - c as i32).abs());
        if pa <= pb && pa <= pc {
            a
        } else if pb <= pc {
            b
        } else {
            c
        }
    }

    /// Writes a PNG with stored (uncompressed) zlib data, cycling through all five
    /// filter types row by row, split over several IDAT chunks.
    fn write_png(s: &Src, interlaced: bool) -> Vec<u8> {
        let ch = channels(s.color);
        let bits = ch * s.depth as usize;
        let bpp = (bits / 8).max(1);
        let pass_list: Vec<(u32, u32, u32, u32)> = if interlaced { ADAM7.to_vec() } else { vec![(0, 0, 1, 1)] };
        let mut raw = Vec::new();
        let mut row_no = 0usize;
        for (x0, y0, dx, dy) in pass_list {
            let xs: Vec<u32> = (x0..s.w).step_by(dx as usize).collect();
            let ys: Vec<u32> = (y0..s.h).step_by(dy as usize).collect();
            if xs.is_empty() || ys.is_empty() {
                continue;
            }
            let mut prev: Option<Vec<u8>> = None;
            for &y in &ys {
                // Pack the samples of this row.
                let mut line = vec![0u8; (xs.len() * bits + 7) / 8];
                let mut bit = 0usize;
                for &x in &xs {
                    for c in 0..ch {
                        let v = s.samples[(y as usize * s.w as usize + x as usize) * ch + c];
                        if s.depth == 16 {
                            line[bit / 8] = (v >> 8) as u8;
                            line[bit / 8 + 1] = v as u8;
                        } else {
                            let shift = 8 - s.depth as usize - bit % 8;
                            line[bit / 8] |= (v as u8) << shift;
                        }
                        bit += s.depth as usize;
                    }
                }
                // Filter it.
                let f = (row_no % 5) as u8;
                row_no += 1;
                raw.push(f);
                for i in 0..line.len() {
                    let a = if i >= bpp { line[i - bpp] } else { 0 };
                    let b = prev.as_ref().map_or(0, |p| p[i]);
                    let c = if i >= bpp { prev.as_ref().map_or(0, |p| p[i - bpp]) } else { 0 };
                    let pred = match f {
                        0 => 0,
                        1 => a,
                        2 => b,
                        3 => ((a as u16 + b as u16) / 2) as u8,
                        _ => paeth_ref(a, b, c),
                    };
                    raw.push(line[i].wrapping_sub(pred));
                }
                prev = Some(line);
            }
        }
        let mut out = SIGNATURE.to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&s.w.to_be_bytes());
        ihdr.extend_from_slice(&s.h.to_be_bytes());
        ihdr.extend_from_slice(&[s.depth, s.color, 0, 0, interlaced as u8]);
        chunk(&mut out, b"IHDR", &ihdr);
        if s.color == 3 {
            let plte: Vec<u8> = s.palette.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
            chunk(&mut out, b"PLTE", &plte);
            if s.palette_alpha > 0 {
                let a: Vec<u8> = s.palette.iter().take(s.palette_alpha).map(|p| p[3]).collect();
                chunk(&mut out, b"tRNS", &a);
            }
        }
        if let Some(k) = s.key {
            let n = if s.color == 0 { 1 } else { 3 };
            let t: Vec<u8> = k[..n].iter().flat_map(|v| v.to_be_bytes()).collect();
            chunk(&mut out, b"tRNS", &t);
        }
        for part in tu::zlib_stored(&raw).chunks(997) {
            chunk(&mut out, b"IDAT", part);
        }
        chunk(&mut out, b"IEND", &[]);
        out
    }

    /// What the decoder must produce for `s`, computed independently of the decoder.
    fn expected(s: &Src) -> Vec<u32> {
        let ch = channels(s.color);
        let max = (1u32 << s.depth) - 1;
        let to8 = |v: u16| -> u8 {
            if s.depth == 16 {
                (v >> 8) as u8
            } else {
                (v as u32 * 255 / max) as u8
            }
        };
        let pm = |r: u8, g: u8, b: u8, a: u8| -> u32 {
            let m = |c: u8| ((c as f64) * (a as f64) / 255.0).round() as u32;
            (a as u32) << 24 | m(r) << 16 | m(g) << 8 | m(b)
        };
        (0..(s.w * s.h) as usize)
            .map(|i| {
                let v = &s.samples[i * ch..i * ch + ch];
                match s.color {
                    0 if s.key.map(|k| k[0]) == Some(v[0]) => 0,
                    0 => pm(to8(v[0]), to8(v[0]), to8(v[0]), 255),
                    2 if s.key == Some([v[0], v[1], v[2]]) => 0,
                    2 => pm(to8(v[0]), to8(v[1]), to8(v[2]), 255),
                    3 => {
                        let p = s.palette.get(v[0] as usize).copied().unwrap_or([0, 0, 0, 255]);
                        let a = if (v[0] as usize) < s.palette_alpha { p[3] } else { 255 };
                        pm(p[0], p[1], p[2], a)
                    }
                    4 => pm(to8(v[0]), to8(v[0]), to8(v[0]), to8(v[1])),
                    _ => pm(to8(v[0]), to8(v[1]), to8(v[2]), to8(v[3])),
                }
            })
            .collect()
    }

    /// All valid color type / bit depth combinations, with and without tRNS.
    fn hand_made_sources() -> Vec<(String, Src)> {
        let combos: [(u8, &[u8]); 5] =
            [(0, &[1, 2, 4, 8, 16]), (2, &[8, 16]), (3, &[1, 2, 4, 8]), (4, &[8, 16]), (6, &[8, 16])];
        let sizes = [(1u32, 1u32), (2, 3), (3, 1), (5, 9), (13, 7), (37, 19)];
        let mut out = Vec::new();
        let mut rng = tu::Rng(2024);
        for (color, depths) in combos {
            for &depth in depths {
                for &(w, h) in &sizes {
                    for variant in 0..2 {
                        let ch = channels(color);
                        let max = (1u32 << depth) - 1;
                        let n = (w * h) as usize;
                        let mut samples = Vec::with_capacity(n * ch);
                        for i in 0..n {
                            let (x, y) = (i as u32 % w, i as u32 / w);
                            for c in 0..ch {
                                // Gradients plus noise; alpha channels also get exact 0 and max.
                                let grad = (x * 40503 + y * 20011 + c as u32 * 7919) as u64;
                                let mut v = ((grad ^ (rng.next() & 0xFFFF)) as u32) & max;
                                if (color == 4 || color == 6) && c == ch - 1 && i % 5 == 0 {
                                    v = if i % 10 == 0 { 0 } else { max };
                                }
                                if color == 3 {
                                    v %= (max + 1).min(200); // some indexes past a short palette below
                                }
                                samples.push(v as u16);
                            }
                        }
                        let mut s = Src { w, h, color, depth, samples, palette: vec![], palette_alpha: 0, key: None };
                        if color == 3 {
                            let entries =
                                if variant == 0 { (max + 1) as usize } else { ((max + 1) as usize * 3 / 4).max(1) };
                            s.palette = (0..entries)
                                .map(|_| {
                                    let r = rng.next();
                                    [r as u8, (r >> 8) as u8, (r >> 16) as u8, (r >> 24) as u8]
                                })
                                .collect();
                            if variant == 1 {
                                s.palette_alpha = entries / 2 + 1;
                            }
                        }
                        if variant == 1 && (color == 0 || color == 2) {
                            // Use a color that occurs in the image as the transparent key.
                            let k = &s.samples[ch * (n / 2)..ch * (n / 2) + ch];
                            s.key = Some(if color == 0 { [k[0], k[0], k[0]] } else { [k[0], k[1], k[2]] });
                        }
                        if variant == 1 && color != 0 && color != 2 && color != 3 {
                            continue; // no tRNS variant for types with an alpha channel
                        }
                        let name = format!("c{}d{}_{}x{}_v{}", color, depth, w, h, variant);
                        out.push((name, s));
                    }
                }
            }
        }
        out
    }

    #[test]
    fn hand_made_all_types_vs_expected_and_wic() {
        let mut tally = tu::Tally::default();
        let mut checked = 0;
        for (name, src) in hand_made_sources() {
            let want = expected(&src);
            for interlaced in [false, true] {
                let file = write_png(&src, interlaced);
                let label = format!("{}{}", name, if interlaced { "_adam7" } else { "" });
                let ours = decode(&file).unwrap_or_else(|| panic!("{}: our decoder failed", label));
                assert_eq!((ours.w, ours.h), (src.w, src.h), "{}", label);
                assert_eq!(dimensions(&file), Some((src.w, src.h)));
                // Exact match with the independently computed result.
                let d = tu::compare(&ours.px, &want, src.w, src.h);
                assert!(d.max == 0, "{}: differs from expected: {:?}", label, d);
                checked += 1;
                // And against WIC.
                match tu::wic_decode(&file) {
                    Ok(wic) if (wic.w, wic.h) == (src.w, src.h) => {
                        let d = tu::compare(&ours.px, &wic.px, src.w, src.h);
                        tally.add_diff(&label, &d, &tu::format_name(&wic.native));
                    }
                    Ok(wic) => tally.size_mismatch.push(format!("{}: WIC {}x{}", label, wic.w, wic.h)),
                    Err(e) => tally.wic_failed.push(format!("{}: {}", label, e)),
                }
            }
        }
        println!("hand-made PNGs: {} files match the expected pixels exactly", checked);
        tally.print("hand-made PNGs vs WIC");
        // Differences of 1 are allowed (WIC rounds 16-bit samples, we take the high byte).
        assert!(tally.differ.is_empty() && tally.wic_failed.is_empty() && tally.size_mismatch.is_empty());
    }

    // ------------------------------------------------------------ WIC-encoded samples

    /// Deterministic gradient-plus-noise bytes for a pixel buffer.
    fn pattern(stride: usize, h: usize, seed: u64) -> Vec<u8> {
        let mut rng = tu::Rng(seed);
        let mut v = vec![0u8; stride * h];
        for y in 0..h {
            for i in 0..stride {
                v[y * stride + i] = ((i * 3 + y * 5) as u8) ^ (rng.next() as u8 & 0x1F);
            }
        }
        v
    }

    fn palette(n: usize, alpha: bool, seed: u64) -> Vec<u32> {
        let mut rng = tu::Rng(seed);
        (0..n)
            .map(|i| {
                let c = rng.next() as u32 & 0x00FF_FFFF;
                let a = if alpha && i % 3 == 0 { (i * 37 % 256) as u32 } else { 255 };
                a << 24 | c
            })
            .collect()
    }

    /// Writes PNG variants with WIC's encoder; returns (name, path) of those produced
    /// and a list of variants that could not be produced.
    fn wic_variants(folder: &str) -> (Vec<(String, std::path::PathBuf)>, Vec<String>) {
        let dir = tu::testdata_dir(folder);
        let (w, h) = (67u32, 45u32);
        let formats: [(&str, GUID, u32, usize); 13] = [
            ("1bppIndexed", GUID_WICPixelFormat1bppIndexed, 1, 2),
            ("2bppIndexed", GUID_WICPixelFormat2bppIndexed, 2, 4),
            ("4bppIndexed", GUID_WICPixelFormat4bppIndexed, 4, 16),
            ("8bppIndexed", GUID_WICPixelFormat8bppIndexed, 8, 256),
            ("BlackWhite", GUID_WICPixelFormatBlackWhite, 1, 0),
            ("2bppGray", GUID_WICPixelFormat2bppGray, 2, 0),
            ("4bppGray", GUID_WICPixelFormat4bppGray, 4, 0),
            ("8bppGray", GUID_WICPixelFormat8bppGray, 8, 0),
            ("16bppGray", GUID_WICPixelFormat16bppGray, 16, 0),
            ("24bppBGR", GUID_WICPixelFormat24bppBGR, 24, 0),
            ("32bppBGRA", GUID_WICPixelFormat32bppBGRA, 32, 0),
            ("48bppRGB", GUID_WICPixelFormat48bppRGB, 48, 0),
            ("64bppRGBA", GUID_WICPixelFormat64bppRGBA, 64, 0),
        ];
        let mut made = Vec::new();
        let mut failed = Vec::new();
        for (i, (name, fmt, bits, pal_n)) in formats.iter().enumerate() {
            let stride = (w as usize * *bits as usize + 7) / 8;
            let pixels = pattern(stride, h as usize, i as u64 + 1);
            let pal = if *pal_n > 0 { Some(palette(*pal_n, true, i as u64 + 9)) } else { None };
            let mut variants: Vec<(String, Vec<(&str, Opt)>)> = vec![
                (name.to_string(), vec![]),
                (format!("{}_interlaced", name), vec![("InterlaceOption", Opt::Bool(true))]),
            ];
            if *name == "24bppBGR" || *name == "32bppBGRA" || *name == "64bppRGBA" {
                // WICPngFilterOption: 1 none, 2 sub, 3 up, 4 average, 5 paeth, 6 adaptive.
                for (f, fname) in [(1u8, "none"), (2, "sub"), (3, "up"), (4, "avg"), (5, "paeth"), (6, "adaptive")] {
                    variants.push((format!("{}_filter_{}", name, fname), vec![("FilterOption", Opt::U8(f))]));
                }
            }
            for (vname, opts) in variants {
                let path = dir.join(format!("{}.png", vname));
                match tu::wic_encode(
                    &path,
                    &GUID_ContainerFormatPng,
                    fmt,
                    w,
                    h,
                    stride as u32,
                    &pixels,
                    pal.as_deref(),
                    &opts,
                ) {
                    Ok(()) => made.push((vname, path)),
                    Err(e) => failed.push(format!("{}: {}", vname, e)),
                }
            }
        }
        (made, failed)
    }

    /// IHDR summary for reports.
    fn describe(data: &[u8]) -> String {
        match parse_header(data) {
            Some(h) => format!("type {} depth {}{}", h.color, h.depth, if h.interlaced { " adam7" } else { "" }),
            None => "bad header".into(),
        }
    }

    fn compare_files(title: &str, files: &[(String, Vec<u8>)]) -> tu::Tally {
        let results = tu::par_map(files, &|(name, data)| {
            let ours = decode(data);
            let wic = tu::wic_decode(data);
            let d = match (&ours, &wic) {
                (Some(o), Ok(w)) if (o.w, o.h) == (w.w, w.h) => Some(tu::compare(&o.px, &w.px, o.w, o.h)),
                _ => None,
            };
            let note = match &wic {
                Ok(w) => format!("{}; WIC native {}", describe(data), tu::format_name(&w.native)),
                Err(_) => describe(data),
            };
            (name.clone(), ours.map(|o| (o.w, o.h)), wic.map(|w| (w.w, w.h)).map_err(|e| e.to_string()), d, note)
        });
        let mut tally = tu::Tally::default();
        for (name, ours, wic, d, note) in results {
            match (ours, wic, d) {
                (_, _, Some(d)) => tally.add_diff(&name, &d, &note),
                (None, Ok(_), _) => tally.ours_failed.push(format!("{} [{}]", name, note)),
                (Some(_), Err(e), _) => tally.wic_failed.push(format!("{}: {} [{}]", name, e, note)),
                (None, Err(e), _) => tally.wic_failed.push(format!("{}: both failed ({}) [{}]", name, e, note)),
                (Some(o), Ok(w), None) => tally.size_mismatch.push(format!("{}: ours {:?} WIC {:?}", name, o, w)),
            }
        }
        tally.print(title);
        tally
    }

    #[test]
    fn wic_encoded_variants_vs_wic() {
        let (made, failed) = wic_variants("png_wic");
        for f in &failed {
            println!("could not produce: {}", f);
        }
        let files: Vec<(String, Vec<u8>)> = made.iter().map(|(n, p)| (n.clone(), std::fs::read(p).unwrap())).collect();
        let tally = compare_files("WIC-encoded PNGs", &files);
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
            tu::samples().png.iter().filter_map(|p| std::fs::read(p).ok().map(|d| (tu::short(p), d))).collect();
        let mut kinds = std::collections::BTreeMap::new();
        for (_, d) in &files {
            *kinds.entry(describe(d)).or_insert(0) += 1;
        }
        println!("Windows PNG samples: {} files; by IHDR: {:?}", files.len(), kinds);
        let tally = compare_files("Windows PNG samples", &files);
        // Known: this 1x1 file has a wrong Adler-32. Its pixel data (filter 0, then
        // FF 00 00) is red, which we show (as browsers do); WIC returns black.
        let known = [r"AutoPlay\Assets\Square44x44Logo.png"];
        for d in &tally.differ {
            assert!(known.iter().any(|k| d.contains(k)), "unexpected difference: {}", d);
        }
        assert!(tally.wic_failed.is_empty());
        assert!(tally.ours_failed.is_empty() && tally.size_mismatch.is_empty());
    }

    #[test]
    fn header_checks_and_limits() {
        let src =
            Src { w: 3, h: 2, color: 6, depth: 8, samples: vec![7; 24], palette: vec![], palette_alpha: 0, key: None };
        let good = write_png(&src, false);
        assert_eq!(dimensions(&good), Some((3, 2)));
        assert!(decode(&good).is_some());
        // Bad signature, IHDR fields and limits.
        let patch = |at: usize, bytes: &[u8]| {
            let mut f = good.clone();
            f[at..at + bytes.len()].copy_from_slice(bytes);
            f
        };
        assert!(decode(&patch(0, &[0])).is_none());
        assert!(decode(&patch(24, &[3])).is_none(), "depth 3");
        assert!(decode(&patch(25, &[5])).is_none(), "color type 5");
        assert!(decode(&patch(26, &[1])).is_none(), "compression method");
        assert!(decode(&patch(27, &[1])).is_none(), "filter method");
        assert!(decode(&patch(28, &[2])).is_none(), "interlace method");
        assert!(decode(&patch(16, &[0, 0, 0, 0])).is_none(), "zero width");
        // Over 200 megapixels: rejected from the header alone.
        let huge = patch(16, &[0, 0, 0x4E, 0x20, 0, 0, 0x4E, 0x20]);
        assert!(dimensions(&huge).is_none() && decode(&huge).is_none());
        // 20000 x 10000 = 200 MP is allowed by the header but the data is implausible.
        let big = patch(16, &[0, 0, 0x4E, 0x20, 0, 0, 0x27, 0x10]);
        assert_eq!(dimensions(&big), Some((20000, 10000)));
        assert!(decode(&big).is_none());
        // Bad filter type.
        let mut s2 =
            Src { w: 2, h: 1, color: 0, depth: 8, samples: vec![1, 2], palette: vec![], palette_alpha: 0, key: None };
        let mut f = write_png(&s2, false);
        let pos = f.windows(4).position(|c| c == b"IDAT").unwrap() + 4 + 7; // zlib(2) + stored header(5)
        f[pos] = 5;
        assert!(decode(&f).is_none(), "filter type 5");
        // Palette image without PLTE.
        s2.color = 3;
        s2.palette = vec![];
        assert!(decode(&write_png(&s2, false)).is_none());
        // APNG: acTL/fcTL before IDAT and fcTL/fdAT after it are ignored; the default
        // image (IDAT) is decoded.
        let iend = good.len() - 12;
        let first_idat = good.windows(4).position(|c| c == b"IDAT").unwrap() - 4;
        let mut apng = good[..first_idat].to_vec();
        chunk(&mut apng, b"acTL", &[0, 0, 0, 2, 0, 0, 0, 0]);
        chunk(&mut apng, b"fcTL", &[0; 26]);
        apng.extend_from_slice(&good[first_idat..iend]);
        chunk(&mut apng, b"fcTL", &[0; 26]);
        chunk(&mut apng, b"fdAT", &[0, 0, 0, 2, 0x78, 0x01, 0xFF]);
        apng.extend_from_slice(&good[iend..]);
        assert_eq!(decode(&apng).map(|d| d.px), decode(&good).map(|d| d.px));
        // Missing IEND is fine; a bad CRC is ignored.
        let no_iend = &good[..good.len() - 12];
        assert!(decode(no_iend).is_some());
        let mut bad_crc = good.clone();
        let i = bad_crc.len() - 13;
        bad_crc[i] ^= 0xFF; // last byte of the IDAT CRC
        assert!(decode(&bad_crc).is_some());
    }

    // ------------------------------------------------------------ fuzzing (debug build)

    #[test]
    #[cfg_attr(not(debug_assertions), ignore = "fuzz: run in a debug build")]
    fn fuzz_no_panic() {
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        for (name, src) in hand_made_sources().into_iter().filter(|(n, _)| n.contains("13x7")) {
            files.push((name.clone(), write_png(&src, false)));
            files.push((name + "_adam7", write_png(&src, true)));
        }
        let (made, _) = wic_variants("png_wic_fuzz");
        for (n, p) in made {
            files.push((n, std::fs::read(p).unwrap()));
        }
        for p in &tu::samples().png {
            if let Ok(d) = std::fs::read(p) {
                files.push((tu::short(p), d));
            }
        }
        let f = |d: &[u8]| {
            let _ = dimensions(d);
            let _ = decode(d);
        };
        let (n, cases, fails) = tu::fuzz_files(&files, &f);
        println!("PNG fuzz: {} files, {} cases, {} panics", n, cases, fails.len());
        assert!(fails.is_empty(), "{:#?}", fails);
    }

    // ------------------------------------------------------------ timing (release build)

    fn time<F: FnMut()>(reps: usize, mut f: F) -> f64 {
        let mut best = f64::MAX;
        for _ in 0..reps {
            let t = std::time::Instant::now();
            f();
            best = best.min(t.elapsed().as_secs_f64());
        }
        best * 1e3
    }

    #[test]
    #[cfg_attr(debug_assertions, ignore = "timing: run with --release")]
    fn speed_phases() {
        // reads samples from all over C:\Windows: run with DVFILES_SHELLTEST=1
        if std::env::var_os("DVFILES_SHELLTEST").is_none() {
            return;
        }
        let path = tu::testdata_dir("png_wic").join("big_1920x1080_gradient+n.png");
        let Ok(data) = std::fs::read(&path) else { return };
        let hdr = parse_header(&data).unwrap();
        let mut z = Vec::new();
        let mut pos = 8;
        let mut filters = [0usize; 5];
        while let (Some(len), Some(kind)) = (be32(&data, pos), data.get(pos + 4..pos + 8)) {
            if kind == b"IDAT" {
                z.extend_from_slice(&data[pos + 8..pos + 8 + len as usize]);
            }
            pos += 12 + len as usize;
        }
        let stride = hdr.stride(hdr.w) as usize;
        let size = hdr.h as usize * (stride + 1);
        let raw0 = zlib_decompress_known_size(&z, size).unwrap();
        for y in 0..hdr.h as usize {
            filters[raw0[y * (stride + 1)] as usize] += 1;
        }
        let mut raw = raw0.clone();
        let t = std::time::Instant::now();
        for y in 0..hdr.h as usize {
            let off = y * (stride + 1);
            let (done, rest) = raw.split_at_mut(off);
            let (f, cur) = rest[..stride + 1].split_first_mut().unwrap();
            let prev = if y > 0 { Some(&done[off - stride..off]) } else { None };
            unfilter(*f, cur, prev, 4).unwrap();
        }
        let unfilter_ms = t.elapsed().as_secs_f64() * 1e3;
        let conv = Converter::new(&hdr, &[], &[]).unwrap();
        let t = std::time::Instant::now();
        let mut px = vec![0u32; (hdr.w * hdr.h) as usize];
        for (y, dst) in px.chunks_exact_mut(hdr.w as usize).enumerate() {
            let off = y * (stride + 1) + 1;
            conv.row(&raw[off..off + stride], dst);
        }
        let convert_ms = t.elapsed().as_secs_f64() * 1e3;
        println!(
            "phases: filters per type {:?}; unfilter {:.2} ms; alloc+convert {:.2} ms",
            filters, unfilter_ms, convert_ms
        );
    }

    #[test]
    #[cfg_attr(debug_assertions, ignore = "timing: run with --release")]
    fn speed_decode() {
        // reads samples from all over C:\Windows: run with DVFILES_SHELLTEST=1
        if std::env::var_os("DVFILES_SHELLTEST").is_none() {
            return;
        }
        // Two 1920x1080 RGBA images (BGRA in memory for WIC), both with varying alpha:
        // a screenshot-like one (flat panels, gradients, fine "text" detail) and a
        // worst case (gradients with noise in the low bits: compresses only ~3:1).
        let (w, h) = (1920u32, 1080u32);
        let mut rng = tu::Rng(42);
        let mut noisy = vec![0u8; (w * h * 4) as usize];
        let mut screen = vec![0u8; (w * h * 4) as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let i = (y * w as usize + x) * 4;
                let n = rng.next();
                noisy[i] = (x / 8) as u8 ^ (n as u8 & 7);
                noisy[i + 1] = (y / 5) as u8 ^ ((n >> 8) as u8 & 7);
                noisy[i + 2] = ((x + y) / 12) as u8;
                noisy[i + 3] = if x < 1700 { 255 } else { (y % 256) as u8 };
                let panel = (x / 480 + y / 270) % 3;
                let text = y % 18 < 12 && x % 9 < 6 && (n >> 20) % 3 == 0 && panel == 1;
                let (b, g, r) = match (panel, text) {
                    (_, true) => (30, 30, 30),
                    (0, _) => ((x / 8) as u8, (y / 5) as u8, 200),
                    (1, _) => (245, 245, 245),
                    _ => (180, 120, (x % 256) as u8),
                };
                screen[i..i + 4].copy_from_slice(&[b, g, r, if y < 1000 { 255 } else { (x % 256) as u8 }]);
            }
        }
        let mut files = Vec::new();
        for (name, px) in [("screenshot-like", &screen), ("gradient+noise (worst case)", &noisy)] {
            let path = tu::testdata_dir("png_wic").join(format!("big_1920x1080_{}.png", &name[..10]));
            tu::wic_encode(&path, &GUID_ContainerFormatPng, &GUID_WICPixelFormat32bppBGRA, w, h, w * 4, px, None, &[])
                .unwrap();
            files.push((format!("1920x1080 RGBA {}", name), std::fs::read(&path).unwrap()));
        }
        // Plus the largest Windows samples by pixel count.
        let mut samples: Vec<_> =
            tu::samples().png.iter().filter_map(|p| std::fs::read(p).ok().map(|d| (tu::short(p), d))).collect();
        samples.sort_by_key(|(_, d)| std::cmp::Reverse(dimensions(d).map_or(0, |(w, h)| w as u64 * h as u64)));
        files.extend(samples.into_iter().take(2));
        for (name, data) in &files {
            let img = decode(data).expect("decode");
            let ours = time(15, || {
                std::hint::black_box(decode(data));
            });
            // The inflate part alone (all IDAT data, output size from the header).
            let mut z = Vec::new();
            let mut pos = 8;
            while let (Some(len), Some(kind)) = (be32(data, pos), data.get(pos + 4..pos + 8)) {
                if kind == b"IDAT" {
                    z.extend_from_slice(&data[pos + 8..pos + 8 + len as usize]);
                }
                pos += 12 + len as usize;
            }
            let hdr = parse_header(data).unwrap();
            let size: u64 = passes(&hdr).iter().map(|p| p.h as u64 * (1 + hdr.stride(p.w))).sum();
            let inflate_ms = time(15, || {
                std::hint::black_box(zlib_decompress_known_size(&z, size as usize));
            });
            let wic = time(5, || {
                std::hint::black_box(tu::wic_decode(data).ok());
            });
            println!(
                "PNG decode {} ({}x{}, {}, {} KB): ours {:.2} ms (inflate {:.2} ms), WIC (incl. PBGRA conversion) {:.2} ms",
                name,
                img.w,
                img.h,
                describe(data),
                data.len() / 1024,
                ours,
                inflate_ms,
                wic
            );
        }
    }
}
