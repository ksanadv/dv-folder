// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Small, dependency-free JPEG decoder for thumbnails.
//!
//! Handles baseline / extended (SOF0, SOF1) and progressive (SOF2) Huffman JPEGs with 8-bit samples,
//! 1 component (gray) or 3 components (YCbCr, or RGB when an Adobe APP14 marker says so), sampling
//! factors 1..2, restart markers and any number of scans. It decodes at 1/1, 1/2, 1/4 or 1/8 of the
//! size with reduced inverse DCTs, applies the EXIF orientation, and reads a few EXIF fields plus the
//! EXIF thumbnail. Broken or unsupported input gives `None`; no input makes it panic.

use super::Decoded;

/// EXIF fields used by the file manager. `orientation` is 1..=8 (1 when the tag is missing or invalid).
pub struct Exif {
    pub orientation: u16,
    pub date_taken: Option<String>,
    pub make: Option<String>,
    pub model: Option<String>,
    pub pixel_size: Option<(u32, u32)>,
}

/// Image size from the SOF header, without decoding (also for files this decoder cannot decode, if the SOF is readable).
pub fn dimensions(data: &[u8]) -> Option<(u32, u32)> {
    for (m, a, b) in segments(data)? {
        if is_sof(m) {
            let s = data.get(a..b)?;
            let (h, w) = (be16(s, 1)?, be16(s, 3)?);
            if w == 0 || h == 0 {
                return None;
            }
            return Some((w as u32, h as u32));
        }
        if m == 0xDA || m == 0xD9 {
            return None;
        }
    }
    None
}

/// Decodes at the smallest of the scales 1/8, 1/4, 1/2, 1/1 whose result is still at least `min_side` pixels on its
/// longer side (or at 1/1 if none is). Applies the EXIF orientation (rotate/flip so the picture is upright; for
/// orientations 5..8 width and height swap). Returns None for unsupported or broken data.
pub fn decode(data: &[u8], min_side: u32) -> Option<Decoded> {
    let (w, h) = dimensions(data)?;
    let long = w.max(h);
    let denom = [8u32, 4, 2].into_iter().find(|&d| long.div_ceil(d) >= min_side).unwrap_or(1);
    let img = decode_unrotated(data, denom)?;
    let orientation = exif(data).map_or(1, |e| e.orientation);
    orient(img, orientation)
}

/// The EXIF block's fields above, if the file has one.
pub fn exif(data: &[u8]) -> Option<Exif> {
    let t = exif_tiff(data)?;
    let ifd0 = t.u32(4)? as usize;
    t.u16(ifd0)?; // IFD0 must at least be readable
    let orientation = t.uint(ifd0, 0x0112).filter(|o| (1..=8).contains(o)).unwrap_or(1) as u16;
    let sub = t.uint(ifd0, 0x8769).map(|o| o as usize);
    let size = sub.and_then(|e| Some((t.uint(e, 0xA002)?, t.uint(e, 0xA003)?)));
    Some(Exif {
        orientation,
        date_taken: sub.and_then(|e| t.ascii(e, 0x9003)),
        make: t.ascii(ifd0, 0x010F),
        model: t.ascii(ifd0, 0x0110),
        pixel_size: size.filter(|&(w, h)| w > 0 && h > 0),
    })
}

/// The small JPEG thumbnail a camera stores in the EXIF block (often 160x120), as a slice of `data`.
pub fn exif_thumbnail(data: &[u8]) -> Option<&[u8]> {
    let t = exif_tiff(data)?;
    let ifd0 = t.u32(4)? as usize;
    let count = usize::from(t.u16(ifd0)?);
    let ifd1 = t.u32(ifd0.checked_add(2 + 12 * count)?)? as usize;
    if ifd1 == 0 {
        return None;
    }
    let off = t.uint(ifd1, 0x0201)? as usize;
    let len = t.uint(ifd1, 0x0202)? as usize;
    let thumb = t.d.get(off..off.checked_add(len)?)?;
    if thumb.len() < 4 || thumb.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    Some(thumb)
}

// ------------------------------------------------------------------------------------------------
// Constants and small helpers
// ------------------------------------------------------------------------------------------------

/// Largest image (width * height) we agree to decode.
const MAX_PIXELS: u64 = 200_000_000;

/// Number of bits looked up at once when decoding Huffman codes.
const FAST_BITS: u32 = 10;

/// ZIGZAG[k] is the row-major position of the k-th coefficient in stream (zigzag) order.
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21,
    28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54,
    47, 55, 62, 63,
];

/// AAN IDCT scale factors: cos(k * pi / 16) * sqrt(2), and 1 for k = 0.
const AAN: [f32; 8] = [1.0, 1.387_039_8, 1.306_563, 1.175_875_6, 1.0, 0.785_694_96, 0.541_196_1, 0.275_899_38];

/// Allocates a vector of `len` copies of `value`, returning None instead of aborting when memory is short.
fn try_vec<T: Clone>(len: usize, value: T) -> Option<Vec<T>> {
    let mut v = Vec::new();
    v.try_reserve_exact(len).ok()?;
    v.resize(len, value);
    Some(v)
}

/// Reads a big-endian u16 at `i`.
fn be16(d: &[u8], i: usize) -> Option<usize> {
    Some(usize::from(*d.get(i)?) << 8 | usize::from(*d.get(i.checked_add(1)?)?))
}

/// Clamps to the i16 range (coefficients are stored as i16).
fn clamp16(v: i32) -> i16 {
    v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

/// True for every start-of-frame marker (all SOFn variants, supported or not).
fn is_sof(m: u8) -> bool {
    matches!(m, 0xC0..=0xCF) && !matches!(m, 0xC4 | 0xC8 | 0xCC)
}

// ------------------------------------------------------------------------------------------------
// Marker segments
// ------------------------------------------------------------------------------------------------

/// Finds the next marker at or after `pos` and returns (marker code, index after the two marker bytes).
/// Skips garbage, fill bytes (FF FF), stuffed zeros (FF 00) and restart markers on the way.
fn next_marker(d: &[u8], mut pos: usize) -> Option<(u8, usize)> {
    loop {
        if *d.get(pos)? != 0xFF {
            pos += 1;
            continue;
        }
        match *d.get(pos + 1)? {
            0xFF => pos += 1,
            0x00 | 0xD0..=0xD7 => pos += 2,
            m => return Some((m, pos + 2)),
        }
    }
}

/// Iterates over the marker segments of a JPEG as (marker, payload start, payload end).
/// Markers without a payload (SOI, EOI, TEM) come back with an empty range. After a scan
/// header the caller moves `pos` past the entropy-coded data (the iterator would also skip it).
struct Segments<'a> {
    d: &'a [u8],
    pos: usize,
}

impl Iterator for Segments<'_> {
    type Item = (u8, usize, usize);
    fn next(&mut self) -> Option<Self::Item> {
        let (m, p) = next_marker(self.d, self.pos)?;
        if matches!(m, 0xD8 | 0xD9 | 0x01) {
            self.pos = p;
            return Some((m, p, p));
        }
        let len = be16(self.d, p)?;
        let end = p.checked_add(len)?;
        if len < 2 || end > self.d.len() {
            self.pos = self.d.len();
            return None;
        }
        self.pos = end;
        Some((m, p + 2, end))
    }
}

/// Segment iterator for a file that starts with SOI.
fn segments(d: &[u8]) -> Option<Segments<'_>> {
    if d.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    Some(Segments { d, pos: 2 })
}

// ------------------------------------------------------------------------------------------------
// Huffman tables and the bit reader
// ------------------------------------------------------------------------------------------------

/// One Huffman table with a lookup table for short codes.
struct Huff {
    /// For each FAST_BITS-bit prefix: (code length << 8) | symbol, or 0 if the code is longer.
    fast: [u16; 1 << FAST_BITS],
    /// Largest code of each length (index 1..=16), or -1 when there is none.
    maxcode: [i32; 17],
    /// Symbol index = code + valoff[length].
    valoff: [i32; 17],
    vals: [u8; 256],
}

impl Huff {
    /// Builds a table from the 16 code-length counts and the symbols of a DHT segment.
    fn build(counts: &[u8; 16], syms: &[u8]) -> Option<Box<Huff>> {
        let mut h = Box::new(Huff { fast: [0; 1 << FAST_BITS], maxcode: [-1; 17], valoff: [0; 17], vals: [0; 256] });
        let total: usize = counts.iter().map(|&c| usize::from(c)).sum();
        if total > 256 || total > syms.len() {
            return None;
        }
        h.vals.get_mut(..total)?.copy_from_slice(syms.get(..total)?);
        let (mut code, mut k) = (0u32, 0usize);
        for len in 1..=16usize {
            let cnt = usize::from(counts[len - 1]);
            h.valoff[len] = k as i32 - code as i32;
            for _ in 0..cnt {
                if len <= FAST_BITS as usize {
                    // Every FAST_BITS-bit prefix that starts with this code maps to it.
                    let shift = FAST_BITS as usize - len;
                    let first = (code as usize) << shift;
                    let entry = (len as u16) << 8 | u16::from(*h.vals.get(k)?);
                    for e in h.fast.iter_mut().skip(first).take(1 << shift) {
                        *e = entry;
                    }
                }
                code += 1;
                k += 1;
            }
            if code > 1 << len {
                return None; // more codes than fit in this many bits
            }
            if cnt > 0 {
                h.maxcode[len] = code as i32 - 1;
            }
            code <<= 1;
        }
        Some(h)
    }
}

/// Turns `s` raw magnitude bits into a signed value (JPEG's EXTEND step), for 1 <= s <= 16.
/// Branch-free: when the top bit is clear the value is negative, v - (2^s - 1).
#[inline(always)]
fn extend(v: u32, s: u32) -> i32 {
    let v = v as i32;
    let negative = (v >> (s - 1)) - 1; // -1 when the top bit is clear, else 0
    v + (negative & (1 - (1 << s)))
}

/// Reads the entropy-coded bits of a scan, dropping stuffed zero bytes. When it reaches a marker or
/// the end of data it feeds zero bytes instead and counts them in `fake`, so callers can stop
/// once a scan clearly runs past its data.
struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    /// Bit accumulator; the next bit is bit 63.
    acc: u64,
    /// Number of valid bits in `acc`.
    n: u32,
    fake: u32,
}

impl<'a> Bits<'a> {
    fn new(d: &'a [u8], pos: usize) -> Self {
        Bits { d, pos, acc: 0, n: 0, fake: 0 }
    }

    /// Makes sure at least 32 bits are buffered: enough for one Huffman code plus its value bits.
    #[inline(always)]
    fn need32(&mut self) {
        if self.n < 32 {
            self.refill();
        }
    }

    /// Tops up the accumulator to more than 56 bits. Fast path: the next 8 bytes hold no 0xFF, so
    /// whole bytes can be copied as they are.
    #[inline(always)]
    fn refill(&mut self) {
        if self.fake == 0 {
            if let Some(chunk) = self.d.get(self.pos..).and_then(|r| r.first_chunk::<8>()) {
                let word = u64::from_be_bytes(*chunk);
                let inv = !word;
                let has_ff = inv.wrapping_sub(0x0101_0101_0101_0101) & !inv & 0x8080_8080_8080_8080 != 0;
                if !has_ff && self.n <= 56 {
                    let take = (64 - self.n) / 8;
                    let bits = take * 8;
                    self.acc |= (word >> (64 - bits)) << (64 - bits - self.n);
                    self.n += bits;
                    self.pos += take as usize;
                    return;
                }
            }
        }
        (self.pos, self.acc, self.n, self.fake) = refill_bytes(self.d, self.pos, self.acc, self.n, self.fake);
    }

