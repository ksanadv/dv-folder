// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Small, dependency-free baseline JPEG encoder for thumbnails.
//!
//! Writes baseline (SOF0) JFIF files: 8-bit YCbCr with 4:2:0 or 4:4:4 chroma sampling, the example
//! quantisation tables of the JPEG spec scaled by quality the way libjpeg does, and the standard
//! Huffman tables of Annex K, in one interleaved scan without restart markers. Partial blocks at the
//! right and bottom edges repeat the last column and row. Bad sizes give `None`; no input makes it panic.

/// Chroma subsampling of the encoded file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sampling {
    /// Colour at half width and half height: smaller files, the usual choice for photos.
    #[default]
    S420,
    /// Colour at full resolution: sharper coloured edges and text, larger files.
    S444,
}

/// Encodes premultiplied BGRA pixels, packed as `(a << 24) | (r << 16) | (g << 8) | b` with the top row
/// first, as a baseline JPEG. Transparent parts are laid on `background` (0xRRGGBB; the top byte is
/// ignored). `quality` works like libjpeg's 1..=100: 0 counts as 1 and anything above 100 as 100.
/// Returns None when the width or height is 0 or above 65535, or `px.len()` is not `w * h`.
pub fn encode(px: &[u32], w: u32, h: u32, quality: u8, sampling: Sampling, background: u32) -> Option<Vec<u8>> {
    if w == 0 || h == 0 || w > 0xFFFF || h > 0xFFFF {
        return None;
    }
    let (wu, hu) = (usize::try_from(w).ok()?, usize::try_from(h).ok()?);
    if px.len() != wu.checked_mul(hu)? {
        return None;
    }
    let quality = quality.clamp(1, 100);
    let tables = [scaled_table(&LUMA_Q, quality), scaled_table(&CHROMA_Q, quality)];
    let mut out = Vec::with_capacity((1024 + px.len() / 4).min(1 << 26));
    write_headers(&mut out, w as u16, h as u16, &tables, sampling);

    let mut scan = Scan::new(out, &tables);
    let mcu = if sampling == Sampling::S420 { 16 } else { 8 };
    let padded_w = wu.div_ceil(mcu) * mcu;
    let mut rows = Rows::new(padded_w, mcu);
    // Background channels per unit of (255 - alpha).
    let bg = [16, 8, 0].map(|s| ((background >> s) & 255) as f32 / 255.0);
    let mut blk = [0f32; 64];
    for y0 in (0..hu).step_by(mcu) {
        rows.fill(px, wu, hu, y0, &bg)?;
        for x0 in (0..padded_w).step_by(mcu) {
            match sampling {
                Sampling::S420 => {
                    for (dx, dy) in [(0, 0), (8, 0), (0, 8), (8, 8)] {
                        load_block(&rows.y, padded_w, x0 + dx, dy, &mut blk);
                        scan.block(&mut blk, 0);
                    }
                    load_half_block(&rows.cb, padded_w, x0, &mut blk);
                    scan.block(&mut blk, 1);
                    load_half_block(&rows.cr, padded_w, x0, &mut blk);
                    scan.block(&mut blk, 2);
                }
                Sampling::S444 => {
                    for (comp, plane) in [&rows.y, &rows.cb, &rows.cr].into_iter().enumerate() {
                        load_block(plane, padded_w, x0, 0, &mut blk);
                        scan.block(&mut blk, comp);
                    }
                }
            }
        }
    }
    let mut out = scan.bits.finish();
    out.extend_from_slice(&[0xFF, 0xD9]);
    Some(out)
}

// ------------------------------------------------------------------------------------------------
// Tables
// ------------------------------------------------------------------------------------------------

/// ZIGZAG[k] is the row-major position of the k-th coefficient in stream (zigzag) order.
const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21,
    28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54,
    47, 55, 62, 63,
];

/// Luminance quantisation table for quality 50 (JPEG spec, Annex K.1), row-major.
const LUMA_Q: [u8; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, //
    12, 12, 14, 19, 26, 58, 60, 55, //
    14, 13, 16, 24, 40, 57, 69, 56, //
    14, 17, 22, 29, 51, 87, 80, 62, //
    18, 22, 37, 56, 68, 109, 103, 77, //
    24, 35, 55, 64, 81, 104, 113, 92, //
    49, 64, 78, 87, 103, 121, 120, 101, //
    72, 92, 95, 98, 112, 100, 103, 99,
];

/// Chrominance quantisation table for quality 50 (JPEG spec, Annex K.1), row-major.
const CHROMA_Q: [u8; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, //
    18, 21, 26, 66, 99, 99, 99, 99, //
    24, 26, 56, 99, 99, 99, 99, 99, //
    47, 66, 99, 99, 99, 99, 99, 99, //
    99, 99, 99, 99, 99, 99, 99, 99, //
    99, 99, 99, 99, 99, 99, 99, 99, //
    99, 99, 99, 99, 99, 99, 99, 99, //
    99, 99, 99, 99, 99, 99, 99, 99,
];

/// Standard Huffman tables (Annex K.3): number of codes of each length 1..=16, then the symbols.
const DC_LUMA_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const DC_CHROMA_BITS: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
const DC_VALS: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

const AC_LUMA_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d];
const AC_LUMA_VALS: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71, 0x14,
    0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09,
    0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a,
    0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65,
    0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88,
    0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9,
    0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca,
    0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea,
    0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
];

const AC_CHROMA_BITS: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
const AC_CHROMA_VALS: [u8; 162] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71, 0x13, 0x22, 0x32,
    0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16,
    0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39,
    0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64,
    0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x82, 0x83, 0x84, 0x85, 0x86,
    0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7,
    0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8,
    0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9,
    0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
];

/// The four Huffman tables as written to the DHT segment: (class << 4 | id, code counts, symbols).
const HUFFMAN_TABLES: [(u8, &[u8; 16], &[u8]); 4] = [
    (0x00, &DC_LUMA_BITS, &DC_VALS),
    (0x10, &AC_LUMA_BITS, &AC_LUMA_VALS),
    (0x01, &DC_CHROMA_BITS, &DC_VALS),
    (0x11, &AC_CHROMA_BITS, &AC_CHROMA_VALS),
];

/// AAN DCT scale factors: cos(k * pi / 16) * sqrt(2), and 1 for k = 0.
const AAN: [f32; 8] = [1.0, 1.387_039_8, 1.306_563, 1.175_875_6, 1.0, 0.785_694_96, 0.541_196_1, 0.275_899_38];

/// Scales a quality-50 table like libjpeg (jpeg_quality_scaling + jpeg_add_quant_table with
/// force_baseline): quality below 50 multiplies by 50 / quality, above 50 by (100 - quality) / 50,
/// and every entry is kept in 1..=255 so it fits the 8-bit DQT entries of a baseline file.
fn scaled_table(base: &[u8; 64], quality: u8) -> [u8; 64] {
    let q = u32::from(quality.clamp(1, 100));
    let scale = if q < 50 { 5000 / q } else { 200 - 2 * q };
    base.map(|b| ((u32::from(b) * scale + 50) / 100).clamp(1, 255) as u8)
}

// ------------------------------------------------------------------------------------------------
// File headers
// ------------------------------------------------------------------------------------------------

