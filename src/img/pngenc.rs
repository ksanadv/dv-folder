// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! A small PNG encoder, copied from dvsnip's (the set shares such pieces by copying, see the set's README), for a
//! picture pasted into a folder as a file. Screenshots are mostly flat colour and repeated rows, which a quick
//! deflate handles well: one hash probe per byte (LZ77 over a 32 KB window) and the fixed Huffman code. Each row gets
//! the filter (none, sub, up) with the smallest sum of magnitudes, as libpng does. No Windows dependencies.

static CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

pub fn crc32(parts: &[&[u8]]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for p in parts {
        for &b in *p {
            c = CRC_TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
        }
    }
    c ^ 0xFFFF_FFFF
}

pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    // 5552 bytes is the most that can be summed before the 32-bit total can overflow
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

struct Bits {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    /// Writes the low `len` bits of `v`, least significant first (how deflate packs everything but Huffman codes).
    #[inline]
    fn put(&mut self, v: u32, len: u32) {
        self.acc |= (v as u64) << self.n;
        self.n += len;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// Huffman codes are defined most significant bit first.
    #[inline]
    fn code(&mut self, code: u32, len: u32) {
        self.put(code.reverse_bits() >> (32 - len), len);
    }

    fn flush(&mut self) {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.acc = 0;
        self.n = 0;
    }
}

/// The fixed Huffman code of a literal or length symbol (RFC 1951, 3.2.6).
#[inline]
fn lit_code(sym: u32) -> (u32, u32) {
    match sym {
        0..=143 => (0x30 + sym, 8),
        144..=255 => (0x190 + sym - 144, 9),
        256..=279 => (sym - 256, 7),
        _ => (0xC0 + sym - 280, 8),
    }
}

const LEN_BASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145,
    8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] =
    [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

fn put_match(bits: &mut Bits, len: usize, dist: usize) {
    let li = LEN_BASE.partition_point(|&b| b as usize <= len) - 1;
    let (code, n) = lit_code(257 + li as u32);
    bits.code(code, n);
    bits.put((len - LEN_BASE[li] as usize) as u32, LEN_EXTRA[li] as u32);
    let di = DIST_BASE.partition_point(|&b| b as usize <= dist) - 1;
    bits.code(di as u32, 5);
    bits.put((dist - DIST_BASE[di] as usize) as u32, DIST_EXTRA[di] as u32);
}

const HASH_BITS: u32 = 15;
const WINDOW: usize = 32768;
const MAX_LEN: usize = 258;

/// A zlib stream (RFC 1950) holding `data`, compressed with LZ77 and the fixed Huffman code in one block.
pub fn zlib(data: &[u8]) -> Vec<u8> {
    let mut bits = Bits { out: Vec::with_capacity(data.len() / 4 + 64), acc: 0, n: 0 };
    bits.out.extend_from_slice(&[0x78, 0x01]);
    bits.put(1, 1); // last block
    bits.put(1, 2); // fixed Huffman
    let mut head = vec![u32::MAX; 1 << HASH_BITS];
    let hash = |i: usize| {
        let v = data[i] as u32 | (data[i + 1] as u32) << 8 | (data[i + 2] as u32) << 16;
        (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
    };
    let mut i = 0;
    while i < data.len() {
        let mut best = 0;
        let mut dist = 0;
        if i + 3 <= data.len() {
            let h = hash(i);
            let cand = head[h];
            head[h] = i as u32;
            if cand != u32::MAX && i - cand as usize <= WINDOW {
                let c = cand as usize;
                let max = (data.len() - i).min(MAX_LEN);
                let mut l = 0;
                while l < max && data[c + l] == data[i + l] {
                    l += 1;
                }
                if l >= 3 {
                    best = l;
                    dist = i - c;
                }
            }
        }
        if best >= 3 {
            put_match(&mut bits, best, dist);
            // index the skipped positions sparsely: enough to keep finding matches, cheap on long runs
            let end = i + best;
            let mut j = i + 1;
            while j + 3 <= data.len() && j < end {
                head[hash(j)] = j as u32;
                j += if best > 32 { 8 } else { 1 };
            }
            i = end;
        } else {
            let (code, n) = lit_code(data[i] as u32);
            bits.code(code, n);
            i += 1;
        }
    }
    let (code, n) = lit_code(256);
    bits.code(code, n);
    bits.flush();
    bits.out.extend_from_slice(&adler32(data).to_be_bytes());
    bits.out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
}

/// Filters one row of `bpp`-byte pixels with whichever of none, sub and up looks most compressible.
fn filter_row(cur: &[u8], prev: Option<&[u8]>, bpp: usize, out: &mut Vec<u8>) {
    // a byte counts as its distance from zero, so a run of small differences scores low
    let mag = |b: u8| (b as i8).unsigned_abs() as u32;
    let (head, tail) = cur.split_at(bpp.min(cur.len()));
    let sub_diff = |a: &u8, b: &u8| a.wrapping_sub(*b);
    let none: u32 = cur.iter().map(|&b| mag(b)).sum();
    let sub: u32 =
        head.iter().map(|&b| mag(b)).sum::<u32>() + tail.iter().zip(cur).map(|(a, b)| mag(sub_diff(a, b))).sum::<u32>();
    let up = prev.map_or(u32::MAX, |p| cur.iter().zip(p).map(|(a, b)| mag(sub_diff(a, b))).sum());
    if none <= sub && none <= up {
        out.push(0);
        out.extend_from_slice(cur);
    } else if sub <= up {
        out.push(1);
        out.extend_from_slice(head);
        out.extend(tail.iter().zip(cur).map(|(a, b)| sub_diff(a, b)));
    } else {
        out.push(2);
        out.extend(cur.iter().zip(prev.unwrap()).map(|(a, b)| sub_diff(a, b)));
    }
}

/// straight alpha from premultiplied pixels
fn unpremultiply(px: &[u32]) -> Vec<u32> {
    px.iter()
        .map(|&p| {
            let a = p >> 24;
            if a == 0xFF || a == 0 {
                return p;
            }
            let ch = |s: u32| (((p >> s) & 0xFF) * 255 + a / 2) / a;
            (a << 24) | (ch(16).min(255) << 16) | (ch(8).min(255) << 8) | ch(0).min(255)
        })
        .collect()
}

/// Premultiplied 0xAARRGGBB pixels, `w` by `h`, as PNG: 8-bit RGB when every pixel is opaque, RGBA (straight alpha)
/// otherwise.
pub fn encode(px: &[u32], w: u32, h: u32) -> Vec<u8> {
    let opaque = px.iter().all(|&p| p >> 24 == 0xFF);
    let bpp = if opaque { 3 } else { 4 };
    let straight;
    let px = if opaque {
        px
    } else {
        straight = unpremultiply(px);
        &straight
    };
    let wu = w as usize;
    let mut raw = Vec::with_capacity((wu * bpp + 1) * h as usize);
    let mut prev: Vec<u8> = Vec::new();
    let mut cur: Vec<u8> = Vec::with_capacity(wu * bpp);
    for row in px.chunks_exact(wu.max(1)) {
        cur.clear();
        for &p in row {
            cur.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
            if !opaque {
                cur.push((p >> 24) as u8);
            }
        }
        filter_row(&cur, if prev.is_empty() { None } else { Some(&prev) }, bpp, &mut raw);
        std::mem::swap(&mut prev, &mut cur);
    }
    let mut out = Vec::with_capacity(raw.len() / 4 + 128);
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, if opaque { 2 } else { 6 }, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &zlib(&raw));
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// what the encoder writes, read back by dvfiles' own PNG decoder: the same pixels, opaque and with alpha
    #[test]
    fn round_trips_through_dvfiles_decoder() {
        let (w, h) = (37u32, 23u32);
        let opaque: Vec<u32> =
            (0..w * h).map(|i| 0xFF00_0000 | (i.wrapping_mul(2654435761) >> 8 & 0x00FF_FFFF) / ((i % 7) + 1)).collect();
        let png = encode(&opaque, w, h);
        let d = crate::img::png::decode(&png).expect("decodes");
        assert_eq!((d.w, d.h), (w, h));
        assert_eq!(d.px, opaque);
        // half transparent pixels come back within rounding of what went in
        let alpha: Vec<u32> = (0..w * h).map(|i| if i % 3 == 0 { 0x8040_2010 } else { 0xFF10_2030 }).collect();
        let d = crate::img::png::decode(&encode(&alpha, w, h)).expect("decodes");
        for (a, b) in d.px.iter().zip(&alpha) {
            for sh in [24, 16, 8, 0] {
                assert!(((a >> sh & 0xFF) as i32 - (b >> sh & 0xFF) as i32).abs() <= 1, "{a:08x} {b:08x}");
            }
        }
        // flat screenshots squeeze
        let flat = vec![0xFF20_2020u32; 1920 * 1080];
        let n = encode(&flat, 1920, 1080).len();
        assert!(n < 100_000, "{n}");
    }
}