    /// Takes `s` bits (0..=32) that are known to be buffered.
    #[inline(always)]
    fn take(&mut self, s: u32) -> u32 {
        let v = ((self.acc >> 32) >> (32 - s)) as u32;
        self.acc <<= s;
        self.n -= s;
        v
    }

    /// Drops `s` bits (0..=32) that are known to be buffered.
    #[inline(always)]
    fn skip(&mut self, s: u32) {
        self.acc <<= s;
        self.n -= s;
    }

    /// Reads `s` bits (0..=16) as an unsigned number.
    #[inline(always)]
    fn bits(&mut self, s: u32) -> u32 {
        self.need32();
        self.take(s)
    }

    /// Reads `s` magnitude bits and sign-extends them (0 bits give 0).
    #[inline(always)]
    fn receive(&mut self, s: u32) -> i32 {
        if s == 0 {
            0
        } else {
            extend(self.bits(s), s)
        }
    }

    /// Decodes one Huffman symbol.
    #[inline(always)]
    fn huff(&mut self, h: &Huff) -> Option<u8> {
        self.need32();
        self.sym(h)
    }

    /// Decodes one Huffman symbol when at least 16 bits are buffered.
    #[inline(always)]
    fn sym(&mut self, h: &Huff) -> Option<u8> {
        let e = h.fast[(self.acc >> (64 - FAST_BITS)) as usize];
        let (len, sym) = if e != 0 { (u32::from(e >> 8), e as u8) } else { long_code(self.acc, h)? };
        self.skip(len);
        Some(sym)
    }

    /// Handles a restart marker: drops buffered bits and moves past the next RSTn marker.
    #[inline(always)]
    fn restart(&mut self) -> Option<()> {
        *self = Bits::new(self.d, find_restart(self.d, self.pos)?);
        Some(())
    }
}

// The helpers below take and return plain values (no `&mut Bits`), so the reader's state can
// stay in registers in the decoding loops.

/// Byte-by-byte refill near markers and at the end of the data; returns the new (pos, acc, n, fake).
#[cold]
#[inline(never)]
fn refill_bytes(d: &[u8], mut pos: usize, mut acc: u64, mut n: u32, mut fake: u32) -> (usize, u64, u32, u32) {
    while n <= 56 {
        let mut byte = 0u64;
        if fake == 0 {
            match d.get(pos) {
                Some(&0xFF) if d.get(pos + 1) == Some(&0) => {
                    byte = 0xFF;
                    pos += 2;
                }
                Some(&0xFF) | None => fake = 1,
                Some(&b) => {
                    byte = u64::from(b);
                    pos += 1;
                }
            }
        } else {
            fake = fake.saturating_add(1);
        }
        acc |= byte << (56 - n);
        n += 8;
    }
    (pos, acc, n, fake)
}

/// Decodes a code longer than FAST_BITS bits from the top of `acc`: returns (length, symbol).
#[inline(never)]
fn long_code(acc: u64, h: &Huff) -> Option<(u32, u8)> {
    let code = (acc >> 48) as u32;
    for len in FAST_BITS + 1..=16 {
        let c = (code >> (16 - len)) as i32;
        if c <= h.maxcode[len as usize] {
            return Some((len, *h.vals.get(usize::try_from(c + h.valoff[len as usize]).ok()?)?));
        }
    }
    None
}