/// Writes SOI, APP0 (JFIF 1.01), DQT, SOF0, DHT and SOS: everything before the entropy-coded data.
fn write_headers(out: &mut Vec<u8>, w: u16, h: u16, tables: &[[u8; 64]; 2], sampling: Sampling) {
    out.extend_from_slice(&[0xFF, 0xD8]);
    // APP0: "JFIF\0", version 1.01, no density unit with a 1:1 aspect ratio, no embedded thumbnail.
    out.extend_from_slice(&[0xFF, 0xE0, 0, 16, b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0]);
    // DQT: two 8-bit tables (0 = luma, 1 = chroma) in zigzag order.
    out.extend_from_slice(&[0xFF, 0xDB, 0, 132]);
    for (id, table) in (0u8..).zip(tables) {
        out.push(id);
        out.extend(ZIGZAG.iter().map(|&i| table[usize::from(i) & 63]));
    }
    // SOF0: 8-bit samples, height, width, 3 components (id, sampling factors, quantisation table).
    let y_factors = if sampling == Sampling::S420 { 0x22 } else { 0x11 };
    let [h1, h0] = h.to_be_bytes();
    let [w1, w0] = w.to_be_bytes();
    out.extend_from_slice(&[0xFF, 0xC0, 0, 17, 8, h1, h0, w1, w0, 3, 1, y_factors, 0, 2, 0x11, 1, 3, 0x11, 1]);
    // DHT: all four tables in one segment.
    let len: usize = 2 + HUFFMAN_TABLES.iter().map(|(_, _, vals)| 17 + vals.len()).sum::<usize>();
    out.extend_from_slice(&[0xFF, 0xC4]);
    out.extend_from_slice(&(len as u16).to_be_bytes());
    for (class_id, bits, vals) in HUFFMAN_TABLES {
        out.push(class_id);
        out.extend_from_slice(bits);
        out.extend_from_slice(vals);
    }
    // SOS: the three components (Y with tables 0/0, Cb and Cr with 1/1), spectral range 0..63.
    out.extend_from_slice(&[0xFF, 0xDA, 0, 12, 3, 1, 0x00, 2, 0x11, 3, 0x11, 0, 63, 0]);
}

// ------------------------------------------------------------------------------------------------
// Colour conversion and block extraction
// ------------------------------------------------------------------------------------------------

/// One row of MCUs as Y, Cb and Cr planes (level-shifted floats), `width` wide (the image width rounded
/// up to whole MCUs) and `height` (8 or 16) rows tall.
struct Rows {
    y: Vec<f32>,
    cb: Vec<f32>,
    cr: Vec<f32>,
    width: usize,
    height: usize,
}

impl Rows {
    fn new(width: usize, height: usize) -> Rows {
        let n = width * height;
        Rows { y: vec![0.0; n], cb: vec![0.0; n], cr: vec![0.0; n], width, height }
    }

    /// Converts image rows `y0..y0 + height` to YCbCr. Rows below the image repeat its last row and
    /// columns right of it repeat its last column. `bg` is the background [r, g, b] divided by 255.
    fn fill(&mut self, px: &[u32], w: usize, h: usize, y0: usize, bg: &[f32; 3]) -> Option<()> {
        for r in 0..self.height {
            let sy = (y0 + r).min(h - 1);
            let src = px.get(sy * w..sy * w + w)?;
            let at = r * self.width;
            let yr = self.y.get_mut(at..at + self.width)?;
            let cbr = self.cb.get_mut(at..at + self.width)?;
            let crr = self.cr.get_mut(at..at + self.width)?;
            to_ycbcr(src, yr, cbr, crr, bg);
            for row in [yr, cbr, crr] {
                repeat_last(row, w);
            }
        }
        Some(())
    }
}

/// Lays premultiplied pixels on the background (colour + background * (255 - alpha) / 255, capped at
/// 255, which also tames invalid pixels whose colour exceeds their alpha) and converts them to JFIF
/// YCbCr, level-shifted to be centred on 0. Branch-free, so the compiler can vectorise it.
fn to_ycbcr(src: &[u32], y: &mut [f32], cb: &mut [f32], cr: &mut [f32], bg: &[f32; 3]) {
    let n = src.len().min(y.len()).min(cb.len()).min(cr.len());
    let (src, y, cb, cr) = (&src[..n], &mut y[..n], &mut cb[..n], &mut cr[..n]);
    for i in 0..n {
        let p = src[i];
        let k = (255 - (p >> 24)) as i32 as f32;
        let channel = |shift: u32, bg: f32| (((p >> shift) & 255) as i32 as f32 + bg * k).min(255.0);
        let (r, g, b) = (channel(16, bg[0]), channel(8, bg[1]), channel(0, bg[2]));
        y[i] = 0.299 * r + 0.587 * g + 0.114 * b - 128.0;
        cb[i] = -0.168_736 * r - 0.331_264 * g + 0.5 * b;
        cr[i] = 0.5 * r - 0.418_688 * g - 0.081_312 * b;
    }
}

/// Fills `row[w..]` with `row[w - 1]`.
fn repeat_last(row: &mut [f32], w: usize) {
    if let Some(&last) = row.get(w.wrapping_sub(1)) {
        if let Some(rest) = row.get_mut(w..) {
            rest.fill(last);
        }
    }
}

/// Copies the 8x8 block at (x0, y0) of a plane `width` wide.
fn load_block(plane: &[f32], width: usize, x0: usize, y0: usize, blk: &mut [f32; 64]) {
    for (r, dst) in blk.as_chunks_mut::<8>().0.iter_mut().enumerate() {
        let at = (y0 + r) * width + x0;
        if let Some(src) = plane.get(at..at + 8) {
            dst.copy_from_slice(src);
        }
    }
}

/// Averages the 16x16 area at column x0 of a 16-row plane down to an 8x8 block (4:2:0 chroma).
fn load_half_block(plane: &[f32], width: usize, x0: usize, blk: &mut [f32; 64]) {
    for (r, dst) in blk.as_chunks_mut::<8>().0.iter_mut().enumerate() {
        let at = 2 * r * width + x0;
        let (Some(top), Some(bottom)) = (plane.get(at..at + 16), plane.get(at + width..at + width + 16)) else {
            continue;
        };
        for ((d, t), b) in dst.iter_mut().zip(top.as_chunks::<2>().0).zip(bottom.as_chunks::<2>().0) {
            *d = 0.25 * (t[0] + t[1] + b[0] + b[1]);
        }
    }
}

// ------------------------------------------------------------------------------------------------
// DCT and quantisation
// ------------------------------------------------------------------------------------------------

/// One 8-point AAN forward DCT (as in libjpeg's jfdctflt.c). Output k comes out scaled by AAN[k] * sqrt(8).
#[inline]
fn dct8(x: [f32; 8]) -> [f32; 8] {
    let [x0, x1, x2, x3, x4, x5, x6, x7] = x;
    let (t0, t7) = (x0 + x7, x0 - x7);
    let (t1, t6) = (x1 + x6, x1 - x6);
    let (t2, t5) = (x2 + x5, x2 - x5);
    let (t3, t4) = (x3 + x4, x3 - x4);
    // Even part.
    let (t10, t13) = (t0 + t3, t0 - t3);
    let (t11, t12) = (t1 + t2, t1 - t2);
    let z1 = (t12 + t13) * std::f32::consts::FRAC_1_SQRT_2;
    // Odd part.
    let (o10, o11, o12) = (t4 + t5, t5 + t6, t6 + t7);
    let z5 = (o10 - o12) * 0.382_683_43;
    let z2 = 0.541_196_1 * o10 + z5;
    let z4 = 1.306_563 * o12 + z5;
    let z3 = o11 * std::f32::consts::FRAC_1_SQRT_2;
    let (z11, z13) = (t7 + z3, t7 - z3);
    [t10 + t11, z11 + z4, t13 + z1, z13 - z2, t10 - t11, z13 + z2, t13 - z1, z11 - z4]
}

/// For each row-major position, 1 / (quantiser * the AAN output scale), so quantising is one multiply.
fn reciprocals(table: &[u8; 64]) -> [f32; 64] {
    let mut out = [0f32; 64];
    for (i, (o, &q)) in out.iter_mut().zip(table).enumerate() {
        *o = 1.0 / (f32::from(q.max(1)) * AAN[i >> 3 & 7] * AAN[i & 7] * 8.0);
    }
    out
}

/// Adding 1.5 * 2^23 to a float of magnitude below 2^22 rounds it to the nearest integer and leaves that
/// integer in the low bits: the result's bit pattern minus ROUND_BITS.
const ROUND: f32 = 12_582_912.0;
const ROUND_BITS: i32 = 0x4B40_0000;

/// Rounds to the nearest integer within `lo..=1023`.
#[inline]
fn round_clamp(v: f32, lo: f32) -> i32 {
    (v.clamp(lo, 1023.0) + ROUND).to_bits() as i32 - ROUND_BITS
}

/// Forward DCT of a row-major 8x8 block (`d` is used as scratch), quantised into `q` (row-major) with
/// rounding to nearest. Coefficients are clamped to what baseline Huffman tables can code: AC to +-1023,
/// DC to -1024..=1023, so DC differences stay within +-2047. Returns a mask with bit k set when AC
/// coefficient k in zigzag order is not zero.
fn dct_quantise(d: &mut [f32; 64], recip: &[f32; 64], q: &mut [i32; 64]) -> u64 {
    // Columns first, all eight at once (the compiler vectorises this across the columns).
    for c in 0..8 {
        let v = dct8([d[c], d[8 + c], d[16 + c], d[24 + c], d[32 + c], d[40 + c], d[48 + c], d[56 + c]]);
        for (k, v) in v.into_iter().enumerate() {
            d[8 * k + c] = v;
        }
    }
    // Then each row on its own (a transpose to vectorise this pass costs more than it saves).
    for r in 0..8 {
        let i = 8 * r;
        let v = dct8([d[i], d[i + 1], d[i + 2], d[i + 3], d[i + 4], d[i + 5], d[i + 6], d[i + 7]]);
        d[i..i + 8].copy_from_slice(&v);
    }
    for ((q, &v), &r) in q.iter_mut().zip(d.iter()).zip(recip) {
        *q = round_clamp(v * r, -1023.0);
    }
    q[0] = round_clamp(d[0] * recip[0], -1024.0);
    // Which coefficients are not zero: a byte each, every 8 bytes folded into 8 bits by a multiply
    // (bit j of the top byte collects byte j), then the bits reordered into zigzag order 4 at a time.
    let nonzero: [u8; 64] = std::array::from_fn(|i| u8::from(q[i] != 0));
    let mut row_major = 0u64;
    for (r, &bytes) in nonzero.as_chunks::<8>().0.iter().enumerate() {
        row_major |= (u64::from_le_bytes(bytes).wrapping_mul(0x0102_0408_1020_4080) >> 56) << (8 * r);
    }
    let mut mask = 0u64;
    for (n, table) in ZIGZAG_NIBBLES.iter().enumerate() {
        mask |= table[(row_major >> (4 * n) & 15) as usize];
    }
    mask & !1
}

/// ZIGZAG_NIBBLES[n][bits]: the zigzag-order bits of the row-major positions 4n..4n+4 set in `bits`.
const ZIGZAG_NIBBLES: [[u64; 16]; 16] = {
    let mut pos = [0usize; 64];
    let mut k = 0;
    while k < 64 {
        pos[ZIGZAG[k] as usize] = k;
        k += 1;
    }
    let mut table = [[0u64; 16]; 16];
    let mut n = 0;
    while n < 16 {
        let mut bits = 0;
        while bits < 16 {
            let mut b = 0;
            while b < 4 {
                if bits >> b & 1 == 1 {
                    table[n][bits] |= 1 << pos[4 * n + b];
                }
                b += 1;
            }
            bits += 1;
        }
        n += 1;
    }
    table
};

// ------------------------------------------------------------------------------------------------
// Huffman coding
// ------------------------------------------------------------------------------------------------

/// Code and code length for each symbol of one Huffman table (length 0 for unused symbols).
struct Huffman {
    code: [u16; 256],
    len: [u8; 256],
}

impl Huffman {
    /// Builds the canonical codes from code counts per length and the symbol list (JPEG spec, Annex C).
    fn new(bits: &[u8; 16], vals: &[u8]) -> Huffman {
        let mut h = Huffman { code: [0; 256], len: [0; 256] };
        let mut code = 0u32;
        let mut syms = vals.iter();
        for (len, &count) in (1u8..).zip(bits) {
            for _ in 0..count {
                if let Some(&s) = syms.next() {
                    h.code[usize::from(s)] = code as u16;
                    h.len[usize::from(s)] = len;
                }
                code += 1;
            }
            code <<= 1;
        }
        h
    }

    /// (code, length) for a symbol.
    #[inline]
    fn get(&self, sym: u32) -> (u32, u32) {
        let s = (sym & 255) as usize;
        (u32::from(self.code[s]), u32::from(self.len[s]))
    }
}

/// Number of bits needed for |v| (the JPEG "size category").
#[inline]
fn bit_len(v: i32) -> u32 {
    32 - v.unsigned_abs().leading_zeros()
}

/// The `size` extra bits that follow a size category: v itself if positive, v - 1 (one's complement) if negative.
#[inline]
fn extra_bits(v: i32, size: u32) -> u32 {
    let mask = (1u32 << (size & 15)) - 1;
    (if v < 0 { v.wrapping_sub(1) } else { v }) as u32 & mask
}

/// Writes bits MSB first, inserting a 0x00 after every 0xFF byte (byte stuffing).
struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    /// Appends the low `len` bits of `bits` (len at most 31; the bits above `len` must be zero). Fewer
    /// than 32 bits wait in `acc` between calls, so it never holds more than 63.
    #[inline(always)]
    fn put(&mut self, bits: u32, len: u32) {
        self.acc = (self.acc << (len & 31)) | u64::from(bits);
        self.n += len & 31;
        if self.n >= 32 {
            self.n -= 32;
            let word = (self.acc >> self.n) as u32;
            // Copy four bytes at once unless one of them is 0xFF (zero-byte test on the inverted word).
            if (!word).wrapping_sub(0x0101_0101) & word & 0x8080_8080 == 0 {
                self.out.extend_from_slice(&word.to_be_bytes());
            } else {
                word.to_be_bytes().into_iter().for_each(|b| self.byte(b));
            }
        }
    }

    /// Appends one byte, stuffed.
    fn byte(&mut self, b: u8) {
        self.out.push(b);
        if b == 0xFF {
            self.out.push(0);
        }
    }

    /// Pads the last byte with 1 bits, writes what is left and returns the output.
    fn finish(mut self) -> Vec<u8> {
        let pad = (8 - self.n % 8) % 8;
        self.acc = (self.acc << pad) | ((1 << pad) - 1);
        self.n += pad;
        while self.n >= 8 {
            self.n -= 8;
            self.byte((self.acc >> self.n) as u8);
        }
        self.out
    }
}

/// State of the one interleaved scan: output, quantisers and Huffman tables (0 = luma, 1 = chroma),
/// and the DC predictor of each component.
struct Scan {
    bits: BitWriter,
    recip: [[f32; 64]; 2],
    dc: [Huffman; 2],
    ac: [Huffman; 2],
    pred: [i32; 3],
}

impl Scan {
    fn new(out: Vec<u8>, tables: &[[u8; 64]; 2]) -> Scan {
        Scan {
            bits: BitWriter { out, acc: 0, n: 0 },
            recip: [reciprocals(&tables[0]), reciprocals(&tables[1])],
            dc: [Huffman::new(&DC_LUMA_BITS, &DC_VALS), Huffman::new(&DC_CHROMA_BITS, &DC_VALS)],
            ac: [Huffman::new(&AC_LUMA_BITS, &AC_LUMA_VALS), Huffman::new(&AC_CHROMA_BITS, &AC_CHROMA_VALS)],
            pred: [0; 3],
        }
    }