/// Finds the restart marker at or after `p` and returns the position just after it.
fn find_restart(d: &[u8], mut p: usize) -> Option<usize> {
    loop {
        if *d.get(p)? != 0xFF {
            p += 1;
            continue;
        }
        match *d.get(p + 1)? {
            0xD0..=0xD7 => return Some(p + 2),
            0xFF => p += 1,
            0x00 => p += 2,
            _ => return None, // another marker: the scan data is incomplete
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Block decoding
// ------------------------------------------------------------------------------------------------

/// Decodes one baseline block: the DC difference (added to `pred`) and the AC coefficients into
/// `blk` (row-major, quantized). With DC_ONLY the AC coefficients are read but dropped and `blk`
/// is not touched. Returns whether any AC coefficient was stored. (Kept out of line on purpose:
/// in a small function the bit reader's state stays in registers through the symbol loop.)
#[inline(never)]
fn baseline_block<const DC_ONLY: bool>(
    bits: &mut Bits,
    dc: &Huff,
    ac: &Huff,
    pred: &mut i32,
    blk: &mut [i32; 64],
) -> Option<bool> {
    bits.need32();
    let t = u32::from(bits.sym(dc)?);
    if t > 15 {
        return None;
    }
    let diff = if t == 0 { 0 } else { extend(bits.take(t), t) };
    *pred = pred.wrapping_add(diff);
    if !DC_ONLY {
        blk[0] = (*pred).clamp(i32::from(i16::MIN), i32::from(i16::MAX));
    }
    let mut any = false;
    let mut k = 1usize;
    while k < 64 {
        bits.need32();
        let rs = bits.sym(ac)?;
        let (r, s) = (usize::from(rs >> 4), u32::from(rs & 15));
        if s == 0 {
            if r != 15 {
                break; // end of block
            }
            k += 16; // run of 16 zeros
            continue;
        }
        k += r;
        if k > 63 {
            return None;
        }
        if DC_ONLY {
            bits.skip(s);
        } else {
            blk[ZIGZAG[k]] = extend(bits.take(s), s);
            any = true;
        }
        k += 1;
    }
    Some(any)
}

/// The sample value of a block that has only a DC coefficient.
#[inline(always)]
fn dc_sample(coef: i32, q0: u16) -> u8 {
    let dc = coef.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) * i32::from(q0); // fits: |dc| < 2^31
    (((dc + 4) >> 3) + 128).clamp(0, 255) as u8
}

/// Progressive DC scan, first pass: the DC difference, shifted by the point transform `al`.
#[inline(always)]
fn dc_first(bits: &mut Bits, dc: &Huff, pred: &mut i32, out: &mut i16, al: u8) -> Option<()> {
    let t = u32::from(bits.huff(dc)?);
    if t > 15 {
        return None;
    }
    *pred = pred.wrapping_add(bits.receive(t));
    *out = clamp16(pred.wrapping_shl(u32::from(al)));
    Some(())
}

/// Progressive AC scan, first pass over coefficients ss..=se of one block.
#[inline(never)]
fn ac_first(
    bits: &mut Bits,
    ac: &Huff,
    blk: &mut [i16; 64],
    ss: usize,
    se: usize,
    al: u8,
    eobrun: &mut u32,
) -> Option<()> {
    if *eobrun > 0 {
        *eobrun -= 1;
        return Some(());
    }
    let mut k = ss;
    while k <= se {
        let rs = bits.huff(ac)?;
        let (r, s) = (u32::from(rs >> 4), u32::from(rs & 15));
        if s == 0 {
            if r < 15 {
                // End of band, possibly for a run of following blocks too.
                *eobrun = (1u32 << r) - 1 + bits.bits(r);
                break;
            }
            k += 16;
            continue;
        }
        k += r as usize;
        let z = *ZIGZAG.get(k)?;
        blk[z] = clamp16(bits.receive(s).wrapping_shl(u32::from(al)));
        k += 1;
    }
    Some(())
}

/// Applies one correction bit to an already nonzero coefficient (progressive refinement).
#[inline(always)]
fn refine_coef(bits: &mut Bits, c: &mut i16, p1: i16) {
    if bits.bits(1) != 0 && *c & p1 == 0 {
        *c = if *c >= 0 { c.saturating_add(p1) } else { c.saturating_sub(p1) };
    }
}

/// Progressive AC scan, refinement pass over coefficients ss..=se (follows libjpeg's decode_mcu_AC_refine).
#[inline(never)]
fn ac_refine(
    bits: &mut Bits,
    ac: &Huff,
    blk: &mut [i16; 64],
    ss: usize,
    se: usize,
    al: u8,
    eobrun: &mut u32,
) -> Option<()> {
    let p1: i16 = 1 << al;
    let se = se.min(63);
    let mut k = ss;
    if *eobrun == 0 {
        while k <= se {
            let rs = bits.huff(ac)?;
            let (mut r, s) = (u32::from(rs >> 4), rs & 15);
            let mut val = 0i16;
            if s != 0 {
                val = if bits.bits(1) != 0 { p1 } else { -p1 };
            } else if r != 15 {
                *eobrun = (1u32 << r) + bits.bits(r);
                break;
            }
            // Skip `r` zero coefficients, refining the nonzero ones passed on the way.
            while k <= se {
                let z = ZIGZAG[k];
                if blk[z] != 0 {
                    refine_coef(bits, &mut blk[z], p1);
                } else {
                    if r == 0 {
                        break;
                    }
                    r -= 1;
                }
                k += 1;
            }
            if val != 0 && k <= se {
                blk[ZIGZAG[k]] = val;
            }
            k += 1;
        }
    }
    if *eobrun > 0 {
        // Inside an end-of-band run: only correction bits for coefficients that are already nonzero.
        while k <= se {
            let z = ZIGZAG[k];
            if blk[z] != 0 {
                refine_coef(bits, &mut blk[z], p1);
            }
            k += 1;
        }
        *eobrun -= 1;
    }
    Some(())
}

// ------------------------------------------------------------------------------------------------
// Inverse DCT: full 8x8 (AAN), scaled nx x ny outputs (box-averaged), DC only
// ------------------------------------------------------------------------------------------------

/// Dequantization table for the IDCT: with AAN scaling and the final 1/8 folded in for the full
/// 8x8 IDCT, plain quantizer values for the scaled ones.
fn float_table(q: &[u16; 64], aan: bool) -> [f32; 64] {
    let mut t = [0f32; 64];
    for (i, v) in t.iter_mut().enumerate() {
        *v = if aan { f32::from(q[i]) * AAN[i / 8] * AAN[i % 8] / 8.0 } else { f32::from(q[i]) };
    }
    t
}

/// Weights for the scaled IDCTs, one table per output size n = 1, 2, 4, 8 (index log2 n). Entry
/// [i][u] is the 8-point DCT basis function u averaged over output sample i's span of 8/n input
/// samples, so a scaled block equals the full IDCT box-filtered down (n = 8 is the plain IDCT).
fn idct_weights() -> [[[f32; 8]; 8]; 4] {
    let mut all = [[[0f32; 8]; 8]; 4];
    for (k, m) in all.iter_mut().enumerate() {
        let span = 8 >> k;
        for (i, row) in m.iter_mut().enumerate().take(1 << k) {
            for (u, w) in row.iter_mut().enumerate() {
                let cu = if u == 0 { std::f64::consts::FRAC_1_SQRT_2 } else { 1.0 };
                let sum: f64 = (i * span..(i + 1) * span)
                    .map(|x| (((2 * x + 1) * u) as f64 * std::f64::consts::PI / 16.0).cos())
                    .sum();
                let v = cu / 2.0 * sum / span as f64;
                *w = if v.abs() < 1e-9 { 0.0 } else { v as f32 };
            }
        }
    }
    all
}

/// Index of an output size (1, 2, 4, 8) in the weight tables.
fn size_index(n: usize) -> usize {
    (n.trailing_zeros() as usize).min(3)
}

/// Rounds a sample (already level-shifted by +128) to 0..=255.
fn to_u8(v: f32) -> u8 {
    (v + 0.5).clamp(0.0, 255.0) as u8
}

/// One 8-point AAN inverse DCT (the float algorithm from the IJG library).
#[inline(always)]
fn aan_1d(i: [f32; 8]) -> [f32; 8] {
    // Even part.
    let (t10, t11) = (i[0] + i[4], i[0] - i[4]);
    let t13 = i[2] + i[6];
    let t12 = (i[2] - i[6]) * std::f32::consts::SQRT_2 - t13;
    let (e0, e3, e1, e2) = (t10 + t13, t10 - t13, t11 + t12, t11 - t12);
    // Odd part.
    let (z13, z10) = (i[5] + i[3], i[5] - i[3]);
    let (z11, z12) = (i[1] + i[7], i[1] - i[7]);
    let o7 = z11 + z13;
    let o11 = (z11 - z13) * std::f32::consts::SQRT_2;
    let z5 = (z10 + z12) * 1.847_759;
    let o10 = 1.082_392_2 * z12 - z5;
    let o12 = -2.613_126 * z10 + z5;
    let o6 = o12 - o7;
    let o5 = o11 - o6;
    let o4 = o10 + o5;
    [e0 + o7, e1 + o6, e2 + o5, e3 - o4, e3 + o4, e2 - o5, e1 - o6, e0 - o7]
}

/// Full 8x8 inverse DCT of quantized coefficients `blk` with the AAN-scaled table `qf`:
/// columns first (a column with only a DC term is just copied down), then rows.
fn idct8(blk: &[i32; 64], qf: &[f32; 64], out: &mut [u8; 64]) {
    let mut f = [0f32; 64];
    for c in 0..8 {
        if (1..8).all(|r| blk[r * 8 + c] == 0) {
            let dc = blk[c] as f32 * qf[c];
            for r in 0..8 {
                f[r * 8 + c] = dc;
            }
            continue;
        }
        let col = aan_1d(std::array::from_fn(|r| blk[r * 8 + c] as f32 * qf[r * 8 + c]));
        for (r, v) in col.into_iter().enumerate() {
            f[r * 8 + c] = v;
        }
    }
    for r in 0..8 {
        let b = r * 8;
        let v = aan_1d([f[b], f[b + 1], f[b + 2], f[b + 3], f[b + 4], f[b + 5], f[b + 6], f[b + 7]]);
        for (c, v) in v.into_iter().enumerate() {
            out[b + c] = to_u8(v + 128.0);
        }
    }
}

/// Scaled inverse DCT to nx x ny samples (separable, with the weight tables) from dequantized coefficients.
fn idct_scaled(f: &[f32; 64], nx: usize, ny: usize, wx: &[[f32; 8]; 8], wy: &[[f32; 8]; 8], out: &mut [u8; 64]) {
    let mut tmp = [[0f32; 8]; 8]; // [coefficient row][output column]
    for (v, t) in tmp.iter_mut().enumerate() {
        let row = &f[v * 8..v * 8 + 8];
        if row.iter().all(|&x| x == 0.0) {
            continue;
        }
        for (j, tj) in t.iter_mut().enumerate().take(nx) {
            *tj = row.iter().zip(&wx[j]).map(|(a, b)| a * b).sum();
        }
    }
    for (i, w) in wy.iter().enumerate().take(ny) {
        for j in 0..nx.min(8) {
            let s: f32 = tmp.iter().zip(w).map(|(t, w)| w * t[j]).sum();
            if let Some(o) = out.get_mut(i * nx + j) {
                *o = to_u8(s + 128.0);
            }
        }
    }
}

/// Turns one block of quantized coefficients into nx x ny samples (row-major, first nx*ny bytes).
fn block_pixels(
    blk: &[i32; 64],
    ac: bool,
    q: &[u16; 64],
    qf: &[f32; 64],
    nx: usize,
    ny: usize,
    wts: &[[[f32; 8]; 8]; 4],
) -> [u8; 64] {
    let mut out = [0u8; 64];
    if !ac || nx * ny == 1 {
        if let Some(o) = out.get_mut(..nx * ny) {
            o.fill(dc_sample(blk[0], q[0])); // flat block: the DC term alone
        }
        return out;
    }
    let (wx, wy) = (&wts[size_index(nx)], &wts[size_index(ny)]);
    match (nx, ny) {
        (8, 8) => idct8(blk, qf, &mut out),
        // Small outputs (chroma at 1/8, 4:2:2 / 4:4:0 chroma at 1/4).
        (2, 2) => idct_sparse::<2, 2>(blk, qf, wx, wy, &mut out),
        (2, 1) => idct_sparse::<2, 1>(blk, qf, wx, wy, &mut out),
        (1, 2) => idct_sparse::<1, 2>(blk, qf, wx, wy, &mut out),
        (4, 2) => idct_sparse::<4, 2>(blk, qf, wx, wy, &mut out),
        (2, 4) => idct_sparse::<2, 4>(blk, qf, wx, wy, &mut out),
        _ => {
            let mut f = [0f32; 64];
            for ((d, &c), &w) in f.iter_mut().zip(blk).zip(qf) {
                *d = c as f32 * w;
            }
            idct_scaled(&f, nx, ny, wx, wy, &mut out);
        }
    }
    out
}

/// Scaled inverse DCT for small outputs (nx * ny <= 8, e.g. 2x2 chroma at 1/8): adds up the
/// contribution of each nonzero coefficient, which is cheaper than the separable passes there.
fn idct_sparse<const NX: usize, const NY: usize>(
    blk: &[i32; 64],
    qf: &[f32; 64],
    wx: &[[f32; 8]; 8],
    wy: &[[f32; 8]; 8],
    out: &mut [u8; 64],
) {
    let mut acc = [[0f32; NX]; NY];
    for (idx, (&c, &q)) in blk.iter().zip(qf).enumerate() {
        if c == 0 {
            continue;
        }
        let (f, v, u) = (c as f32 * q, idx / 8, idx % 8);
        for (row, w) in acc.iter_mut().zip(wy) {
            let a = f * w[v];
            for (s, w) in row.iter_mut().zip(wx) {
                *s += a * w[u];
            }
        }
    }
    for (o, &a) in out.iter_mut().zip(acc.iter().flatten()) {
        *o = to_u8(a + 128.0);
    }
}

/// Copies nx x ny samples into a component plane at block position (bx, by).
fn put_block(plane: &mut [u8], stride: usize, bx: usize, by: usize, nx: usize, ny: usize, px: &[u8; 64]) {
    for r in 0..ny {
        let off = (by * ny + r) * stride + bx * nx;
        if let (Some(dst), Some(src)) = (plane.get_mut(off..off + nx), px.get(r * nx..r * nx + nx)) {
            dst.copy_from_slice(src);
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Frame, scans and the decoder
// ------------------------------------------------------------------------------------------------

/// One image component (Y, Cb, Cr or gray) and its decoded samples.
struct Comp {
    id: u8,
    /// Sampling factors.
    h: usize,
    v: usize,
    /// Quantization table slot.
    tq: usize,
    /// Blocks stored per row / column (padded to whole MCUs).
    bw: usize,
    bh: usize,
    /// Blocks that hold image data; single-component scans walk only these.
    cbw: usize,
    cbh: usize,
    /// Samples each block turns into, per row / column. Subsampled chroma gets a larger IDCT
    /// output than luma when the image is scaled down, so it needs no upsampling then.
    nx: usize,
    ny: usize,
    /// Upsampling still needed after the IDCT (1 or 2), only for subsampled chroma at 1/1.
    ux: usize,
    uy: usize,
    /// Valid samples per row / column in `plane`.
    vw: usize,
    vh: usize,
    /// Huffman table slots chosen by the current scan.
    dc: usize,
    ac: usize,
    /// DC predictor.
    pred: i32,
    /// Quantization table, latched when the component's first scan starts.
    q: Option<[u16; 64]>,
    /// Decoded samples; row stride is bw * nx.
    plane: Vec<u8>,
    /// Progressive only: coefficients kept per block (64, or 1 when only the DC term is needed).
    kc: usize,
    coefs: Vec<i16>,
}

struct Decoder<'a> {
    d: &'a [u8],
    /// Output samples per luma block side: 8, 4, 2 or 1 (scale 1/1 .. 1/8).
    n: usize,
    w: usize,
    h: usize,
    mcux: usize,
    mcuy: usize,
    progressive: bool,
    comps: Vec<Comp>,
    qt: [Option<[u16; 64]>; 4],
    dc: [Option<Box<Huff>>; 4],
    ac: [Option<Box<Huff>>; 4],
    /// Restart interval in MCUs (0 = none).
    restart: usize,
    jfif: bool,
    adobe: Option<u8>,
    /// Number of scans decoded so far.
    scans: usize,
    /// Block visits left for all scans together (limits the work a file full of scans can cause).
    budget: usize,
    /// Weights for the scaled IDCTs.
    wts: [[[f32; 8]; 8]; 4],
}

/// Decodes at 1/denom of the size (denom = 1, 2, 4 or 8) without applying the EXIF orientation.
fn decode_unrotated(data: &[u8], denom: u32) -> Option<Decoded> {
    let n = match denom {
        1 => 8,
        2 => 4,
        4 => 2,
        8 => 1,
        _ => return None,
    };
    let mut dec = Decoder {
        d: data,
        n,
        w: 0,
        h: 0,
        mcux: 0,
        mcuy: 0,
        progressive: false,
        comps: Vec::new(),
        qt: [None; 4],
        dc: Default::default(),
        ac: Default::default(),
        restart: 0,
        jfif: false,
        adobe: None,
        scans: 0,
        budget: 0,
        wts: idct_weights(),
    };
    let mut segs = segments(data)?;
    while let Some((m, a, b)) = segs.next() {
        let s = data.get(a..b)?;
        match m {
            0xD9 => break,
            0xD8 => return None, // a second SOI: corrupt
            0xC0..=0xC2 => dec.read_sof(m == 0xC2, s)?,
            0xC4 => dec.read_dht(s)?,
            0xDB => dec.read_dqt(s)?,
            0xDD => dec.restart = be16(s, 0)?,
            0xDA => segs.pos = dec.read_sos(s, b)?,
            0xE0 => dec.jfif |= s.starts_with(b"JFIF\0"),
            0xEE if s.starts_with(b"Adobe") => dec.adobe = s.get(11).copied(),
            _ if is_sof(m) => return None, // lossless, hierarchical or arithmetic coding
            _ => {}
        }
    }
    dec.finish()
}

impl Decoder<'_> {
    /// Reads the frame header: size, components, sampling factors; allocates the planes.
    fn read_sof(&mut self, progressive: bool, s: &[u8]) -> Option<()> {
        if !self.comps.is_empty() || *s.first()? != 8 {
            return None; // second frame, or not 8-bit
        }
        let (h, w, nc) = (be16(s, 1)?, be16(s, 3)?, usize::from(*s.get(5)?));
        if w == 0 || h == 0 || (w as u64) * (h as u64) > MAX_PIXELS || (nc != 1 && nc != 3) {
            return None;
        }
        let mut comps: Vec<Comp> = Vec::with_capacity(nc);
        for i in 0..nc {
            let (id, hv, tq) = (*s.get(6 + 3 * i)?, *s.get(7 + 3 * i)?, usize::from(*s.get(8 + 3 * i)?));
            let (ch, cv) = (usize::from(hv >> 4), usize::from(hv & 15));
            if !(1..=2).contains(&ch) || !(1..=2).contains(&cv) || tq > 3 || comps.iter().any(|c| c.id == id) {
                return None;
            }
            comps.push(Comp {
                id,
                h: ch,
                v: cv,
                tq,
                bw: 0,
                bh: 0,
                cbw: 0,
                cbh: 0,
                nx: 0,
                ny: 0,
                ux: 1,
                uy: 1,
                vw: 0,
                vh: 0,
                dc: 0,
                ac: 0,
                pred: 0,
                q: None,
                plane: Vec::new(),
                kc: 64,
                coefs: Vec::new(),
            });
        }
        let hmax = comps.iter().map(|c| c.h).max()?;
        let vmax = comps.iter().map(|c| c.v).max()?;
        let (mcux, mcuy) = (w.div_ceil(8 * hmax), h.div_ceil(8 * vmax));
        let n = self.n;
        let mut blocks = 0usize;
        for c in comps.iter_mut() {
            let (cw, ch) = ((w * c.h).div_ceil(hmax), (h * c.v).div_ceil(vmax));
            c.cbw = cw.div_ceil(8);
            c.cbh = ch.div_ceil(8);
            c.bw = mcux * c.h;
            c.bh = mcuy * c.v;
            // Wanted samples per block side, e.g. 2 for 4:2:0 chroma at 1/8; above 8 the rest is upsampling.
            let (sx, sy) = (n * hmax / c.h, n * vmax / c.v);
            (c.nx, c.ny) = (sx.min(8), sy.min(8));
            (c.ux, c.uy) = (sx / c.nx, sy / c.ny);
            c.vw = (cw * c.nx).div_ceil(8);
            c.vh = (ch * c.ny).div_ceil(8);
            c.kc = if c.nx * c.ny == 1 { 1 } else { 64 };
            blocks = blocks.checked_add(c.cbw.checked_mul(c.cbh)?)?;
        }
        // Every block costs at least one bit, so a short file cannot describe a huge image.
        if blocks > self.d.len().saturating_mul(8) {
            return None;
        }
        for c in comps.iter_mut() {
            c.plane = try_vec((c.bw * c.nx).checked_mul(c.bh * c.ny)?, 128u8)?;
            if progressive {
                c.coefs = try_vec(c.bw.checked_mul(c.bh)?.checked_mul(c.kc)?, 0i16)?;
            }
        }
        (self.w, self.h, self.mcux, self.mcuy) = (w, h, mcux, mcuy);
        // Real files visit each block a handful of times (about 10 scans at most); allow 64.
        self.budget = blocks.saturating_mul(64);
        self.progressive = progressive;
        self.comps = comps;
        Some(())
    }

    /// Reads one or more Huffman tables.
    fn read_dht(&mut self, mut s: &[u8]) -> Option<()> {
        while let Some(&tc_th) = s.first() {
            let (tc, th) = (tc_th >> 4, usize::from(tc_th & 15));
            if tc > 1 || th > 3 {
                return None;
            }
            let counts: [u8; 16] = s.get(1..17)?.try_into().ok()?;
            let total: usize = counts.iter().map(|&c| usize::from(c)).sum();
            let table = Huff::build(&counts, s.get(17..17 + total)?)?;
            if tc == 0 {
                self.dc[th] = Some(table);
            } else {
                self.ac[th] = Some(table);
            }
            s = s.get(17 + total..)?;
        }
        Some(())
    }

    /// Reads one or more quantization tables (8- or 16-bit), stored in row-major order.
    fn read_dqt(&mut self, mut s: &[u8]) -> Option<()> {
        while let Some(&pq_tq) = s.first() {
            let (pq, tq) = (pq_tq >> 4, usize::from(pq_tq & 15));
            if pq > 1 || tq > 3 {
                return None;
            }
            let size = if pq == 0 { 64 } else { 128 };
            let body = s.get(1..1 + size)?;
            let mut q = [0u16; 64];
            for (k, &z) in ZIGZAG.iter().enumerate() {
                q[z] = if pq == 0 { u16::from(*body.get(k)?) } else { be16(body, 2 * k)? as u16 };
            }
            self.qt[tq] = Some(q);
            s = s.get(1 + size..)?;
        }
        Some(())
    }

    /// Reads a scan header and decodes the scan that starts at `start`. Returns where the scan data ends.
    fn read_sos(&mut self, s: &[u8], start: usize) -> Option<usize> {
        let ns = usize::from(*s.first()?);
        if self.comps.is_empty() || ns == 0 || ns > self.comps.len() {
            return None;
        }
        let mut sc: Vec<usize> = Vec::with_capacity(ns);
        for i in 0..ns {
            let (id, t) = (*s.get(1 + 2 * i)?, *s.get(2 + 2 * i)?);
            let ci = self.comps.iter().position(|c| c.id == id)?;
            if sc.contains(&ci) || t >> 4 > 3 || t & 15 > 3 {
                return None;
            }
            let c = self.comps.get_mut(ci)?;
            c.dc = usize::from(t >> 4);
            c.ac = usize::from(t & 15);
            sc.push(ci);
        }
        let (ss, se, a) = (usize::from(*s.get(1 + 2 * ns)?), usize::from(*s.get(2 + 2 * ns)?), *s.get(3 + 2 * ns)?);
        let (ah, al) = (a >> 4, a & 15);
        if ns > 1 {
            let units: usize = sc.iter().filter_map(|&ci| self.comps.get(ci)).map(|c| c.h * c.v).sum();
            if units > 10 {
                return None;
            }
        }
        if self.progressive {
            let bad_band = if ss == 0 { se != 0 } else { se < ss || se > 63 || ns != 1 };
            if bad_band || ah > 13 || al > 13 {
                return None;
            }
        }
        let need_dc = !self.progressive || (ss == 0 && ah == 0);
        let need_ac = !self.progressive || ss > 0;
        let mut dc_only = true;
        for &ci in &sc {
            let c = self.comps.get_mut(ci)?;
            if (need_dc && self.dc[c.dc].is_none()) || (need_ac && self.ac[c.ac].is_none()) {
                return None;
            }
            if c.q.is_none() {
                c.q = Some(self.qt[c.tq]?);
            }
            dc_only &= c.kc == 1;
        }
        if self.progressive && ss > 0 && dc_only {
            return Some(start); // AC detail of this component is not needed; the marker search skips the data
        }
        let visits = match sc.as_slice() {
            [ci] => self.comps.get(*ci).map_or(0, |c| c.cbw * c.cbh),
            _ => {
                self.mcux * self.mcuy * sc.iter().filter_map(|&ci| self.comps.get(ci)).map(|c| c.h * c.v).sum::<usize>()
            }
        };
        self.budget = self.budget.checked_sub(visits)?;
        let end = self.decode_scan(start, &sc, ss, se, ah, al)?;
        self.scans += 1;
        Some(end)
    }

    /// Decodes the entropy-coded data of one scan.
    fn decode_scan(&mut self, start: usize, sc: &[usize], ss: usize, se: usize, ah: u8, al: u8) -> Option<usize> {
        let Decoder { d, comps, dc, ac, restart, progressive, mcux, mcuy, wts, .. } = self;
        let (restart, progressive) = (*restart, *progressive);
        let single = sc.len() == 1;
        let first = comps.get(*sc.first()?)?;
        // A single-component scan has one block per MCU and covers only the blocks with image data.
        let (mx, my) = if single { (first.cbw, first.cbh) } else { (*mcux, *mcuy) };
        // Per scan component: its Huffman tables, quantizers and IDCT dequantization table.
        let mut parts = Vec::with_capacity(sc.len());
        for &ci in sc {
            let c = comps.get_mut(ci)?;
            let q = c.q?;
            c.pred = 0;
            let tables = (dc.get(c.dc)?.as_deref(), ac.get(c.ac)?.as_deref());
            parts.push((ci, tables, q, float_table(&q, c.nx == 8 && c.ny == 8)));
        }
        let mut bits = Bits::new(d, start);
        let mut eobrun = 0u32;
        let mut left = restart;
        let mut blk = [0i32; 64];
        for mcu_y in 0..my {
            for mcu_x in 0..mx {
                if restart > 0 {
                    if left == 0 {
                        bits.restart()?;
                        for &ci in sc {
                            comps.get_mut(ci)?.pred = 0;
                        }
                        eobrun = 0;
                        left = restart;
                    }
                    left -= 1;
                }
                for &(ci, (dct, act), ref q, ref qf) in &parts {
                    let c = comps.get_mut(ci)?;
                    let (bh, bv) = if single { (1, 1) } else { (c.h, c.v) };
                    let stride = c.bw * c.nx;
                    for yy in 0..bv {
                        for xx in 0..bh {
                            let (bx, by) = (mcu_x * bh + xx, mcu_y * bv + yy);
                            if !progressive {
                                if c.kc == 1 {
                                    // Only the DC term is needed (1/8 scale): one sample per block.
                                    baseline_block::<true>(&mut bits, dct?, act?, &mut c.pred, &mut blk)?;
                                    if let Some(p) = c.plane.get_mut(by * stride + bx) {
                                        *p = dc_sample(c.pred, q[0]);
                                    }
                                } else {
                                    let any = baseline_block::<false>(&mut bits, dct?, act?, &mut c.pred, &mut blk)?;
                                    let px = block_pixels(&blk, any, q, qf, c.nx, c.ny, wts);
                                    put_block(&mut c.plane, stride, bx, by, c.nx, c.ny, &px);
                                    blk = [0; 64];
                                }
                                continue;
                            }
                            let i = (by * c.bw + bx) * c.kc;
                            let coefs = c.coefs.get_mut(i..i + c.kc)?;
                            if ss == 0 {
                                let dc0 = coefs.first_mut()?;
                                if ah == 0 {
                                    dc_first(&mut bits, dct?, &mut c.pred, dc0, al)?;
                                } else if bits.bits(1) != 0 {
                                    *dc0 |= 1 << al;
                                }
                            } else {
                                let b64: &mut [i16; 64] = coefs.try_into().ok()?;
                                if ah == 0 {
                                    ac_first(&mut bits, act?, b64, ss, se, al, &mut eobrun)?;
                                } else {
                                    ac_refine(&mut bits, act?, b64, ss, se, al, &mut eobrun)?;
                                }
                            }
                        }
                    }
                }
                // More than a few bytes past the end of the data: truncated or corrupt.
                if bits.fake > 16 {
                    return None;
                }
            }
        }
        Some(bits.pos)
    }

    /// Progressive: turns the collected coefficients into samples. Then upsamples and converts to BGRA.
    fn finish(mut self) -> Option<Decoded> {
        if self.scans == 0 {
            return None;
        }
        if self.progressive {
            for c in self.comps.iter_mut() {
                let Some(q) = c.q else { continue }; // never scanned: stays gray
                let stride = c.bw * c.nx;
                if c.kc == 1 {
                    // DC only: one sample per block.
                    for (prow, crow) in c.plane.chunks_mut(stride.max(1)).zip(c.coefs.chunks(c.bw.max(1))).take(c.cbh) {
                        for (p, &k) in prow.iter_mut().zip(crow).take(c.cbw) {
                            *p = dc_sample(i32::from(k), q[0]);
                        }
                    }
                } else {
                    let qf = float_table(&q, c.nx == 8 && c.ny == 8);
                    let mut blk = [0i32; 64];
                    for by in 0..c.cbh {
                        for bx in 0..c.cbw {
                            let i = (by * c.bw + bx) * 64;
                            let src = c.coefs.get(i..i + 64)?;
                            for (d, &s) in blk.iter_mut().zip(src) {
                                *d = i32::from(s);
                            }
                            let any = src.iter().skip(1).any(|&v| v != 0);
                            let px = block_pixels(&blk, any, &q, &qf, c.nx, c.ny, &self.wts);
                            put_block(&mut c.plane, stride, bx, by, c.nx, c.ny, &px);
                        }
                    }
                }
                c.coefs = Vec::new();
            }
        }
        let n = self.n;
        let (ow, oh) = ((self.w * n).div_ceil(8), (self.h * n).div_ceil(8));
        if ow == 0 || oh == 0 {
            return None;
        }
        let mut px = try_vec(ow.checked_mul(oh)?, 0u32)?;
        let ids: Vec<u8> = self.comps.iter().map(|c| c.id).collect();
        let rgb = match self.adobe {
            Some(t) => t == 0,
            None => !self.jfif && ids == b"RGB",
        };
        let mut rows = Vec::with_capacity(self.comps.len());
        for _ in &self.comps {
            rows.push(try_vec(ow, 0u8)?);
        }
        let mut tmp = try_vec(self.comps.iter().map(|c| c.vw).max()?, 0u16)?;
        for (y, out) in px.chunks_exact_mut(ow).enumerate() {
            for (c, row) in self.comps.iter().zip(rows.iter_mut()) {
                upsample_row(c, y, &mut tmp, row)?;
            }
            match rows.as_slice() {
                [g] => {
                    for (o, &v) in out.iter_mut().zip(g) {
                        *o = 0xFF00_0000 | (u32::from(v) * 0x01_0101);
                    }
                }
                [r, g, b] if rgb => {
                    for (((o, &r), &g), &b) in out.iter_mut().zip(r).zip(g).zip(b) {
                        *o = 0xFF00_0000 | u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b);
                    }
                }
                [y, cb, cr] => {
                    for (((o, &y), &cb), &cr) in out.iter_mut().zip(y).zip(cb).zip(cr) {
                        *o = ycc_to_bgra(y, cb, cr);
                    }
                }
                _ => return None,
            }
        }
        Some(Decoded { w: ow as u32, h: oh as u32, px })
    }
}

/// YCbCr (JFIF, full range) to packed BGRA.
#[inline(always)]
fn ycc_to_bgra(y: u8, cb: u8, cr: u8) -> u32 {
    let (y, cb, cr) = (i32::from(y) << 16, i32::from(cb) - 128, i32::from(cr) - 128);
    let r = (y + 91_881 * cr + 32_768) >> 16;
    let g = (y - 22_554 * cb - 46_802 * cr + 32_768) >> 16;
    let b = (y + 116_130 * cb + 32_768) >> 16;
    0xFF00_0000 | (r.clamp(0, 255) as u32) << 16 | (g.clamp(0, 255) as u32) << 8 | b.clamp(0, 255) as u32
}

/// Produces output row `y` of one component at full output width, upsampling by the component's
/// ux x uy (1 or 2) with libjpeg-style "fancy" (triangle) filtering: weights 3/4 and 1/4 per direction.
fn upsample_row(c: &Comp, y: usize, tmp: &mut [u16], out: &mut [u8]) -> Option<()> {
    let (stride, vw, vh) = (c.bw * c.nx, c.vw, c.vh);
    let cy = (y / c.uy).min(vh.checked_sub(1)?);
    let row = c.plane.get(cy * stride..cy * stride + vw)?;
    if c.ux == 1 && c.uy == 1 {
        let len = out.len().min(vw);
        out.get_mut(..len)?.copy_from_slice(row.get(..len)?);
        return Some(());
    }
    // Vertical pass into tmp, scaled by 4.
    let tmp = tmp.get_mut(..vw)?;
    if c.uy == 2 {
        let ny = if y & 1 == 0 { cy.saturating_sub(1) } else { (cy + 1).min(vh - 1) };
        let other = c.plane.get(ny * stride..ny * stride + vw)?;
        for ((t, &a), &b) in tmp.iter_mut().zip(row).zip(other) {
            *t = 3 * u16::from(a) + u16::from(b);
        }
    } else {
        for (t, &a) in tmp.iter_mut().zip(row) {
            *t = 4 * u16::from(a);
        }
    }
    // Horizontal pass.
    if c.ux == 1 {
        for (o, &t) in out.iter_mut().zip(tmp.iter()) {
            *o = ((t + 2) >> 2) as u8;
        }
        return Some(());
    }
    // Doubling: out[2i] = (3 t[i] + t[i-1] + 8) / 16, out[2i+1] = (3 t[i] + t[i+1] + 7) / 16.
    // Inner samples first, then the two edge samples (which repeat themselves as neighbours).
    let last = vw - 1;
    for i in 1..last.min(out.len() / 2) {
        let cur = 3 * tmp[i];
        out[2 * i] = ((cur + tmp[i - 1] + 8) >> 4) as u8;
        out[2 * i + 1] = ((cur + tmp[i + 1] + 7) >> 4) as u8;
    }
    for i in [0, last] {
        let cur = 3 * tmp[i];
        let (l, r) = (tmp[i.saturating_sub(1)], tmp[(i + 1).min(last)]);
        if let Some(p) = out.get_mut(2 * i) {
            *p = ((cur + l + 8) >> 4) as u8;
        }
        if let Some(p) = out.get_mut(2 * i + 1) {
            *p = ((cur + r + 7) >> 4) as u8;
        }
    }
    Some(())
}

/// Rotates / flips the image so EXIF orientation `o` (1..=8) becomes upright.
fn orient(img: Decoded, o: u16) -> Option<Decoded> {
    if !(2..=8).contains(&o) {
        return Some(img);
    }
    let (w, h) = (img.w as isize, img.h as isize);
    // Source index of destination pixel (x, y) = base + x * dx + y * dy.
    let (base, dx, dy) = match o {
        2 => (w - 1, -1, w),
        3 => ((h - 1) * w + w - 1, -1, -w),
        4 => ((h - 1) * w, 1, -w),
        5 => (0, w, 1),
        6 => ((h - 1) * w, -w, 1),
        7 => ((h - 1) * w + w - 1, -w, -1),
        _ => (w - 1, w, -1),
    };
    let (nw, nh) = if o >= 5 { (h, w) } else { (w, h) };
    let mut px = Vec::new();
    px.try_reserve_exact(img.px.len()).ok()?;
    for y in 0..nh {
        for x in 0..nw {
            let i = usize::try_from(base + x * dx + y * dy).ok()?;
            px.push(*img.px.get(i)?);
        }
    }
    Some(Decoded { w: nw as u32, h: nh as u32, px })
}

// ------------------------------------------------------------------------------------------------
// EXIF
// ------------------------------------------------------------------------------------------------

/// The TIFF structure inside an Exif APP1 segment; offsets are relative to its start.
struct Tiff<'a> {
    d: &'a [u8],
    le: bool,
}

impl Tiff<'_> {
    fn u16(&self, at: usize) -> Option<u16> {
        let b: [u8; 2] = self.d.get(at..at.checked_add(2)?)?.try_into().ok()?;
        Some(if self.le { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) })
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let b: [u8; 4] = self.d.get(at..at.checked_add(4)?)?.try_into().ok()?;
        Some(if self.le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    }

    /// Finds `tag` in the IFD at `ifd`: returns (type, count, position of the 4-byte value field).
    fn find(&self, ifd: usize, tag: u16) -> Option<(u16, u32, usize)> {
        let count = usize::from(self.u16(ifd)?);
        for i in 0..count {
            let e = ifd.checked_add(2 + 12 * i)?;
            if self.u16(e)? == tag {
                return Some((self.u16(e + 2)?, self.u32(e + 4)?, e + 8));
            }
        }
        None
    }

    /// A SHORT or LONG value (SHORTs sit in the first two bytes of the value field).
    fn uint(&self, ifd: usize, tag: u16) -> Option<u32> {
        match self.find(ifd, tag)? {
            (3, c, at) if c >= 1 => self.u16(at).map(u32::from),
            (4, c, at) if c >= 1 => self.u32(at),
            _ => None,
        }
    }

    /// An ASCII value, cut at the first NUL, trimmed and capped at 256 bytes.
    fn ascii(&self, ifd: usize, tag: u16) -> Option<String> {
        let (typ, count, at) = self.find(ifd, tag)?;
        if typ != 2 && typ != 7 {
            return None;
        }
        let count = count as usize;
        let start = if count <= 4 { at } else { self.u32(at)? as usize };
        let bytes = self.d.get(start..start.checked_add(count)?)?;
        let bytes = bytes.split(|&b| b == 0).next()?;
        let s = String::from_utf8_lossy(bytes.get(..bytes.len().min(256))?).trim().to_string();
        (!s.is_empty()).then_some(s)
    }
}

/// Locates the first APP1 segment starting with "Exif\0" and its TIFF header.
fn exif_tiff(data: &[u8]) -> Option<Tiff<'_>> {
    for (m, a, b) in segments(data)? {
        match m {
            0xE1 => {
                let p = data.get(a..b)?;
                if !p.starts_with(b"Exif\0") {
                    continue;
                }
                let d = p.get(6..)?;
                let le = match d.get(..4)? {
                    [b'I', b'I', 42, 0] => true,
                    [b'M', b'M', 0, 42] => false,
                    _ => return None,
                };
                return Some(Tiff { d, le });
            }
            0xDA | 0xD9 => return None,
            _ => {}
        }
    }
    None
}

// ------------------------------------------------------------------------------------------------
// Tests
// ------------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;

    /// Windows Imaging Component: the reference decoder and the encoder for synthetic test images.
    mod wic {
        use std::path::Path;
        use windows::core::{HSTRING, PWSTR};
        use windows::Win32::Foundation::GENERIC_WRITE;
        use windows::Win32::Graphics::Imaging::*;
        use windows::Win32::System::Com::StructuredStorage::{IPropertyBag2, PROPBAG2};
        use windows::Win32::System::Com::{
            CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
        };
        use windows::Win32::System::Variant::{VARIANT, VT_R4, VT_UI1};

        pub struct Img {
            pub w: u32,
            pub h: u32,
            pub bgra: Vec<u8>,
        }

        fn factory() -> Option<IWICImagingFactory> {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()
            }
        }

        /// Decodes an image held in memory to 32bpp BGRA at full size (no orientation applied).
        pub fn decode(bytes: &[u8]) -> Option<Img> {
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
                Some(Img { w, h, bgra })
            }
        }

        /// Encodes 24bpp BGR (or 8bpp gray) pixels to a JPEG file with the given quality (0..1) and
        /// chroma subsampling (WICJpegYCrCbSubsamplingOption: 1 = 4:2:0, 2 = 4:2:2, 3 = 4:4:4, 4 = 4:4:0).
        pub fn encode(path: &Path, w: u32, h: u32, px: &[u8], gray: bool, quality: f32, sub: i32) -> Option<()> {
            unsafe {
                let f = factory()?;
                let enc = f.CreateEncoder(&GUID_ContainerFormatJpeg, std::ptr::null()).ok()?;
                let stream = f.CreateStream().ok()?;
                stream.InitializeFromFilename(&HSTRING::from(path.to_string_lossy().as_ref()), GENERIC_WRITE.0).ok()?;
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
                (*vars[1].Anonymous.Anonymous).Anonymous.bVal = sub as u8;
                bag.Write(if gray { 1 } else { 2 }, props.as_ptr(), vars.as_ptr()).ok()?;
                frame.Initialize(&bag).ok()?;
                frame.SetSize(w, h).ok()?;
                let want = if gray { GUID_WICPixelFormat8bppGray } else { GUID_WICPixelFormat24bppBGR };
                let mut fmt = want;
                frame.SetPixelFormat(&mut fmt).ok()?;
                if fmt != want {
                    return None;
                }
                let bpp = if gray { 1 } else { 3 };
                frame.WritePixels(h, w * bpp, px).ok()?;
                frame.Commit().ok()?;
                enc.Commit().ok()?;
                Some(())
            }
        }
    }

    // ---------------------------------------------------------------------------------------------
    // Samples
    // ---------------------------------------------------------------------------------------------

    struct Sample {
        name: String,
        group: &'static str,
        data: Vec<u8>,
    }

    impl Clone for Sample {
        fn clone(&self) -> Self {
            Sample { name: self.name.clone(), group: self.group, data: self.data.clone() }
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
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n.max(1) as u64) as usize
        }
    }

    fn fnv(d: &[u8]) -> u64 {
        d.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
    }

    /// Collects *.jpg / *.jpeg paths below `dir` (read-only, no symlinks), at most `limit`.
    fn walk(dir: &Path, out: &mut Vec<PathBuf>, limit: usize) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut paths: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            if out.len() >= limit {
                return;
            }
            let Ok(md) = std::fs::symlink_metadata(&p) else { continue };
            let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if md.is_dir() {
                walk(&p, out, limit);
            } else if md.is_file() && (ext == "jpg" || ext == "jpeg") {
                out.push(p);
            }
        }
    }

    /// Every JPEG under C:\Windows\Web and up to 40 under C:\Windows\SystemApps, without duplicates.
    fn system_samples() -> Vec<Sample> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for (dir, group, limit) in
            [(r"C:\Windows\Web", "web", usize::MAX), (r"C:\Windows\SystemApps", "systemapps", 40)]
        {
            let mut paths = Vec::new();
            walk(Path::new(dir), &mut paths, limit);
            for p in paths {
                let Ok(data) = std::fs::read(&p) else { continue };
                if seen.insert(fnv(&data)) {
                    let name = p.display().to_string().replace(r"C:\Windows\", "");
                    out.push(Sample { name, group, data });
                }
            }
        }
        out
    }

    /// Synthetic BGR picture: smooth gradients, a saturated disc, blue stripes and optional noise.
    fn synth(w: u32, h: u32, noise: i32, seed: u64) -> Vec<u8> {
        let mut rng = Rng(seed | 1);
        let mut px = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
                let mut c = [255.0 * (1.0 - fx) * (0.5 + 0.5 * (fy * 6.0).sin()), 255.0 * fy, 255.0 * fx];
                let (dx, dy) = (fx - 0.35, fy - 0.6);
                if dx * dx + dy * dy < 0.04 {
                    c = [40.0, 30.0, 230.0];
                }
                if fx > 0.6 && fx < 0.9 && fy > 0.1 && fy < 0.4 && (x / 4) % 2 == 0 {
                    c = [200.0, 20.0, 20.0];
                }
                for v in c {
                    let n = if noise > 0 { (rng.next() % (2 * noise as u64 + 1)) as i32 - noise } else { 0 };
                    px.push((v as i32 + n).clamp(0, 255) as u8);
                }
            }
        }
        px
    }

    /// Folder for the generated test JPEGs: inside the cargo target directory (the test binary lives
    /// in <target>/<profile>/deps), so running the tests never writes into the source tree.
    fn testdata() -> PathBuf {
        let exe = std::env::current_exe().unwrap_or_default();
        let target = exe.ancestors().nth(3).map(Path::to_path_buf);
        target.unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target")).join("jpeg_testdata")
    }

    /// Name, width, height, gray, quality, WIC subsampling option, noise amplitude.
    const GENS: [(&str, u32, u32, bool, f32, i32, i32); 13] = [
        ("g444_q90_333x251", 333, 251, false, 0.90, 3, 4),
        ("g422_q90_333x251", 333, 251, false, 0.90, 2, 4),
        ("g420_q90_333x251", 333, 251, false, 0.90, 1, 4),
        ("g440_q90_333x251", 333, 251, false, 0.90, 4, 4),
        ("gray_q90_333x251", 333, 251, true, 0.90, 0, 4),
        ("g420_q50_1001x751", 1001, 751, false, 0.50, 1, 8),
        ("g422_q98_640x480", 640, 480, false, 0.98, 2, 2),
        ("g444_q75_17x9", 17, 9, false, 0.75, 3, 0),
        ("g420_q75_17x9", 17, 9, false, 0.75, 1, 0),
        ("g440_q75_9x17", 9, 17, false, 0.75, 4, 0),
        ("g420_q90_1x1", 1, 1, false, 0.90, 1, 0),
        ("gray_q60_7x13", 7, 13, true, 0.60, 0, 0),
        ("g420_q90_32x24_thumb", 32, 24, false, 0.90, 1, 0),
    ];
    /// The synthetic "camera photo" used for timing: 12 MP, 4:2:0, quality 0.9, noisy.
    const BIG: (&str, u32, u32, bool, f32, i32, i32) = ("photo_g420_q90_4000x3000", 4000, 3000, false, 0.90, 1, 12);

    /// Writes (once) and loads the WIC-encoded synthetic samples, plus an Adobe RGB variant.
    fn generated() -> &'static Vec<Sample> {
        static GEN: OnceLock<Vec<Sample>> = OnceLock::new();
        GEN.get_or_init(|| {
            std::fs::create_dir_all(testdata()).unwrap();
            let mut out = Vec::new();
            for (i, &(name, w, h, gray, q, sub, noise)) in GENS.iter().chain(std::iter::once(&BIG)).enumerate() {
                let path = testdata().join(format!("{name}.jpg"));
                if !path.exists() {
                    let mut px = synth(w, h, noise, 1234 + i as u64);
                    if gray {
                        px = px
                            .chunks(3)
                            .map(|c| ((u32::from(c[0]) + 2 * u32::from(c[1]) + u32::from(c[2])) / 4) as u8)
                            .collect();
                    }
                    wic::encode(&path, w, h, &px, gray, q, sub).expect("WIC encode");
                }
                out.push(Sample { name: name.to_string(), group: "generated", data: std::fs::read(&path).unwrap() });
            }
            // RGB JPEG: the 4:4:4 file with its JFIF APP0 replaced by an Adobe APP14 marker (transform 0).
            let src = &out[0].data;
            let mut rest = &src[2..];
            if rest[..2] == [0xFF, 0xE0] {
                let len = usize::from(rest[2]) << 8 | usize::from(rest[3]);
                rest = &rest[2 + len..];
            }
            let mut rgb = vec![0xFF, 0xD8, 0xFF, 0xEE, 0x00, 0x0E];
            rgb.extend_from_slice(b"Adobe\x00\x64\x00\x00\x00\x00\x00");
            rgb.extend_from_slice(rest);
            out.push(Sample { name: "adobe_rgb_444_333x251".into(), group: "generated", data: rgb });
            out
        })
    }

    /// "SOF2 22,11,11" style summary of the frame header.
    fn sof_info(d: &[u8]) -> String {
        for (m, a, b) in segments(d).into_iter().flatten() {
            if is_sof(m) {
                let s = &d[a..b];
                let nc = usize::from(s[5]);
                let samp: Vec<String> = (0..nc).filter_map(|i| s.get(7 + 3 * i)).map(|v| format!("{:x}", v)).collect();
                return format!("SOF{:X} {}", m - 0xC0, samp.join(","));
            }
        }
        "no SOF".into()
    }

    /// Runs `f` over `items` on `threads` threads, keeping the input order.
    fn par_map<T: Sync, R: Send>(items: &[T], threads: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
        let next = AtomicUsize::new(0);
        let results = Mutex::new(Vec::new());
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else { break };
                    let r = f(item);
                    results.lock().unwrap().push((i, r));
                });
            }
        });
        let mut v = results.into_inner().unwrap();
        v.sort_by_key(|p| p.0);
        v.into_iter().map(|p| p.1).collect()
    }

    // ---------------------------------------------------------------------------------------------
    // Correctness against WIC (run with --release)
    // ---------------------------------------------------------------------------------------------

    /// Mean and max absolute per-channel (B, G, R) difference.
    fn errors(mine: &[u32], wic: &[u8]) -> (f64, u32) {
        assert_eq!(mine.len() * 4, wic.len());
        let (mut sum, mut max) = (0u64, 0u32);
        for (i, &p) in mine.iter().enumerate() {
            for (c, shift) in [(0usize, 0u32), (1, 8), (2, 16)] {
                let d = ((p >> shift) & 255).abs_diff(u32::from(wic[i * 4 + c]));
                sum += u64::from(d);
                max = max.max(d);
            }
        }
        (sum as f64 / (3 * mine.len().max(1)) as f64, max)
    }

    /// WIC image reduced by `d` with a d x d box filter (output size rounded up). Boxes that stick out
    /// past the right / bottom edge average only the in-bounds pixels, or with `clamp` repeat the edge
    /// pixels (the way JPEG encoders pad partial blocks, which is what a scaled IDCT then sees).
    fn box_down(img: &wic::Img, d: u32, clamp: bool) -> (u32, u32, Vec<u8>) {
        let (w, h) = (img.w.div_ceil(d), img.h.div_ceil(d));
        let mut out = vec![0u8; (w * h * 4) as usize];
        let (ex, ey) = if clamp { (u32::MAX, u32::MAX) } else { (img.w, img.h) };
        for y in 0..h {
            for x in 0..w {
                let (mut acc, mut cnt) = ([0u32; 4], 0u32);
                for sy in y * d..((y + 1) * d).min(ey) {
                    for sx in x * d..((x + 1) * d).min(ex) {
                        let i = ((sy.min(img.h - 1) * img.w + sx.min(img.w - 1)) * 4) as usize;
                        for (c, a) in acc.iter_mut().enumerate() {
                            *a += u32::from(img.bgra[i + c]);
                        }
                        cnt += 1;
                    }
                }
                for c in 0..4 {
                    out[((y * w + x) * 4) as usize + c] = ((acc[c] + cnt / 2) / cnt) as u8;
                }
            }
        }
        (w, h, out)
    }

    struct Row {
        group: &'static str,
        name: String,
        info: String,
        size: (u32, u32),
        full: (f64, u32),
        /// Mean error at 1/2, 1/4, 1/8 against the clamp-to-edge box reference, and against in-bounds only.
        scaled: [f64; 3],
        scaled_inb: [f64; 3],
        problem: Option<String>,
    }

    /// Decodes one sample with WIC and with us at 1/1, 1/2, 1/4, 1/8 and measures the differences.
    fn compare(s: &Sample) -> Row {
        let mut row = Row {
            group: s.group,
            name: s.name.clone(),
            info: sof_info(&s.data),
            size: (0, 0),
            full: (0.0, 0),
            scaled: [0.0; 3],
            scaled_inb: [0.0; 3],
            problem: None,
        };
        let Some(w) = wic::decode(&s.data) else {
            row.problem = Some("WIC could not decode".into());
            return row;
        };
        row.size = (w.w, w.h);
        assert_eq!(dimensions(&s.data), Some((w.w, w.h)), "{}: dimensions", s.name);
        let Some(m) = decode_unrotated(&s.data, 1) else {
            row.problem = Some("our decoder returned None".into());
            return row;
        };
        if (m.w, m.h) != (w.w, w.h) {
            row.problem = Some(format!("size {}x{} vs WIC {}x{}", m.w, m.h, w.w, w.h));
            return row;
        }
        row.full = errors(&m.px, &w.bgra);
        for (i, d) in [2u32, 4, 8].into_iter().enumerate() {
            let (bw, bh, boxed) = box_down(&w, d, true);
            let (_, _, inb) = box_down(&w, d, false);
            match decode_unrotated(&s.data, d) {
                Some(m) if (m.w, m.h) == (bw, bh) => {
                    row.scaled[i] = errors(&m.px, &boxed).0;
                    row.scaled_inb[i] = errors(&m.px, &inb).0;
                }
                Some(m) => row.problem = Some(format!("1/{d}: size {}x{} vs {}x{}", m.w, m.h, bw, bh)),
                None => row.problem = Some(format!("1/{d}: None")),
            }
        }
        row
    }

    #[test]
    #[ignore = "slow; run: cargo test --release -- --ignored release_ --nocapture"]
    fn release_wic_compare() {
        let mut samples = system_samples();
        samples.extend(generated().iter().cloned());
        let t0 = Instant::now();
        let rows = par_map(&samples, 8, compare);
        println!(
            "\n{:<11} {:<70} {:<14} {:>10} {:>6} {:>4} | {:>6} {:>6} {:>6} | {:>6} {:>6} {:>6}",
            "group", "sample", "frame", "size", "mean", "max", "1/2", "1/4", "1/8", "1/2 ib", "1/4 ib", "1/8 ib"
        );
        let mut failures = Vec::new();
        for r in &rows {
            let short: String = r.name.chars().rev().take(70).collect::<Vec<_>>().into_iter().rev().collect();
            println!(
                "{:<11} {:<70} {:<14} {:>10} {:>6.3} {:>4} | {:>6.3} {:>6.3} {:>6.3} | {:>6.3} {:>6.3} {:>6.3} {}",
                r.group,
                short,
                r.info,
                format!("{}x{}", r.size.0, r.size.1),
                r.full.0,
                r.full.1,
                r.scaled[0],
                r.scaled[1],
                r.scaled[2],
                r.scaled_inb[0],
                r.scaled_inb[1],
                r.scaled_inb[2],
                r.problem.as_deref().unwrap_or("")
            );
            // Scaled limits apply unless the picture is tiny (under 64 px on a side): there nearly every
            // block is a partial edge block or sits on a chroma boundary. Thumbnail-sized min_side values
            // never scale such pictures down (decode() picks 1/1 for them). Their numbers are still printed.
            let tiny = r.size.0 < 64 || r.size.1 < 64;
            let scaled_bad = r.scaled.iter().chain(&r.scaled_inb).any(|&e| e >= 6.0);
            let bad = r.problem.is_some() || r.full.0 >= 3.0 || r.full.1 >= 40 || (!tiny && scaled_bad);
            if bad {
                failures.push(r.name.clone());
            }
        }
        for g in ["web", "systemapps", "generated"] {
            let rs: Vec<&Row> = rows.iter().filter(|r| r.group == g && r.problem.is_none()).collect();
            if rs.is_empty() {
                continue;
            }
            let avg = rs.iter().map(|r| r.full.0).sum::<f64>() / rs.len() as f64;
            let worst_mean = rs.iter().map(|r| r.full.0).fold(0.0, f64::max);
            let worst_max = rs.iter().map(|r| r.full.1).max().unwrap();
            let worst = |f: &dyn Fn(&Row) -> f64| format!("{:.3}", rs.iter().map(|r| f(r)).fold(0.0, f64::max));
            println!(
                "GROUP {g}: {} samples; 1/1 mean error: average {avg:.3}, worst {worst_mean:.3}; worst max error {worst_max}; \
                 worst scaled mean (1/2, 1/4, 1/8) clamp-to-edge box: {}, {}, {}; in-bounds box: {}, {}, {}",
                rs.len(),
                worst(&|r| r.scaled[0]),
                worst(&|r| r.scaled[1]),
                worst(&|r| r.scaled[2]),
                worst(&|r| r.scaled_inb[0]),
                worst(&|r| r.scaled_inb[1]),
                worst(&|r| r.scaled_inb[2]),
            );
        }
        println!("compared {} samples in {:.1} s", rows.len(), t0.elapsed().as_secs_f64());
        assert!(failures.is_empty(), "samples over the error limits: {failures:?}");
    }

    // ---------------------------------------------------------------------------------------------
    // Robustness: truncated and bit-flipped copies (run in the debug profile: overflow checks on)
    // ---------------------------------------------------------------------------------------------

    /// Calls every public function on `d`; returns false if any of them panicked.
    fn survives(d: &[u8]) -> bool {
        std::panic::catch_unwind(|| {
            let _ = dimensions(d);
            let _ = exif(d).map(|e| (e.orientation, e.date_taken, e.make, e.model, e.pixel_size));
            let _ = exif_thumbnail(d);
            let _ = decode(d, u32::MAX);
            let _ = decode(d, 0);
            let _ = decode_unrotated(d, 2);
            let _ = decode_unrotated(d, 4);
        })
        .is_ok()
    }

    #[test]
    #[ignore = "slow (minutes, hours at opt-level 0); run in a debug build: cargo test -- --ignored fuzz_ --nocapture"]
    fn fuzz_truncate_and_flip() {
        let mut samples = system_samples();
        samples.extend(generated().iter().filter(|g| !g.name.starts_with("photo_")).cloned());
        samples.sort_by_key(|s| std::cmp::Reverse(s.data.len())); // big ones first for better balance
        let cases = AtomicUsize::new(0);
        let failures = Mutex::new(Vec::new());
        let t0 = Instant::now();
        par_map(&samples, 8, |s| {
            let d = &s.data;
            assert!(decode(d, 0).is_some(), "{}: the intact file must decode", s.name);
            // Truncated copies at 200 evenly spaced lengths.
            for i in 0..200 {
                let len = d.len() * i / 200;
                if !survives(&d[..len]) {
                    failures.lock().unwrap().push(format!("{} truncated to {len}", s.name));
                }
            }
            // Where the headers are: everything before the first scan's data, and later marker segments.
            let mut header_end = d.len();
            let mut marks = Vec::new();
            for (m, a, b) in segments(d).into_iter().flatten() {
                marks.push(a.saturating_sub(4));
                if m == 0xDA && header_end == d.len() {
                    header_end = b;
                }
            }
            // 500 single-byte flips: half in the headers, a quarter around markers, a quarter anywhere.
            let mut rng = Rng(fnv(s.name.as_bytes()) | 1);
            let mut buf = d.clone();
            for j in 0..500 {
                let pos = match j % 4 {
                    0 | 1 => rng.below((header_end + 64).min(d.len())),
                    2 => (marks[rng.below(marks.len())] + rng.below(64)).saturating_sub(32).min(d.len() - 1),
                    _ => rng.below(d.len()),
                };
                let x = 1 + rng.below(255) as u8;
                buf[pos] ^= x;
                if !survives(&buf) {
                    failures.lock().unwrap().push(format!("{} byte {pos} ^= {x:#04x}", s.name));
                }
                buf[pos] ^= x;
            }
            cases.fetch_add(700, Ordering::Relaxed);
        });
        let failures = failures.into_inner().unwrap();
        // Proof that overflow checks are compiled in: this addition must panic (its message is silenced).
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let overflow_checks = std::panic::catch_unwind(|| std::hint::black_box(255u8) + 1).is_err();
        std::panic::set_hook(hook);
        println!(
            "fuzz: {} samples, {} corrupted files x 7 calls (dimensions, exif, exif_thumbnail, decode at 1/1, 1/2, 1/4, 1/8), {:.1} s, debug_assertions={}, overflow checks={}",
            samples.len(),
            cases.load(Ordering::Relaxed),
            t0.elapsed().as_secs_f64(),
            cfg!(debug_assertions),
            overflow_checks
        );
        assert!(failures.is_empty(), "panics: {failures:?}");
    }

    // ---------------------------------------------------------------------------------------------
    // Speed (run with --release)
    // ---------------------------------------------------------------------------------------------

    /// Best of `runs` timings of `f`, in milliseconds.
    fn best_ms(runs: usize, mut f: impl FnMut()) -> f64 {
        (0..runs)
            .map(|_| {
                let t = Instant::now();
                f();
                t.elapsed().as_secs_f64() * 1000.0
            })
            .fold(f64::MAX, f64::min)
    }

    #[test]
    #[ignore = "timing; run: cargo test --release -- --ignored release_speed --nocapture --test-threads 1"]
    fn release_speed() {
        let samples = system_samples();
        let mut picks: Vec<&Sample> = Vec::new();
        let px = |s: &Sample| dimensions(&s.data).map_or(0, |(w, h)| u64::from(w) * u64::from(h));
        picks.extend(samples.iter().max_by_key(|s| px(s)));
        for (w, h) in [(1920, 1080), (1920, 1200)] {
            picks.extend(samples.iter().find(|s| dimensions(&s.data) == Some((w, h))));
        }
        picks.extend(samples.iter().filter(|s| sof_info(&s.data).starts_with("SOF2")).max_by_key(|s| px(s)));
        picks.push(generated().iter().find(|g| g.name.starts_with("photo_")).unwrap());
        println!();
        for s in picks {
            let (w, h) = dimensions(&s.data).unwrap();
            let full = best_ms(5, || assert!(decode(&s.data, u32::MAX).is_some()));
            let small = best_ms(5, || assert!(decode(&s.data, 200).is_some()));
            let wic_ms = best_ms(3, || assert!(wic::decode(&s.data).is_some()));
            let t = decode(&s.data, 200).unwrap();
            let short: String = s.name.chars().rev().take(60).collect::<Vec<_>>().into_iter().rev().collect();
            println!(
                "{short:<60} {w:>5}x{h:<5} {:<14} {:>5} KB | 1/1: {full:>6.1} ms | min_side=200 -> {}x{}: {small:>5.1} ms | WIC full decode to BGRA: {wic_ms:>6.1} ms",
                sof_info(&s.data),
                s.data.len() / 1024,
                t.w,
                t.h,
            );
        }
    }

    // ---------------------------------------------------------------------------------------------
    // EXIF and orientation
    // ---------------------------------------------------------------------------------------------

    #[test]
    fn exif_on_samples() {
        let (mut with_exif, mut with_thumb) = (0, 0);
        for s in system_samples() {
            let Some(e) = exif(&s.data) else { continue };
            with_exif += 1;
            let thumb = exif_thumbnail(&s.data);
            println!(
                "{}: orientation {} date {:?} make {:?} model {:?} pixels {:?} thumbnail {:?}",
                s.name,
                e.orientation,
                e.date_taken,
                e.make,
                e.model,
                e.pixel_size,
                thumb.map(|t| t.len())
            );
            if let Some(t) = thumb {
                with_thumb += 1;
                assert!(decode(t, 0).is_some(), "{}: EXIF thumbnail should decode", s.name);
            }
        }
        println!("{with_exif} samples with EXIF, {with_thumb} with an EXIF thumbnail");
        assert!(with_exif > 0);
    }

    /// Builds an Exif APP1 segment by hand from the TIFF/EXIF layout. Offsets from the TIFF header:
    /// 0 header, 8 IFD0 (4 entries), 62 IFD1 (2 entries), 92 Exif IFD (3 entries), 134 Make text,
    /// 146 DateTimeOriginal text, 166 thumbnail.
    fn build_exif(le: bool, orientation: u16, thumb: &[u8]) -> Vec<u8> {
        let w16 = |v: u16| if le { v.to_le_bytes() } else { v.to_be_bytes() };
        let w32 = |v: u32| if le { v.to_le_bytes() } else { v.to_be_bytes() };
        let short = |v: u16| {
            let b = w16(v);
            [b[0], b[1], 0, 0] // SHORT values are left-justified in the 4-byte field
        };
        let entry = |t: &mut Vec<u8>, tag: u16, typ: u16, count: u32, value: [u8; 4]| {
            t.extend(w16(tag));
            t.extend(w16(typ));
            t.extend(w32(count));
            t.extend(value);
        };
        let mut t: Vec<u8> = Vec::new();
        t.extend(if le { *b"II" } else { *b"MM" });
        t.extend(w16(42));
        t.extend(w32(8));
        t.extend(w16(4)); // IFD0
        entry(&mut t, 0x010F, 2, 12, w32(134)); // Make, 12 bytes at 134
        entry(&mut t, 0x0110, 2, 4, *b"X10\0"); // Model, inline
        entry(&mut t, 0x0112, 3, 1, short(orientation));
        entry(&mut t, 0x8769, 4, 1, w32(92)); // Exif IFD pointer
        t.extend(w32(62)); // next IFD = IFD1
        assert_eq!(t.len(), 62);
        t.extend(w16(2)); // IFD1
        entry(&mut t, 0x0201, 4, 1, w32(166));
        entry(&mut t, 0x0202, 4, 1, w32(thumb.len() as u32));
        t.extend(w32(0));
        assert_eq!(t.len(), 92);
        t.extend(w16(3)); // Exif IFD
        entry(&mut t, 0x9003, 2, 20, w32(146));
        entry(&mut t, 0xA002, 3, 1, short(640));
        entry(&mut t, 0xA003, 4, 1, w32(480));
        t.extend(w32(0));
        assert_eq!(t.len(), 134);
        t.extend_from_slice(b"TestMake Co\0");
        t.extend_from_slice(b"2021:03:04 05:06:07\0");
        assert_eq!(t.len(), 166);
        t.extend_from_slice(thumb);
        let mut seg = vec![0xFF, 0xE1];
        seg.extend(((t.len() + 8) as u16).to_be_bytes());
        seg.extend_from_slice(b"Exif\0\0");
        seg.extend(t);
        seg
    }

    /// `jpeg` with an APP1 segment inserted right after SOI.
    fn with_app1(jpeg: &[u8], app1: &[u8]) -> Vec<u8> {
        let mut v = jpeg[..2].to_vec();
        v.extend_from_slice(app1);
        v.extend_from_slice(&jpeg[2..]);
        v
    }

    #[test]
    fn the_stored_thumbnail_needs_only_the_files_start() {
        // a photo whose own small picture is big enough is made from its first bytes, the rest never read
        let gen = generated();
        let thumb = &gen.iter().find(|g| g.name.ends_with("_thumb")).unwrap().data;
        let base = &gen.iter().find(|g| g.name == "g420_q90_333x251").unwrap().data;
        let app1 = build_exif(true, 1, thumb);
        let file = with_app1(base, &app1);
        let head = &file[..2 + app1.len()];
        let t = decode(thumb, u32::MAX).unwrap();
        let side = t.w.max(t.h);
        let small = crate::img::stored_thumbnail(head, side, side).expect("from the start of the file alone");
        assert_eq!((small.w, small.h), (t.w, t.h));
        // a box bigger than it needs the whole picture
        assert!(crate::img::stored_thumbnail(head, side + 1, side + 1).is_none());
        assert!(crate::img::stored_thumbnail(base, side, side).is_none());
    }

    #[test]
    fn exif_synthetic_and_orientation() {
        let gen = generated();
        let thumb = &gen.iter().find(|g| g.name.ends_with("_thumb")).unwrap().data;
        let base = &gen.iter().find(|g| g.name == "g420_q90_333x251").unwrap().data;
        // The big-endian orientation entry written out from the spec: tag 0112, type 3, count 1, value 6 left-justified.
        let be = build_exif(false, 6, thumb);
        let want = [0x01, 0x12, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01, 0x00, 0x06, 0x00, 0x00];
        assert!(be.windows(12).any(|w| w == want), "hand-built big-endian entry");
        let raw = decode_unrotated(base, 1).unwrap();
        let (w, h) = (raw.w as usize, raw.h as usize);
        let at = |img: &Decoded, x: usize, y: usize| img.px[y * img.w as usize + x];
        for le in [true, false] {
            for o in 1..=8u16 {
                let file = with_app1(base, &build_exif(le, o, thumb));
                let e = exif(&file).expect("exif");
                assert_eq!(e.orientation, o);
                assert_eq!(e.make.as_deref(), Some("TestMake Co"));
                assert_eq!(e.model.as_deref(), Some("X10"));
                assert_eq!(e.date_taken.as_deref(), Some("2021:03:04 05:06:07"));
                assert_eq!(e.pixel_size, Some((640, 480)));
                assert_eq!(exif_thumbnail(&file), Some(&thumb[..]));
                let img = decode(&file, u32::MAX).unwrap();
                let swapped = o >= 5;
                assert_eq!((img.w as usize, img.h as usize), if swapped { (h, w) } else { (w, h) }, "orientation {o}");
                // Where the stored image's corners must end up on screen.
                let (tl, tr, bl, br) = (at(&raw, 0, 0), at(&raw, w - 1, 0), at(&raw, 0, h - 1), at(&raw, w - 1, h - 1));
                let (nw, nh) = (img.w as usize, img.h as usize);
                let shown = [at(&img, 0, 0), at(&img, nw - 1, 0), at(&img, 0, nh - 1), at(&img, nw - 1, nh - 1)];
                let expect = match o {
                    1 => [tl, tr, bl, br],
                    2 => [tr, tl, br, bl],
                    3 => [br, bl, tr, tl],
                    4 => [bl, br, tl, tr],
                    5 => [tl, bl, tr, br],
                    6 => [bl, tl, br, tr],
                    7 => [br, tr, bl, tl],
                    _ => [tr, br, tl, bl],
                };
                assert_eq!(shown, expect, "corners for orientation {o} (le={le})");
            }
        }
        // Orientation 6 in full: displayed pixel (x, y) is stored pixel (y, h - 1 - x).
        let img = decode(&with_app1(base, &build_exif(true, 6, thumb)), u32::MAX).unwrap();
        for y in 0..img.h as usize {
            for x in 0..img.w as usize {
                assert_eq!(at(&img, x, y), at(&raw, y, h - 1 - x));
            }
        }
        // A scaled decode is rotated too (333x251 at 1/8 is 42x32).
        let small = decode(&with_app1(base, &build_exif(false, 6, thumb)), 0).unwrap();
        assert_eq!((small.w, small.h), (32, 42));
    }

    // ---------------------------------------------------------------------------------------------
    // Smaller checks
    // ---------------------------------------------------------------------------------------------

    #[test]
    fn scale_selection() {
        let d = &generated().iter().find(|g| g.name == "g420_q50_1001x751").unwrap().data;
        let size = |min: u32| decode(d, min).map(|i| (i.w, i.h)).unwrap();
        assert_eq!(size(0), (126, 94));
        assert_eq!(size(126), (126, 94));
        assert_eq!(size(127), (251, 188));
        assert_eq!(size(251), (251, 188));
        assert_eq!(size(252), (501, 376));
        assert_eq!(size(501), (501, 376));
        assert_eq!(size(502), (1001, 751));
        assert_eq!(size(u32::MAX), (1001, 751));
    }

    #[test]
    fn unsupported_variants_return_none() {
        let d = &generated().iter().find(|g| g.name == "g444_q90_333x251").unwrap().data;
        let (_, a, _) = segments(d).unwrap().find(|s| s.0 == 0xC0).unwrap();
        for code in [0xC3u8, 0xC5, 0xC9, 0xCA, 0xCB] {
            let mut v = d.clone();
            v[a - 3] = code; // the marker byte of SOF0
            assert_eq!(dimensions(&v), Some((333, 251)), "SOF{code:X} size still readable");
            assert!(decode(&v, 0).is_none(), "SOF{code:X} must be rejected");
        }
        let mut v = d.clone();
        v[a] = 12; // 12-bit precision
        assert!(decode(&v, 0).is_none());
        assert!(decode(&[], 0).is_none() && dimensions(&[]).is_none() && exif(&[]).is_none());
        assert!(decode(&d[1..], 0).is_none());
    }

    /// A progressive file whose last scan is repeated 1000 times must be refused quickly (work cap).
    #[test]
    fn many_scans_are_capped() {
        let samples = system_samples();
        let Some(s) = samples.iter().filter(|s| sof_info(&s.data).starts_with("SOF2")).min_by_key(|s| s.data.len())
        else {
            return;
        };
        let d = &s.data;
        let last_sos = segments(d).unwrap().filter(|seg| seg.0 == 0xDA).last().unwrap().1 - 4;
        let eoi = d.len() - 2;
        assert_eq!(d[eoi..], [0xFF, 0xD9]);
        let mut bomb = d[..eoi].to_vec();
        for _ in 0..1000 {
            bomb.extend_from_slice(&d[last_sos..eoi]);
        }
        bomb.extend_from_slice(&[0xFF, 0xD9]);
        let t = Instant::now();
        assert!(decode(&bomb, u32::MAX).is_none());
        println!(
            "{}: 1000 extra scans ({} KB) refused in {:.1} ms",
            s.name,
            bomb.len() / 1024,
            t.elapsed().as_secs_f64() * 1e3
        );
        assert!(decode(d, u32::MAX).is_some());
    }

    /// Prints the scan scripts (Ss-Se, Ah, Al) of the progressive samples, to show what was exercised.
    #[test]
    fn progressive_scan_scripts() {
        let mut seen_refine = false;
        for s in system_samples() {
            if !sof_info(&s.data).starts_with("SOF2") {
                continue;
            }
            let mut scans = Vec::new();
            for (m, a, b) in segments(&s.data).into_iter().flatten() {
                if m == 0xDA {
                    let p = &s.data[a..b];
                    let ns = usize::from(p[0]);
                    let (ss, se, ahal) = (p[1 + 2 * ns], p[2 + 2 * ns], p[3 + 2 * ns]);
                    seen_refine |= ahal >> 4 != 0;
                    scans.push(format!("n{ns}:{ss}-{se}/{}/{}", ahal >> 4, ahal & 15));
                }
            }
            println!("{} ({}): {}", s.name, sof_info(&s.data), scans.join(" "));
        }
        println!("successive-approximation refinement scans present: {seen_refine}");
    }
}