    /// Transforms, quantises and Huffman-codes one block of component `comp` (0 = Y, 1 = Cb, 2 = Cr).
    fn block(&mut self, blk: &mut [f32; 64], comp: usize) {
        let t = usize::from(comp != 0);
        let mut q = [0i32; 64];
        let mut mask = dct_quantise(blk, &self.recip[t], &mut q);
        let (dc, ac, bits) = (&self.dc[t], &self.ac[t], &mut self.bits);

        let pred = &mut self.pred[comp % 3];
        let diff = q[0] - *pred;
        *pred = q[0];
        let size = bit_len(diff);
        let (code, len) = dc.get(size);
        bits.put(code << size | extra_bits(diff, size), len + size);

        // AC: (zero run, size) symbols for the non-zero coefficients, ZRL for runs of 16 zeros, EOB.
        let mut last = 0u32;
        while mask != 0 {
            let k = mask.trailing_zeros();
            mask &= mask - 1;
            let mut run = k - last - 1;
            while run >= 16 {
                let (code, len) = ac.get(0xF0);
                bits.put(code, len);
                run -= 16;
            }
            let v = q[usize::from(ZIGZAG[(k & 63) as usize]) & 63];
            let size = bit_len(v);
            let (code, len) = ac.get(run << 4 | size);
            bits.put(code << size | extra_bits(v, size), len + size);
            last = k;
        }
        if last != 63 {
            let (code, len) = ac.get(0x00);
            bits.put(code, len);
        }
    }
}

#[cfg(test)]
mod tests {
    // The project's decoder: crate::jpeg in the scratch crate, the sibling module img::jpeg in dvfiles.
    use super::super::jpeg;
    use super::*;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    /// Windows Imaging Component: an independent reference decoder, and a reference encoder to compare with.
    mod wic {
        use windows::core::PWSTR;
        use windows::Win32::Foundation::HGLOBAL;
        use windows::Win32::Graphics::Imaging::*;
        use windows::Win32::System::Com::StructuredStorage::{CreateStreamOnHGlobal, IPropertyBag2, PROPBAG2};
        use windows::Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, STREAM_SEEK_SET,
        };
        use windows::Win32::System::Variant::{VARIANT, VT_R4, VT_UI1};

        fn factory() -> Option<IWICImagingFactory> {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()
            }
        }

        /// Decodes a JPEG held in memory to (width, height, 32bpp BGRA bytes).
        pub fn decode(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
            unsafe {
                let f = factory()?;
                let stream = f.CreateStream().ok()?;
                stream.InitializeFromMemory(bytes).ok()?;
                let dec = f.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand).ok()?;
                let frame = dec.GetFrame(0).ok()?;
                let conv = f.CreateFormatConverter().ok()?;
                conv.Initialize(
                    &frame,
                    &GUID_WICPixelFormat32bppBGRA,
                    WICBitmapDitherTypeNone,
                    None,
                    0.0,
                    WICBitmapPaletteTypeCustom,
                )
                .ok()?;
                let (mut w, mut h) = (0u32, 0u32);
                conv.GetSize(&mut w, &mut h).ok()?;
                let mut bgra = vec![0u8; w as usize * h as usize * 4];
                conv.CopyPixels(std::ptr::null(), w * 4, &mut bgra).ok()?;
                Some((w, h, bgra))
            }
        }

        /// Encodes opaque 0xAARRGGBB pixels with Windows' JPEG encoder at `quality` (0..1), 4:2:0 or 4:4:4.
        pub fn encode(px: &[u32], w: u32, h: u32, quality: f32, s420: bool) -> Option<Vec<u8>> {
            let bgr: Vec<u8> = px.iter().flat_map(|&p| [p as u8, (p >> 8) as u8, (p >> 16) as u8]).collect();
            unsafe {
                let f = factory()?;
                let stream = CreateStreamOnHGlobal(HGLOBAL::default(), true).ok()?;
                let enc = f.CreateEncoder(&GUID_ContainerFormatJpeg, std::ptr::null()).ok()?;
                enc.Initialize(&stream, WICBitmapEncoderNoCache).ok()?;
                let mut frame: Option<IWICBitmapFrameEncode> = None;
                let mut bag: Option<IPropertyBag2> = None;
                enc.CreateNewFrame(&mut frame, &mut bag).ok()?;
                let (frame, bag) = (frame?, bag?);
                let n1: Vec<u16> = "ImageQuality\0".encode_utf16().collect();
                let n2: Vec<u16> = "JpegYCrCbSubsampling\0".encode_utf16().collect();
                let mut props = [PROPBAG2::default(), PROPBAG2::default()];
                props[0].pstrName = PWSTR(n1.as_ptr() as *mut u16);
                props[1].pstrName = PWSTR(n2.as_ptr() as *mut u16);
                let mut vars: [VARIANT; 2] = std::mem::zeroed();
                (*vars[0].Anonymous.Anonymous).vt = VT_R4;
                (*vars[0].Anonymous.Anonymous).Anonymous.fltVal = quality;
                (*vars[1].Anonymous.Anonymous).vt = VT_UI1;
                // WICJpegYCrCbSubsampling420 = 1, WICJpegYCrCbSubsampling444 = 3.
                (*vars[1].Anonymous.Anonymous).Anonymous.bVal = if s420 { 1 } else { 3 };
                bag.Write(2, props.as_ptr(), vars.as_ptr()).ok()?;
                frame.Initialize(&bag).ok()?;
                frame.SetSize(w, h).ok()?;
                let mut fmt = GUID_WICPixelFormat24bppBGR;
                frame.SetPixelFormat(&mut fmt).ok()?;
                if fmt != GUID_WICPixelFormat24bppBGR {
                    return None;
                }
                frame.WritePixels(h, w * 3, &bgr).ok()?;
                frame.Commit().ok()?;
                enc.Commit().ok()?;
                stream.Seek(0, STREAM_SEEK_SET, None).ok()?;
                let (mut out, mut buf) = (Vec::new(), vec![0u8; 1 << 16]);
                loop {
                    let mut got = 0u32;
                    stream.Read(buf.as_mut_ptr().cast(), buf.len() as u32, Some(&mut got)).ok().ok()?;
                    out.extend_from_slice(buf.get(..got as usize)?);
                    if got == 0 {
                        return Some(out);
                    }
                }
            }
        }
    }

    /// Deterministic xorshift64 random numbers.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n.max(1)
        }
    }

    fn rgb(r: u32, g: u32, b: u32) -> u32 {
        0xFF00_0000 | r.min(255) << 16 | g.min(255) << 8 | b.min(255)
    }

    /// The test's own compositing of premultiplied pixels on `bg`: the reference every result is measured against.
    fn flatten(px: &[u32], bg: u32) -> Vec<u32> {
        px.iter()
            .map(|&p| {
                let k = 255 - (p >> 24);
                let ch = |s: u32| ((p >> s) & 255) + (((bg >> s) & 255) * k + 127) / 255;
                rgb(ch(16), ch(8), ch(0))
            })
            .collect()
    }

    /// PSNR in dB over R, G and B (mean squared error of all three channels, peak 255); 99 when identical.
    fn psnr(a: &[u32], b: &[u32]) -> f64 {
        assert_eq!(a.len(), b.len());
        let mut se = 0u64;
        for (&p, &q) in a.iter().zip(b) {
            for s in [0, 8, 16] {
                let d = u64::from(((p >> s) & 255).abs_diff((q >> s) & 255));
                se += d * d;
            }
        }
        if se == 0 {
            return 99.0;
        }
        let mse = se as f64 / (3 * a.len()) as f64;
        (10.0 * (255.0 * 255.0 / mse).log10()).min(99.0)
    }

    /// True when every 0xFF in the entropy-coded data is followed by a stuffed 0x00 and the file ends in EOI.
    fn stream_ok(bytes: &[u8]) -> bool {
        let sos = bytes.windows(2).position(|p| p == [0xFF, 0xDA]).unwrap_or(bytes.len());
        let data = bytes.get(sos + 14..bytes.len().saturating_sub(2)).unwrap_or(&[]);
        let stuffed = data.windows(2).all(|p| p[0] != 0xFF || p[1] == 0) && data.last() != Some(&0xFF);
        stuffed && bytes.ends_with(&[0xFF, 0xD9])
    }

    /// Encodes, checks the stream, decodes with the project's decoder and checks the size.
    /// Returns (file size, decoded pixels).
    fn encode_decode(px: &[u32], w: u32, h: u32, q: u8, s: Sampling, bg: u32) -> (usize, Vec<u32>) {
        let bytes = encode(px, w, h, q, s, bg).expect("encode");
        assert!(stream_ok(&bytes), "{w}x{h} q{q} {s:?}: bad byte stuffing or no EOI");
        let d = jpeg::decode(&bytes, u32::MAX).unwrap_or_else(|| panic!("{w}x{h} q{q} {s:?}: decoder returned None"));
        assert_eq!((d.w, d.h), (w, h), "decoded size");
        (bytes.len(), d.px)
    }

    /// Round trip; returns (file size, PSNR against the flattened input).
    fn round_trip(px: &[u32], w: u32, h: u32, q: u8, s: Sampling, bg: u32) -> (usize, f64) {
        let (n, out) = encode_decode(px, w, h, q, s, bg);
        (n, psnr(&out, &flatten(px, bg)))
    }

    // ---------------------------------------------------------------------------------------------
    // Synthetic pictures
    // ---------------------------------------------------------------------------------------------

    fn picture(w: u32, h: u32, f: impl Fn(f32, f32, u32, u32) -> u32) -> Vec<u32> {
        let mut v = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                v.push(f((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32, x, y));
            }
        }
        v
    }

    /// Smooth colour gradients, a few levels per pixel at most, so smooth at any picture size.
    fn gradient(w: u32, h: u32) -> Vec<u32> {
        picture(w, h, |_, _, x, y| {
            let (x, y) = (x as f32, y as f32);
            let r = 128.0 + 90.0 * (x / 23.0 + y / 61.0).sin();
            let g = 120.0 + 100.0 * (y / 29.0).cos();
            let b = 128.0 + 70.0 * ((x + y) / 37.0).sin();
            rgb(r as u32, g as u32, b as u32)
        })
    }

    /// Hard edges: black/white stripes, a saturated red disc, a blue square and a diagonal.
    fn edges(w: u32, h: u32) -> Vec<u32> {
        picture(w, h, |fx, fy, x, _| {
            let (dx, dy) = (fx - 0.3, fy - 0.6);
            if dx * dx + dy * dy < 0.04 {
                rgb(230, 20, 30)
            } else if fx > 0.6 && fy < 0.4 {
                rgb(20, 40, 220)
            } else if fy < 0.25 {
                if (x / 3) % 2 == 0 {
                    rgb(0, 0, 0)
                } else {
                    rgb(255, 255, 255)
                }
            } else if fx > fy {
                rgb(250, 240, 60)
            } else {
                rgb(40, 120, 40)
            }
        })
    }

    /// Uniform random colours.
    fn noise(w: u32, h: u32, seed: u64) -> Vec<u32> {
        let mut rng = Rng(seed | 1);
        (0..w * h).map(|_| 0xFF00_0000 | (rng.next() as u32 & 0xFF_FFFF)).collect()
    }

    fn solid(w: u32, h: u32, c: u32) -> Vec<u32> {
        vec![0xFF00_0000 | c; (w * h) as usize]
    }

    const SIZES: [(u32, u32); 11] =
        [(1, 1), (7, 3), (17, 9), (33, 65), (255, 1), (1, 255), (4096, 1), (1, 4096), (96, 96), (120, 90), (256, 256)];
    const QUALITIES: [u8; 5] = [1, 50, 85, 95, 100];

    // ---------------------------------------------------------------------------------------------
    // Tables and headers
    // ---------------------------------------------------------------------------------------------

    #[test]
    fn tables() {
        for (_, bits, vals) in HUFFMAN_TABLES {
            assert_eq!(bits.iter().map(|&b| usize::from(b)).sum::<usize>(), vals.len());
            assert_eq!(vals.iter().collect::<HashSet<_>>().len(), vals.len(), "duplicate symbols");
        }
        // Every symbol the coder can emit has a code: all (run, size) pairs, ZRL and EOB; DC sizes 0..=11.
        for ac in [Huffman::new(&AC_LUMA_BITS, &AC_LUMA_VALS), Huffman::new(&AC_CHROMA_BITS, &AC_CHROMA_VALS)] {
            for run in 0..16 {
                for size in 1..=10 {
                    assert!(ac.get(run << 4 | size).1 > 0);
                }
            }
            assert!(ac.get(0x00).1 > 0 && ac.get(0xF0).1 > 0);
        }
        for dc in [Huffman::new(&DC_LUMA_BITS, &DC_VALS), Huffman::new(&DC_CHROMA_BITS, &DC_VALS)] {
            assert!((0..=11).all(|s| dc.get(s).1 > 0));
        }
        // Quality scaling like libjpeg: 50 is the Annex K table, 100 all ones, 1 capped at 255; 0 and 255 clamp.
        assert_eq!(scaled_table(&LUMA_Q, 50), LUMA_Q);
        assert_eq!(scaled_table(&CHROMA_Q, 50), CHROMA_Q);
        assert!(scaled_table(&LUMA_Q, 100).iter().all(|&q| q == 1));
        assert!(scaled_table(&LUMA_Q, 1).iter().all(|&q| q == 255));
        assert_eq!(scaled_table(&LUMA_Q, 75)[0], 8); // (16 * 50 + 50) / 100
        assert_eq!(scaled_table(&LUMA_Q, 25)[0], 32); // (16 * 200 + 50) / 100
        assert_eq!(scaled_table(&CHROMA_Q, 10)[4], 255); // 99 * 500 / 100 = 495, capped for baseline
        let px = gradient(40, 30);
        assert_eq!(encode(&px, 40, 30, 0, Sampling::S420, 0), encode(&px, 40, 30, 1, Sampling::S420, 0));
        assert_eq!(encode(&px, 40, 30, 255, Sampling::S444, 0), encode(&px, 40, 30, 100, Sampling::S444, 0));
    }

    #[test]
    fn headers() {
        for (s, factors) in [(Sampling::S420, 0x22), (Sampling::S444, 0x11)] {
            let bytes = encode(&gradient(300, 200), 300, 200, 85, s, 0).unwrap();
            let mut i = 2;
            let mut markers = Vec::new();
            assert_eq!(bytes[..2], [0xFF, 0xD8]);
            while bytes[i] == 0xFF && bytes[i + 1] != 0xDA {
                let len = usize::from(bytes[i + 2]) << 8 | usize::from(bytes[i + 3]);
                markers.push(bytes[i + 1]);
                let seg = &bytes[i + 4..i + 2 + len];
                match bytes[i + 1] {
                    0xE0 => assert_eq!(&seg[..5], b"JFIF\0"),
                    0xDB => {
                        assert_eq!(seg.len(), 130);
                        let t = scaled_table(&LUMA_Q, 85);
                        assert!((0..64).all(|k| seg[1 + k] == t[usize::from(ZIGZAG[k])]), "DQT in zigzag order");
                    }
                    0xC0 => assert_eq!(seg, [8, 0, 200, 1, 44, 3, 1, factors, 0, 2, 0x11, 1, 3, 0x11, 1]),
                    _ => {}
                }
                i += 2 + len;
            }
            assert_eq!(markers, [0xE0, 0xDB, 0xC0, 0xC4]);
            assert_eq!(jpeg::dimensions(&bytes), Some((300, 200)));
            assert!(stream_ok(&bytes));
        }
    }

    // ---------------------------------------------------------------------------------------------
    // Round trips through the project's decoder
    // ---------------------------------------------------------------------------------------------

    #[test]
    fn psnr_table() {
        println!("\nPSNR (dB, RGB) by quality {QUALITIES:?}; bytes at quality 85");
        let mut noise_q85 = Vec::new();
        for (name, smooth) in [("gradient", true), ("solid", true), ("edges", false), ("noise", false)] {
            for &(w, h) in &SIZES {
                let px = match name {
                    "gradient" => gradient(w, h),
                    "solid" => solid(w, h, 0x3A7FC4),
                    "edges" => edges(w, h),
                    _ => noise(w, h, u64::from(w * 7919 + h)),
                };
                for s in [Sampling::S420, Sampling::S444] {
                    let r: Vec<(usize, f64)> = QUALITIES.iter().map(|&q| round_trip(&px, w, h, q, s, 0)).collect();
                    let cells: Vec<String> = r.iter().map(|&(_, p)| format!("{p:5.1}")).collect();
                    println!("{name:<9} {:>9} {s:?}  {}  {:>7} B", format!("{w}x{h}"), cells.join(" "), r[2].0);
                    if smooth {
                        for (q, &(_, p)) in QUALITIES.iter().zip(&r) {
                            assert!(*q < 85 || p >= 35.0, "{name} {w}x{h} {s:?} q{q}: {p:.1} dB");
                        }
                    } else if name == "noise" {
                        noise_q85.push(r[2].1);
                    }
                    assert!(r[4].1 >= r[1].1 - 0.5, "{name} {w}x{h} {s:?}: q100 worse than q50");
                }
            }
        }
        let mean = noise_q85.iter().sum::<f64>() / noise_q85.len() as f64;
        let min = noise_q85.iter().fold(99.0f64, |a, &b| a.min(b));
        println!("noise at q85: mean {mean:.1} dB, min {min:.1} dB");
    }

    #[test]
    fn solid_colours() {
        for c in [0x000000, 0xFFFFFF, 0xFF0000, 0x00FF00, 0x0000FF, 0x808080, 0x123456, 0xFEDCBA] {
            for &(w, h) in &[(1, 1), (17, 9), (64, 64)] {
                for s in [Sampling::S420, Sampling::S444] {
                    for q in [85, 100] {
                        let (_, p) = round_trip(&solid(w, h, c), w, h, q, s, 0);
                        assert!(p >= 35.0, "{c:06x} {w}x{h} {s:?} q{q}: {p:.1} dB");
                    }
                }
            }
        }
    }

    #[test]
    fn alpha_is_laid_on_background() {
        let mean = |px: &[u32], s: u32| px.iter().map(|&p| f64::from((p >> s) & 255)).sum::<f64>() / px.len() as f64;
        // Fully transparent (and invalid "transparent but coloured") pixels give the background.
        for (p, bg, want) in [(0u32, 0x336699, [0x33, 0x66, 0x99]), (0x00FF_FFFF, 0x808080, [255, 255, 255])] {
            let (_, out) = encode_decode(&[p; 24 * 24], 24, 24, 95, Sampling::S420, bg);
            for (s, want) in [16, 8, 0].into_iter().zip(want) {
                assert!((mean(&out, s) - f64::from(want)).abs() < 1.5, "{p:08x} on {bg:06x}");
            }
        }
        // Premultiplied half-transparent red on white: 128 + 255 * 127 / 255 = 255, 0 + 127, 0 + 127.
        let (_, out) = encode_decode(&[0x8080_0000; 16 * 16], 16, 16, 95, Sampling::S444, 0xFFFFFF);
        assert!((mean(&out, 16) - 255.0).abs() < 1.5 && (mean(&out, 8) - 127.0).abs() < 1.5);
        // A smooth alpha ramp over a gradient: compared with the test's own compositing.
        let px = picture(100, 80, |fx, fy, _, _| {
            let a = (255.0 * fx) as u32;
            let c = |v: f32| (v * a as f32 / 255.0) as u32;
            a << 24 | c(200.0 * fy + 40.0) << 16 | c(90.0) << 8 | c(255.0 * (1.0 - fy))
        });
        for s in [Sampling::S420, Sampling::S444] {
            let (_, p) = round_trip(&px, 100, 80, 85, s, 0x20C0E0);
            println!("alpha ramp on 0x20C0E0, {s:?}, q85: {p:.1} dB");
            assert!(p >= 35.0);
        }
    }

    // ---------------------------------------------------------------------------------------------
    // Real pictures: the wallpapers under C:\Windows\Web (read-only)
    // ---------------------------------------------------------------------------------------------

    /// *.jpg / *.jpeg paths below `dir`, sorted, without following links.
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            let Ok(md) = std::fs::symlink_metadata(&p) else { continue };
            let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if md.is_dir() {
                walk(&p, out);
            } else if md.is_file() && (ext == "jpg" || ext == "jpeg") {
                out.push(p);
            }
        }
    }

    /// A named picture: (name, width, height, opaque pixels).
    type Picture = (String, u32, u32, Vec<u32>);

    /// Each distinct wallpaper, decoded by the project's decoder at the smallest scale that is at least
    /// 256 px on its long side, as (name, w, h, px). Also returns the names the decoder rejected.
    fn wallpapers() -> (Vec<Picture>, Vec<String>) {
        let mut paths = Vec::new();
        walk(Path::new(r"C:\Windows\Web"), &mut paths);
        let (mut seen, mut ok, mut failed) = (HashSet::new(), Vec::new(), Vec::new());
        for p in paths {
            let Ok(data) = std::fs::read(&p) else { continue };
            if !seen.insert(data.clone()) {
                continue;
            }
            let name = p.display().to_string().replace(r"C:\Windows\Web\", "");
            match jpeg::decode(&data, 256) {
                Some(d) => ok.push((name, d.w, d.h, d.px)),
                None => failed.push(name),
            }
        }
        (ok, failed)
    }

    /// Box-average shrink to exactly tw x th.
    fn shrink(src: &[u32], sw: u32, sh: u32, tw: u32, th: u32) -> Vec<u32> {
        let span = |t: u32, s: u32, n: u32| {
            let a = (u64::from(t) * u64::from(s) / u64::from(n)) as u32;
            (a, ((u64::from(t + 1) * u64::from(s) / u64::from(n)) as u32).max(a + 1))
        };
        let mut out = Vec::with_capacity((tw * th) as usize);
        for ty in 0..th {
            let (y0, y1) = span(ty, sh, th);
            for tx in 0..tw {
                let (x0, x1) = span(tx, sw, tw);
                let (mut acc, mut n) = ([0u32; 3], 0u32);
                for y in y0..y1 {
                    for x in x0..x1 {
                        let p = src[(y * sw + x) as usize];
                        acc[0] += (p >> 16) & 255;
                        acc[1] += (p >> 8) & 255;
                        acc[2] += p & 255;
                        n += 1;
                    }
                }
                out.push(rgb((acc[0] + n / 2) / n, (acc[1] + n / 2) / n, (acc[2] + n / 2) / n));
            }
        }
        out
    }

    /// Size that fits inside `side` x `side` keeping the aspect ratio.
    fn fit(w: u32, h: u32, side: u32) -> (u32, u32) {
        let long = w.max(h);
        (((w * side + long / 2) / long).max(1), ((h * side + long / 2) / long).max(1))
    }

    /// Thumbnails of the wallpapers as dvfiles would make them: (name, fit side, w, h, pixels).
    fn wallpaper_thumbs() -> Vec<(String, u32, u32, u32, Vec<u32>)> {
        let (pics, failed) = wallpapers();
        println!(
            "\n{} distinct wallpapers decoded; the decoder returned None for {}: {failed:?}",
            pics.len(),
            failed.len()
        );
        assert!(!pics.is_empty());
        let mut out = Vec::new();
        for (name, w, h, px) in &pics {
            for side in [256, 120] {
                let (tw, th) = fit(*w, *h, side);
                out.push((name.clone(), side, tw, th, shrink(px, *w, *h, tw, th)));
            }
        }
        out
    }

    /// Quality 85 against Windows' own JPEG encoder at the same quality and sampling (both files decoded
    /// by the project's decoder), then a quality sweep. The 35 dB target is checked on the average: single
    /// detailed thumbnails stay below it at 4:2:0 with any encoder (chroma subsampling), and are listed.
    #[test]
    fn real_wallpapers() {
        let thumbs = wallpaper_thumbs();
        println!(
            "{:<44} {:>8} {:>6} {:>17} {:>17} {:>17} {:>17}",
            "", "", "", "4:2:0 ours", "4:2:0 WIC", "4:4:4 ours", "4:4:4 WIC"
        );
        println!(
            "{:<44} {:>8} {:>6} {:>17} {:>17} {:>17} {:>17}",
            "wallpaper", "size", "fit", "bytes   dB", "bytes   dB", "bytes   dB", "bytes   dB"
        );
        let mut low = Vec::new();
        for (name, side, w, h, px) in &thumbs {
            let mut line = format!("{name:<44} {:>8} {side:>6}", format!("{w}x{h}"));
            let mut p420 = 0.0;
            for s in [Sampling::S420, Sampling::S444] {
                let (n, p) = round_trip(px, *w, *h, 85, s, 0);
                let wic_bytes = wic::encode(px, *w, *h, 0.85, s == Sampling::S420).expect("WIC encode");
                let wic_px = jpeg::decode(&wic_bytes, u32::MAX).expect("decode WIC's file").px;
                let wp = psnr(&wic_px, px);
                line += &format!(" {n:>8} {p:>5.1}   {:>8} {wp:>5.1}", wic_bytes.len());
                assert!(p >= wp - 0.25, "{name} {side} {s:?}: {p:.2} dB vs WIC {wp:.2} dB");
                assert!(
                    n <= wic_bytes.len() * 103 / 100 + 64,
                    "{name} {side} {s:?}: {n} bytes vs WIC {}",
                    wic_bytes.len()
                );
                if s == Sampling::S420 {
                    p420 = p;
                } else if p420 < 35.0 {
                    low.push(format!("{name} {side} px: 4:2:0 {p420:.1} dB, 4:4:4 {p:.1} dB"));
                }
            }
            println!("{line}");
        }
        println!("\nquality sweep (bytes min / mean / max, PSNR mean / min, thumbnails under 35 dB):");
        for side in [256, 120] {
            for s in [Sampling::S420, Sampling::S444] {
                for q in [75, 85, 90, 95] {
                    let r: Vec<(usize, f64)> = thumbs
                        .iter()
                        .filter(|t| t.1 == side)
                        .map(|(_, _, w, h, px)| round_trip(px, *w, *h, q, s, 0))
                        .collect();
                    let bytes: Vec<usize> = r.iter().map(|x| x.0).collect();
                    let mean_p = r.iter().map(|x| x.1).sum::<f64>() / r.len() as f64;
                    let min_p = r.iter().map(|x| x.1).fold(99.0f64, f64::min);
                    let under = r.iter().filter(|x| x.1 < 35.0).count();
                    println!(
                        "  fit {side:>3} {s:?} q{q}: {:>6} / {:>6} / {:>6} B, {mean_p:.1} / {min_p:.1} dB, \
                         {under} of {} under 35 dB",
                        bytes.iter().min().unwrap(),
                        bytes.iter().sum::<usize>() / bytes.len(),
                        bytes.iter().max().unwrap(),
                        r.len()
                    );
                    if q == 85 {
                        assert!(mean_p >= 35.0, "fit {side} {s:?} q85: mean {mean_p:.1} dB");
                        assert!(min_p >= 27.0, "fit {side} {s:?} q85: min {min_p:.1} dB");
                    }
                }
            }
        }
        println!("\nq85 thumbnails under 35 dB at 4:2:0 ({}):", low.len());
        for l in &low {
            println!("  {l}");
        }
    }

    // ---------------------------------------------------------------------------------------------
    // Cross-check with Windows' own decoder
    // ---------------------------------------------------------------------------------------------

    /// Mean and max absolute B, G, R difference between our decode (0xAARRGGBB) and WIC's (BGRA bytes).
    fn errors(ours: &[u32], wic: &[u8]) -> (f64, u32) {
        assert_eq!(ours.len() * 4, wic.len());
        let (mut sum, mut max) = (0u64, 0u32);
        for (&p, w) in ours.iter().zip(wic.as_chunks::<4>().0) {
            for (c, s) in [(0usize, 0u32), (1, 8), (2, 16)] {
                let d = ((p >> s) & 255).abs_diff(u32::from(w[c]));
                sum += u64::from(d);
                max = max.max(d);
            }
        }
        (sum as f64 / (3 * ours.len().max(1)) as f64, max)
    }

    /// Every file we write must decode in WIC at the right size and match the project's decoder (mean
    /// difference under 2). One known exception: for 4:2:0 pictures at most 4 pixels wide (chroma at most
    /// 2 samples wide) libjpeg-style decoders such as WIC replicate chroma instead of filtering it (libjpeg's
    /// jdsample.c only uses "fancy" upsampling when the chroma width is above 2), while the project's
    /// decoder always filters. For those only the luma has to agree; their numbers are printed.
    #[test]
    fn wic_cross_check() {
        let mut cases: Vec<Picture> = Vec::new();
        for &(w, h) in
            &[(1, 1), (7, 3), (17, 9), (33, 65), (255, 1), (1, 255), (4, 64), (5, 64), (4096, 1), (120, 90), (256, 256)]
        {
            cases.push((format!("gradient {w}x{h}"), w, h, gradient(w, h)));
            cases.push((format!("edges {w}x{h}"), w, h, edges(w, h)));
            cases.push((format!("noise {w}x{h}"), w, h, noise(w, h, 99)));
        }
        // Saturated colour checks: the worst case for differences in chroma upsampling.
        let checks =
            picture(64, 48, |_, _, x, y| if (x / 2 + y / 2) % 2 == 0 { rgb(255, 0, 0) } else { rgb(0, 0, 255) });
        cases.push(("red/blue checks 64x48".into(), 64, 48, checks));
        let (pics, _) = wallpapers();
        for (name, w, h, px) in pics.iter().step_by(4) {
            for side in [256, 120] {
                let (tw, th) = fit(*w, *h, side);
                cases.push((format!("{name} {tw}x{th}"), tw, th, shrink(px, *w, *h, tw, th)));
            }
        }
        let (mut files, mut worst, mut worst_max, mut sum) = (0u32, (0.0f64, String::new()), 0u32, 0.0f64);
        let (mut over, mut narrow) = (Vec::new(), Vec::new());
        for (name, w, h, px) in &cases {
            for s in [Sampling::S420, Sampling::S444] {
                for q in [1, 50, 85, 100] {
                    let bytes = encode(px, *w, *h, q, s, 0).unwrap();
                    let (ww, wh, bgra) =
                        wic::decode(&bytes).unwrap_or_else(|| panic!("WIC rejected {name} {s:?} q{q}"));
                    assert_eq!((ww, wh), (*w, *h), "WIC size for {name}");
                    let ours = jpeg::decode(&bytes, u32::MAX).unwrap();
                    let (mean, max) = errors(&ours.px, &bgra);
                    files += 1;
                    if s == Sampling::S420 && *w <= 4 {
                        let wic_px: Vec<u32> = bgra
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .map(|c| rgb(c[2].into(), c[1].into(), c[0].into()))
                            .collect();
                        let (po, pw) = (psnr(&ours.px, px), psnr(&wic_px, px));
                        let luma = |p: u32| {
                            0.299 * ((p >> 16) & 255) as f64
                                + 0.587 * ((p >> 8) & 255) as f64
                                + 0.114 * (p & 255) as f64
                        };
                        let y_diff = ours.px.iter().zip(&wic_px).map(|(&a, &b)| (luma(a) - luma(b)).abs()).sum::<f64>()
                            / px.len() as f64;
                        narrow.push(format!(
                            "{name} q{q}: mean diff {mean:.2} (luma {y_diff:.2}); \
                             vs source ours {po:.1} dB, WIC {pw:.1} dB"
                        ));
                        assert!(y_diff < 2.0, "{name} q{q}: luma differs by {y_diff:.2}");
                        continue;
                    }
                    sum += mean;
                    worst_max = worst_max.max(max);
                    if mean > worst.0 {
                        worst = (mean, format!("{name} {s:?} q{q}"));
                    }
                    if mean >= 2.0 {
                        over.push(format!("{name} {s:?} q{q}: {mean:.2}"));
                    }
                }
            }
        }
        let compared = files - narrow.len() as u32;
        println!(
            "\nWIC decoded all {files} files at the right size. Over the {compared} compared: \
             mean |ours - WIC| average {:.3}, \
             worst {:.3} ({}), largest single difference {worst_max}",
            sum / f64::from(compared),
            worst.0,
            worst.1
        );
        println!("4:2:0 pictures at most 4 px wide ({}, chroma replicated by WIC):", narrow.len());
        for n in &narrow {
            println!("  {n}");
        }
        assert!(over.is_empty(), "mean error >= 2: {over:?}");
    }

    // ---------------------------------------------------------------------------------------------
    // Robustness (meant for the debug profile: overflow checks on)
    // ---------------------------------------------------------------------------------------------

    #[test]
    fn bad_arguments() {
        let px = vec![0xFF00_0000u32; 12];
        assert!(encode(&px, 4, 3, 85, Sampling::S420, 0).is_some());
        assert!(encode(&px, 0, 3, 85, Sampling::S420, 0).is_none());
        assert!(encode(&px, 4, 0, 85, Sampling::S420, 0).is_none());
        assert!(encode(&[], 0, 0, 85, Sampling::S420, 0).is_none());
        assert!(encode(&px[..11], 4, 3, 85, Sampling::S420, 0).is_none());
        assert!(encode(&[0; 13], 4, 3, 85, Sampling::S444, 0).is_none());
        assert!(encode(&px, 3, 3, 85, Sampling::S444, 0).is_none());
        assert!(encode(&px, u32::MAX, u32::MAX, 85, Sampling::S420, 0).is_none());
        assert!(encode(&px, 65536, 1, 85, Sampling::S420, 0).is_none());
        assert!(encode(&vec![0; 65536], 65536, 1, 85, Sampling::S420, 0).is_none(), "wider than JPEG allows");
        assert!(encode(&vec![0; 65535], 65535, 1, 85, Sampling::S420, 0).is_some());
    }

    #[test]
    fn fuzz_random_sizes_and_pixels() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let cases = 400;
        let (mut bytes, mut q0, mut q_over) = (0usize, 0, 0);
        for i in 0..cases {
            let (w, h) = (1 + rng.below(300) as u32, 1 + rng.below(300) as u32);
            let px: Vec<u32> = match i % 4 {
                0 => (0..w * h).map(|_| rng.next() as u32).collect(), // any alpha, often invalid premultiplied
                1 => (0..w * h).map(|_| [0, u32::MAX, 0x00FF_FFFF, 0xFF00_0000][rng.below(4) as usize]).collect(),
                2 => noise(w, h, rng.next()),
                _ => edges(w, h),
            };
            let q = rng.next() as u8;
            q0 += usize::from(q == 0);
            q_over += usize::from(q > 100);
            let s = if rng.next() & 1 == 0 { Sampling::S420 } else { Sampling::S444 };
            let (n, _) = encode_decode(&px, w, h, q, s, rng.next() as u32);
            bytes += n;
        }
        println!(
            "\nfuzz: {cases} random images 1..=300 px a side, all encoded and decoded at the right size \
             ({q0} with quality 0, {q_over} above 100), {bytes} bytes in total"
        );
    }

    #[test]
    fn largest_coefficients() {
        // Blocks shaped like each DCT basis function at full contrast give the largest coefficients there are.
        for u in 0..8u32 {
            for v in 0..8u32 {
                let px = picture(64, 32, |_, _, x, y| {
                    let c = |k: u32, t: u32| (((2 * (t % 8) + 1) * k) as f32 * std::f32::consts::PI / 16.0).cos();
                    let on = c(u, x) * c(v, y) >= 0.0;
                    let chroma = (x / 8 + y / 8) % 2 == 1;
                    match (on, chroma) {
                        (true, false) => rgb(255, 255, 255),
                        (false, false) => rgb(0, 0, 0),
                        (true, true) => rgb(255, 0, 255),
                        (false, true) => rgb(0, 255, 0),
                    }
                });
                for s in [Sampling::S420, Sampling::S444] {
                    for q in [1, 100] {
                        let (_, p) = round_trip(&px, 64, 32, q, s, 0);
                        assert!(q < 100 || s == Sampling::S420 || p >= 40.0, "basis {u},{v} {s:?}: {p:.1} dB");
                    }
                }
            }
        }
    }

    #[test]
    fn largest_size() {
        let (w, h) = (4096, 4096);
        let px = picture(w, h, |fx, fy, x, y| rgb((255.0 * fx) as u32, (255.0 * fy) as u32, (x ^ y) & 255));
        for s in [Sampling::S420, Sampling::S444] {
            let t = Instant::now();
            let bytes = encode(&px, w, h, 90, s, 0).unwrap();
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            let d = jpeg::decode(&bytes, u32::MAX).unwrap();
            assert_eq!((d.w, d.h), (w, h));
            println!("\n4096x4096 {s:?}: {} bytes, encoded in {ms:.0} ms, {:.1} dB", bytes.len(), psnr(&d.px, &px));
        }
    }

    // ---------------------------------------------------------------------------------------------
    // Speed (meaningful with --release)
    // ---------------------------------------------------------------------------------------------

    #[test]
    #[ignore = "timing; run: cargo test --release -- --ignored release_ --nocapture"]
    fn release_speed() {
        let (pics, _) = wallpapers();
        let runs = if cfg!(debug_assertions) { 3 } else { 200 };
        println!();
        for (tw, th) in [(256, 256), (120, 90)] {
            let thumbs: Vec<Vec<u32>> = pics.iter().take(6).map(|(_, w, h, px)| shrink(px, *w, *h, tw, th)).collect();
            for s in [Sampling::S420, Sampling::S444] {
                let mut times = Vec::new();
                for t in &thumbs {
                    let start = Instant::now();
                    for _ in 0..runs {
                        let out = encode(std::hint::black_box(t), tw, th, 85, s, 0);
                        std::hint::black_box(out);
                    }
                    times.push(start.elapsed().as_secs_f64() * 1000.0 / f64::from(runs));
                }
                let mean = times.iter().sum::<f64>() / times.len() as f64;
                let max = times.iter().fold(0.0f64, |a, &b| a.max(b));
                println!(
                    "speed {tw}x{th} {s:?}: mean {:.1} us per encode over {} photos x {runs} runs \
                     (slowest photo {:.1} us)",
                    mean * 1000.0,
                    thumbs.len(),
                    max * 1000.0
                );
                if !cfg!(debug_assertions) && tw == 256 && s == Sampling::S420 {
                    assert!(mean < 1.0, "256x256 took {mean:.3} ms");
                }
            }
        }
    }
}
