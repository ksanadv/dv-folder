// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! zlib (RFC 1950) and raw DEFLATE (RFC 1951) decompression using only `std`.
//!
//! Huffman codes are decoded with a 10-bit lookup table whose entries already say
//! what the symbol means (literal byte, length or distance base plus extra bits, end
//! of block); the rare longer codes fall back to a short canonical-code walk. Every
//! read is bounds-checked and any bad or truncated input returns `None`.

use std::sync::OnceLock;

/// Bits looked up at once in the fast Huffman table.
const FAST_BITS: u32 = 10;
const FAST_SIZE: usize = 1 << FAST_BITS;
const FAST_MASK: u64 = FAST_SIZE as u64 - 1;

/// Base lengths and extra bits for length symbols 257..=285.
const LEN_BASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];

/// Base distances and extra bits for distance symbols 0..=29.
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145,
    8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] =
    [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

/// Order in which the code-length code lengths are stored in a dynamic block header.
const CLEN_ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

/// Highest compression ratio DEFLATE can reach (258 bytes from about 2 bits).
const MAX_RATIO: usize = 1032;

/// Decompresses a zlib stream (2-byte header, DEFLATE data, Adler-32 checksum).
/// Returns `None` on any error, on a checksum mismatch, or if the output would
/// exceed `max_out` bytes. The output buffer starts at a few times the input size
/// and grows as needed, so a generous `max_out` costs nothing up front.
pub fn zlib_decompress(data: &[u8], max_out: usize) -> Option<Vec<u8>> {
    zlib(data, max_out, true, guess_capacity(data.len(), max_out))
}

/// Decompresses zlib data whose decompressed size is known in advance (PNG image
/// data): the buffer is allocated once at `size` bytes (when the input can plausibly
/// expand that far), output beyond `size` fails, and the Adler-32 trailer is not
/// verified or required, because browsers and WIC accept PNGs with a bad checksum.
/// The result may be shorter than `size` if the stream ends early.
pub fn zlib_decompress_known_size(data: &[u8], size: usize) -> Option<Vec<u8>> {
    let start = size.min(data.len().saturating_mul(MAX_RATIO).saturating_add(64));
    zlib(data, size, false, start)
}

/// Decompresses raw DEFLATE data (no zlib header or checksum).
/// Returns `None` on any error or if the output would exceed `max_out` bytes.
pub fn inflate(raw_deflate: &[u8], max_out: usize) -> Option<Vec<u8>> {
    let mut bits = BitReader::new(raw_deflate);
    inflate_blocks(&mut bits, max_out, guess_capacity(raw_deflate.len(), max_out))
}

/// First output buffer size when the final size is unknown.
fn guess_capacity(input_len: usize, max_out: usize) -> usize {
    max_out.min(input_len.saturating_mul(4).saturating_add(64 << 10))
}

fn zlib(data: &[u8], max_out: usize, verify: bool, start: usize) -> Option<Vec<u8>> {
    let cmf = *data.first()?;
    let flg = *data.get(1)?;
    // Compression method 8 (deflate) with a window of at most 32 KiB.
    if cmf & 0x0F != 8 || cmf >> 4 > 7 {
        return None;
    }
    if ((cmf as u16) << 8 | flg as u16) % 31 != 0 {
        return None;
    }
    // A preset dictionary is never used by PNG and is not supported.
    if flg & 0x20 != 0 {
        return None;
    }
    let body = data.get(2..)?;
    let mut bits = BitReader::new(body);
    let out = inflate_blocks(&mut bits, max_out, start)?;
    if verify {
        // The Adler-32 checksum (big-endian) starts at the next byte boundary.
        let at = bits.byte_position()?;
        let sum = body.get(at..at.checked_add(4)?)?;
        if adler32(&out) != u32::from_be_bytes([sum[0], sum[1], sum[2], sum[3]]) {
            return None;
        }
    }
    Some(out)
}

/// Adler-32 checksum as used by zlib.
fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    // 5552 is the largest block size for which `b` cannot overflow a u32.
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

// ---------------------------------------------------------------- bit input

/// Little-endian bit reader over a byte slice, keeping up to 64 bits buffered.
/// Reading past the end yields zero bits and is recorded in `pad`, so callers can
/// detect truncation with `overrun()`.
#[derive(Clone, Copy)]
struct BitReader<'a> {
    data: &'a [u8],
    /// Index of the next byte to load from `data`.
    pos: usize,
    /// Buffered bits, least significant first.
    buf: u64,
    /// Number of valid bits in `buf`.
    n: u32,
    /// Number of zero bytes appended after the end of `data`.
    pad: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0, buf: 0, n: 0, pad: 0 }
    }

    /// Tops the buffer up to at least 56 valid bits.
    #[inline(always)]
    fn refill(&mut self) {
        if self.n > 56 {
            return;
        }
        if let Some(chunk) = self.data.get(self.pos..self.pos + 8) {
            // Fast path: load 8 bytes at once and keep the whole bytes that fit.
            // Bits above `n` may hold part of the next byte; reloading that byte
            // later puts the same bits in the same place, so this is harmless.
            let mut word = [0u8; 8];
            word.copy_from_slice(chunk);
            self.buf |= u64::from_le_bytes(word) << self.n;
            self.pos += ((63 - self.n) >> 3) as usize;
            self.n |= 56;
        } else {
            *self = self.refill_slow();
        }
    }

    /// Near the end of the input: one byte at a time, then zero padding.
    /// (Takes and returns the reader by value so callers' copies stay in registers.)
    #[cold]
    fn refill_slow(mut self) -> Self {
        while self.n <= 56 {
            let byte = match self.data.get(self.pos) {
                Some(&b) => {
                    self.pos += 1;
                    b
                }
                None => {
                    self.pad = self.pad.saturating_add(1);
                    0
                }
            };
            self.buf |= (byte as u64) << self.n;
            self.n += 8;
        }
        self
    }

    /// Drops `k` bits from the buffer.
    #[inline(always)]
    fn consume(&mut self, k: u32) {
        if k <= self.n {
            self.buf = self.buf.checked_shr(k).unwrap_or(0);
            self.n -= k;
        } else {
            // Never happens when callers refill first; treat it as reading past the end.
            self.buf = 0;
            self.n = 0;
            self.pad = u32::MAX / 16;
        }
    }

    /// Reads `k` bits (k <= 32) as an integer, first bit in the lowest position.
    #[inline(always)]
    fn bits(&mut self, k: u32) -> u32 {
        let v = (self.buf & ((1u64 << k) - 1)) as u32;
        self.consume(k);
        v
    }

    /// True if any of the zero padding bits past the end of the input were consumed.
    #[inline(always)]
    fn overrun(&self) -> bool {
        (self.pad as u64) * 8 > self.n as u64
    }

    /// Skips to the next byte boundary.
    fn align_to_byte(&mut self) {
        let k = self.n % 8;
        self.consume(k);
    }

    /// Index in `data` of the first byte not yet consumed, after aligning to a byte
    /// boundary. `None` if padding past the end was consumed.
    fn byte_position(&mut self) -> Option<usize> {
        self.align_to_byte();
        if self.overrun() {
            return None;
        }
        // Valid buffered bytes minus padding bytes are real bytes not yet consumed.
        let buffered_real = (self.n / 8).checked_sub(self.pad)? as usize;
        self.pos.checked_sub(buffered_real)
    }
}

// ---------------------------------------------------------------- output

/// Output buffer. `buf` is allocated (zeroed) ahead of the bytes written so far, so
/// the hot loop writes by index; it grows by doubling but never beyond `max` bytes.
/// The number of bytes written (`len` below) is tracked by the caller.
struct Output {
    buf: Vec<u8>,
    max: usize,
}

impl Output {
    fn new(start: usize, max: usize) -> Output {
        Output { buf: vec![0; start.min(max)], max }
    }

    /// Makes `buf` at least `need` bytes long; `None` if that would exceed `max`.
    #[cold]
    fn grow(&mut self, need: usize) -> Option<()> {
        if need > self.max {
            return None;
        }
        if need > self.buf.len() {
            let size = need.max(self.buf.len().saturating_mul(2)).min(self.max);
            self.buf.resize(size, 0);
        }
        Some(())
    }

    #[inline(always)]
    fn push(&mut self, len: &mut usize, b: u8) -> Option<()> {
        if *len >= self.buf.len() {
            self.grow(len.checked_add(1)?)?;
        }
        *self.buf.get_mut(*len)? = b;
        *len += 1;
        Some(())
    }

    fn extend(&mut self, len: &mut usize, src: &[u8]) -> Option<()> {
        let end = len.checked_add(src.len())?;
        if end > self.buf.len() {
            self.grow(end)?;
        }
        self.buf.get_mut(*len..end)?.copy_from_slice(src);
        *len = end;
        Some(())
    }

    /// Appends `n` bytes copied from `distance` bytes back (the ranges may overlap,
    /// which repeats the last `distance` bytes).
    #[inline(always)]
    fn copy_match(&mut self, len: &mut usize, distance: usize, n: usize) -> Option<()> {
        if distance > *len || distance == 0 {
            return None;
        }
        let end = *len + n;
        let from = *len - distance;
        if end + 16 <= self.buf.len() && distance >= 8 {
            // Fast path, with spare room after `end`: copy whole words. A word's source
            // lies at least one word back, so it is complete before it is read; the
            // last word may spill past `end` into room that later output overwrites.
            if distance >= 16 {
                copy_words::<16>(&mut self.buf, from, *len, end)?;
            } else {
                copy_words::<8>(&mut self.buf, from, *len, end)?;
            }
        } else {
            if end > self.buf.len() {
                self.grow(end)?;
            }
            let window = self.buf.get_mut(from..end)?;
            if distance == 1 {
                let b = window[0];
                window[1..].fill(b);
            } else {
                // Byte by byte so that overlapping copies see the bytes just written.
                for i in distance..window.len() {
                    window[i] = window[i - distance];
                }
            }
        }
        *len = end;
        Some(())
    }

    fn finish(mut self, len: usize) -> Vec<u8> {
        self.buf.truncate(len);
        self.buf
    }
}

/// Copies `buf[from..]` to `buf[to..end]` in W-byte words (may write up to W - 1
/// bytes past `end`). Requires `to - from >= W`.
#[inline(always)]
fn copy_words<const W: usize>(buf: &mut [u8], mut from: usize, mut to: usize, end: usize) -> Option<()> {
    while to < end {
        let mut word = [0u8; W];
        word.copy_from_slice(buf.get(from..from + W)?);
        buf.get_mut(to..to + W)?.copy_from_slice(&word);
        from += W;
        to += W;
    }
    Some(())
}

// ---------------------------------------------------------------- Huffman codes

/// What a decoded symbol means (bits 8-9 of a table entry).
const KIND_LITERAL: u32 = 0; // a literal byte (or a code-length symbol)
const KIND_BASE: u32 = 1; // a match length or a distance: base value plus extra bits
const KIND_END: u32 = 2; // end of block
const KIND_INVALID: u32 = 3; // symbols the format reserves (286, 287, distances 30, 31)

/// A table entry: bits 0-3 code length, 4-7 number of extra bits, 8-9 kind,
/// 16-31 value (literal byte or base). 0 means "not in the fast table".
#[inline]
fn entry(code_len: u32, extra: u32, kind: u32, value: u32) -> u32 {
    code_len | extra << 4 | kind << 8 | value << 16
}

fn kind(e: u32) -> u32 {
    (e >> 8) & 3
}

/// Reads the extra bits of a length/distance entry and adds them to its base.
#[inline(always)]
fn with_extra(e: u32, bits: &mut BitReader) -> usize {
    (e >> 16) as usize + bits.bits((e >> 4) & 15) as usize
}

/// Entries for the literal/length alphabet.
fn litlen_entry(sym: usize, code_len: u32) -> u32 {
    match sym {
        0..=255 => entry(code_len, 0, KIND_LITERAL, sym as u32),
        256 => entry(code_len, 0, KIND_END, 0),
        257..=285 => entry(code_len, LEN_EXTRA[sym - 257] as u32, KIND_BASE, LEN_BASE[sym - 257] as u32),
        _ => entry(code_len, 0, KIND_INVALID, 0),
    }
}

/// Entries for the distance alphabet.
fn dist_entry(sym: usize, code_len: u32) -> u32 {
    match sym {
        0..=29 => entry(code_len, DIST_EXTRA[sym] as u32, KIND_BASE, DIST_BASE[sym] as u32),
        _ => entry(code_len, 0, KIND_INVALID, 0),
    }
}

/// Entries for the code-length alphabet (the symbol itself).
fn clen_entry(sym: usize, code_len: u32) -> u32 {
    entry(code_len, 0, KIND_LITERAL, sym as u32)
}

/// A canonical Huffman code with a fast lookup table.
struct Huffman {
    /// Indexed by the next FAST_BITS input bits.
    fast: [u32; FAST_SIZE],
    /// Number of codes of each length 0..=15.
    count: [u16; 16],
    /// Symbols ordered by code (shorter codes first, then by symbol value).
    symbols: [u16; 288],
    /// Builds the table entry of a symbol.
    entry_of: fn(usize, u32) -> u32,
}

impl Huffman {
    /// Builds a code from per-symbol code lengths (0 = unused). Over-subscribed sets are
    /// rejected; incomplete sets are allowed and their unused codes fail when decoded.
    fn new(lengths: &[u8], entry_of: fn(usize, u32) -> u32) -> Option<Huffman> {
        if lengths.len() > 288 {
            return None;
        }
        let mut h = Huffman { fast: [0; FAST_SIZE], count: [0; 16], symbols: [0; 288], entry_of };
        for &len in lengths {
            *h.count.get_mut(len as usize)? += 1;
        }
        h.count[0] = 0;

        // Check that the code lengths do not over-subscribe the code space.
        let mut left: i32 = 1;
        for len in 1..16 {
            left = left * 2 - h.count[len] as i32;
            if left < 0 {
                return None;
            }
        }

        // Where each length starts in `symbols`, and the first code of each length.
        let mut offset = [0usize; 16];
        let mut next_code = [0u32; 16];
        let mut code = 0u32;
        for len in 1..16 {
            offset[len] = if len == 1 { 0 } else { offset[len - 1] + h.count[len - 1] as usize };
            code = (code + h.count[len - 1] as u32) << 1;
            next_code[len] = code;
        }

        for (sym, &len) in lengths.iter().enumerate() {
            if len == 0 {
                continue;
            }
            let l = len as usize;
            *h.symbols.get_mut(offset[l])? = sym as u16;
            offset[l] += 1;
            let c = next_code[l];
            next_code[l] += 1;
            if (len as u32) <= FAST_BITS {
                // The stream stores codes most significant bit first, so the table is
                // indexed by the bit-reversed code; fill every slot that starts with it.
                let rev = (c.reverse_bits() >> (32 - len as u32)) as usize;
                let e = entry_of(sym, len as u32);
                let mut i = rev;
                while i < FAST_SIZE {
                    h.fast[i] = e;
                    i += 1 << len;
                }
            }
        }
        Some(h)
    }

    /// Decodes one symbol and returns its table entry. The caller must have refilled
    /// the reader (at least 15 bits buffered).
    #[inline(always)]
    fn decode(&self, bits: &mut BitReader) -> Option<u32> {
        let mut e = self.fast[(bits.buf & FAST_MASK) as usize];
        if e == 0 {
            e = self.decode_slow(bits.buf)?;
        }
        bits.consume(e & 15);
        Some(e)
    }

    /// Canonical decoding one bit at a time, for codes longer than FAST_BITS.
    #[cold]
    fn decode_slow(&self, input: u64) -> Option<u32> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16u32 {
            code |= ((input >> (len - 1)) & 1) as i32;
            let count = self.count[len as usize] as i32;
            if code - first < count {
                let sym = *self.symbols.get((index + code - first) as usize)?;
                return Some((self.entry_of)(sym as usize, len));
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        None
    }
}

// ---------------------------------------------------------------- blocks

/// Decodes DEFLATE blocks until the final one.
fn inflate_blocks(bits: &mut BitReader, max_out: usize, start: usize) -> Option<Vec<u8>> {
    let mut out = Output::new(start, max_out);
    let mut len = 0usize;
    loop {
        bits.refill();
        let header = bits.bits(3);
        match header >> 1 {
            0 => stored_block(bits, &mut out, &mut len)?,
            1 => {
                let (lit, dist) = fixed_codes()?;
                huffman_block(bits, lit, dist, &mut out, &mut len)?;
            }
            2 => {
                let (lit, dist) = dynamic_codes(bits)?;
                huffman_block(bits, &lit, &dist, &mut out, &mut len)?;
            }
            _ => return None,
        }
        if bits.overrun() {
            return None;
        }
        if header & 1 != 0 {
            return Some(out.finish(len));
        }
    }
}

/// Copies an uncompressed block: LEN, NLEN, then LEN raw bytes.
fn stored_block(bits: &mut BitReader, out: &mut Output, out_len: &mut usize) -> Option<()> {
    bits.align_to_byte();
    bits.refill();
    let len = bits.bits(16) as usize;
    let nlen = bits.bits(16) as usize;
    if len != !nlen & 0xFFFF || bits.overrun() {
        return None;
    }
    // First take the whole bytes still sitting in the bit buffer...
    let mut left = len;
    while left > 0 && bits.n >= 8 {
        out.push(out_len, bits.bits(8) as u8)?;
        left -= 1;
    }
    if bits.overrun() {
        return None;
    }
    // ...then copy the rest straight from the input.
    if left > 0 {
        let src = bits.data.get(bits.pos..bits.pos.checked_add(left)?)?;
        out.extend(out_len, src)?;
        bits.pos += left;
        bits.buf = 0; // drop any look-ahead bits that belonged to the copied bytes
    }
    Some(())
}

/// The fixed literal/length and distance codes of block type 1 (built once).
fn fixed_codes() -> Option<(&'static Huffman, &'static Huffman)> {
    static FIXED: OnceLock<Option<(Huffman, Huffman)>> = OnceLock::new();
    let codes = FIXED.get_or_init(|| {
        let mut lens = [0u8; 288];
        lens[..144].fill(8);
        lens[144..256].fill(9);
        lens[256..280].fill(7);
        lens[280..].fill(8);
        // All 32 distance codes are 5 bits; symbols 30 and 31 are invalid when decoded.
        Some((Huffman::new(&lens, litlen_entry)?, Huffman::new(&[5u8; 32], dist_entry)?))
    });
    codes.as_ref().map(|(l, d)| (l, d))
}

/// Reads the code definitions at the start of a dynamic block (type 2).
fn dynamic_codes(bits: &mut BitReader) -> Option<(Huffman, Huffman)> {
    bits.refill();
    let hlit = bits.bits(5) as usize + 257;
    let hdist = bits.bits(5) as usize + 1;
    let hclen = bits.bits(4) as usize + 4;
    if hlit > 286 || hdist > 30 {
        return None;
    }

    // Code lengths for the code-length alphabet, 3 bits each, in a fixed order.
    let mut clen = [0u8; 19];
    for &sym in CLEN_ORDER.iter().take(hclen) {
        bits.refill();
        clen[sym] = bits.bits(3) as u8;
    }
    let clen_code = Huffman::new(&clen, clen_entry)?;

    // Literal/length and distance code lengths, run-length encoded.
    let total = hlit + hdist;
    let mut lens = [0u8; 286 + 30];
    let mut i = 0;
    while i < total {
        bits.refill();
        if bits.overrun() {
            return None;
        }
        let sym = clen_code.decode(bits)? >> 16;
        let (value, repeat) = match sym {
            0..=15 => (sym as u8, 1),
            16 => (*lens.get(i.checked_sub(1)?)?, 3 + bits.bits(2) as usize),
            17 => (0, 3 + bits.bits(3) as usize),
            18 => (0, 11 + bits.bits(7) as usize),
            _ => return None,
        };
        if i + repeat > total {
            return None;
        }
        lens.get_mut(i..i + repeat)?.fill(value);
        i += repeat;
    }
    // A block without an end-of-block code could never finish.
    if lens[256] == 0 {
        return None;
    }
    let lit = Huffman::new(lens.get(..hlit)?, litlen_entry)?;
    let dist = Huffman::new(lens.get(hlit..total)?, dist_entry)?;
    Some((lit, dist))
}

/// Decodes the symbols of a Huffman-coded block until end-of-block.
fn huffman_block(
    bits: &mut BitReader,
    lit: &Huffman,
    dist: &Huffman,
    out: &mut Output,
    out_len: &mut usize,
) -> Option<()> {
    // Work on local copies of the reader and the output length: they never escape
    // this function, so the compiler keeps them in registers.
    let mut br = *bits;
    let mut len = *out_len;
    loop {
        br.refill();
        if br.overrun() {
            return None;
        }
        let mut e = lit.decode(&mut br)?;
        if kind(e) == KIND_LITERAL {
            // A refill leaves >= 56 bits: room for three literals (15 bits each at most).
            out.push(&mut len, (e >> 16) as u8)?;
            e = lit.decode(&mut br)?;
            if kind(e) == KIND_LITERAL {
                out.push(&mut len, (e >> 16) as u8)?;
                e = lit.decode(&mut br)?;
                if kind(e) == KIND_LITERAL {
                    out.push(&mut len, (e >> 16) as u8)?;
                    continue;
                }
            }
            // A length/distance pair needs up to 5 + 15 + 13 more bits.
            br.refill();
        }
        match kind(e) {
            KIND_BASE => {
                let n = with_extra(e, &mut br);
                let d = dist.decode(&mut br)?;
                if kind(d) != KIND_BASE {
                    return None;
                }
                let distance = with_extra(d, &mut br);
                out.copy_match(&mut len, distance, n)?;
            }
            KIND_END => break,
            _ => return None,
        }
    }
    *bits = br;
    *out_len = len;
    Some(())
}

#[cfg(test)]
mod tests {
    use super::super::testutil as tu;
    use super::*;

    // Vectors produced once with Python's zlib (tools/gen_vectors.py); the original
    // data is regenerated by `gen` below.
    const V_DYNAMIC: &str = "78da75984b689d4514c7eba6c511ea4631c5d20e485605c54a510b5aa45468252aa89bbaf1865c93e84dbc42a58f950f28bec045dd282811843e4411a57511c50a0a82d0428a8882ae2c9652d280b852d07ee73f77ce6f66ae8b7cf9ee3cce9cc7fffccf996fb8381b878bb361a63f38d00b07e717679e3b18df3e30f7c2c274e80d8673bd787eb6b7b0d00b4fcf0ffaf1c8607e3a6a72387fa83f88b664b4cd64c4b8cd1644dbd03d42273f2d99ee5f5ba17913106c204dda41daa7c934ae1f9aedd6076a20c5df7d6f284362a18b6b52ea28591010efd5211bd3cc35417a93043d930873827699f67fca4fdd70676afcdd16d8a1668ba6bb19378db64aac3ca1c93bfe92786c37e9b6e4a11de6030d43a5f02b4d4cde3155baa36deb9efb342a7314d7b4c98e7a52e1324fbc945151f84901ef24ea4d6a5cb0edb0491192265a620ab86792d145e42131453b4d4b823ba28047258a98d1a176de240161eed2ab866d20edbc82d0d69b22378d221af229087a82a37e404cd84be78f221308cbdf8e1681b4736d11d062f28b9cf578073700aa252ffdc298c8d576803df43ba5a7272d34a65f3580e184b97492cc3d8b9c7417c9fa47b211f436508494e8d421a4edbc7df63e2ef0b24fc7ecb45580585c9f54cc89612bfefd51b37e34900b0cfce1310d95055958b8b2c7fee9d4c79c4f0a44db92e31ee0f55a2381641bfa5eef2021a9f6e8f916081f796835ba9bdea1f3a81c319bf943876c027547148930b0f3f27101fc55eb1b32092409d9734916cdd7e2cb3c96b40fd975a2c59b26e82f3def4a6967e899a2b980692637260ee56f318d3f787809206139747ab03f2445e9eedbd78a9fbe38c325789124fa917ee559f2ca64c1ecb6635b952419b639d95d89b2eccabcdd696c4d299993b12d8cc9a9da563327b80659852987456c3b86a53b9b58d415566f5fb83d7ee0a81541a6e6c8530f27c530c3ce6119a79e4404a032f0fe378ba6278726bfafbdce279d69e77fd696fa6fb6831a9dba2380322688ee7759e1d4470ca3c4250e5fa602901532a117256245bbd66d4604be248325411ba4db53eeb5a4ba978891159f6fad9b08f69085c1057872b7126e808336b0a9d392afa5b39d2cd315b9631a2716894189704049984dbd457dc24f12771a1d60dd63baed63c9c2265e6b0883c5360d6fd691c4d4f33fd40e893b860c499bc7ef48e317a70bbfa259c100fd79067d3dbb25a618e0f77eae309758a16b03a35f42949af0164acbaa334117f4c2aa29cd31bade22916dd3fa13992bfce6013a01b86b36bd054d9244168ad46713c4d731cbeff7760259ebe8710e639f5eb045be7fbd5c948b3ad10baf00a38e6c6eccae35c957abce27bb2441930d623fe10f161f395cf59f20b170483a7e5bd4a738618b57118619e4723c59f44f5e5c4253fcea14f4d195dc9983638bf879b5f1462fa069e20e74607c6eaae98c59dc8347e435c4e10d5c86dd2fcf7a5d6edae79622ab6ba9ef20cbadbcc95b57ea324637dd30565015c9f2d03903e41369ec66678990a580849b2e9c17c15cbcedc0334bc7eb7c0b4e76fbaf228777318f5e417c07403eaf42e4e77c110bcd7d106a7dd7d9718f8627531eecf3c4d5cab3dce65124518e6c2cf89d58c6a700b420a340a10424987c5cfacea2a8a1a3f40abc2fb1b769fb72de48c21843dc9262024e69d1a59152372a44e64352324d3ced48e2e515971e84ed54be883797a2367efb4b2a414e9d687afe64deebe3ca316e77a45d840dcde004d998b7f670f7c2a744da27ffd3c685a91cd70791d56e347ea39d6a5c85aed737b4b72e94bdc0e03a3d3067b91f5fe68aaf07e8aa93bbf79278a1f33a34569e317eb949a82db8e519470af15d7e014b9ebcb513b0ead5f762cbd13479ad28b46457ae2a3e83a67f0f545f531455d42064009a0978da131ffaa35e171dac17655e8dafffc749505161bdd7fe0bdb35f333ba59c2b161bab0d37154f5bee4cd708e04d747c922caf0e1e9556208d4428837376ad6d9ad96953aef9892a8faf0c216e7320a4d4341e1c373ac6ec50cbf507ee55d481873c7791cb75493f11fa71d8a89";
    const V_FIXED: &str = "78012bc84b5728c928cd4d52a8cac94c5228c8ac48cd51f09b9b969993aa509e9997925fae909e989b9ba8f011c2e34a4a2d49542800ea4a49cd01b250d440e4404670410c82a801a986ea069b9b98539091a8b00d428145905c0031096a2c441cac900b6e27174423440e22029203622e646743e44132102321fcd3c8e673b54115819d0c96811a7e08641ac4f51025aac8d60940ad40f224441f8a0320dac12248c21025c8464ba084e17d8841183ee5423115a2f3387a10a23879038442712b2888b9c0e18c882b687c40b42a9520a503a856a44883841452a88125b94a9155c6419442fc81e206880f215ec3080668f020c71e44372876b9501cf410e478484a9c8308022e785c43f5811540ec3402b3c15e7b0d6622ac474eaa90a0990e3113a213228b300aec702e140f89230ba1781891aeb99042880b29c21ec29329d77b90fbc10e80040bc470e4c49b83f00544492a92ebc06102b60a5923864390bc821a2d48b186e14aa48c8d1c2a105311e10966196206103802e1191129e541021d39f120a772ae75b320418e25bb418c027b0839b6d2c09623498203042994909331582d38d8507218c457700f41332122c99d021b0c96470a062e84990af032129207202446b843f338721e402e89610533d70e50f021390a2c8d94b220ee8385113c0f24a50200714f23b4";
    const V_MIXED: &str = "78da002c01d3fe7468756d62207468756d622077696e646f772067616d6d612067616d6d612066696c650a64656c74610a64656c74610a67616d6d61207468756d622064656c74612066696c6520e377696e646f772064656c746120616c7068612067616d6d610a706978656c207a6c696220706e670a7a6c696220a566696c6520616c7068612066696c652066696c6520616c706861207468756d62207468756d6220706e67201a626574612066696c6520706978656c0a64656c7461200f7a6c69620a64656c74612077696e646f7720706e670a64656c74610a6265746120706e670a64656c74612077696e646f770a7468756d62206265746120626574612077696e646f772066696c652080616c7068612064656c74610a7a6c696220706e6720626574612066696c650a66696c65202ac928cd4d5248cbcc49e54a4a2d4954785e821028c84b57000ba627e6e6262aa4a4e600d96001300151b9900d428365b9ca33f352f2cb150a322b5273a01a824066411527e614642442654ba06aab723293b820427f4036420c825809510f55083102c9416012ec4a08bf19595321d80b30f700d5809d01f712d842882d5ce806838d040b825948664099107b8421fa10f6432c86f85aa6033540f2d2b92002db21144415c43a886e503828dc45f131dc1d50a3c141258514f8109d20012ee460037b15ac05e11524f7433c0cf53c72402019871c3208e31494104a1466a1fb10a21022ea035602f613246ea1cacc91621d1125c88290908168420a19246bc0c1801469600783a31b6c2058395802623e24b581bdfc1c12f7403320a20816dc290887811d02000000ffff6d56316b9441106db4598216da67b130580839112cc446d35b0812d0c23beecc9db92f1e72c9050bc1c240482704cc6f100bfb54c1564544453bd146456cacd4c26fdf9b6fe7ed5d8a5bf6766776deccbc99f92683b5eedd4918f7d7ab4ebc3d18f622b7ddde70dc0ea3c1666f185679b4d2aeaa76680f47fd7ac54decf4c6ed787f38e87037e16314b43fa3b595987e903a51ef0276903185b80d6d2c5ff07e146580c2c22b883de43edd13100408ca747162064486be240c61998f88abbca503ea12ed03f86f3b6168a0abc06809929ff7e792bb14a1d9f47f8be1595cf8e978c2533e8727704c2d040fc05e9bc3104bc7e665d2b0ec19b00be9b6a253ea023051d291369738c9d660a095c0d23845974d5655609ed70508fee1c5273b7b9473163c7b6ae01d9e7cf211481105868ae9d038420239bc7408e72ca605143cb79e560685767b4276c9b12f9a7ee8591ea14d9b349699c890308c042e001aeed4c21fd252540114f78805c65f36751372016de32f5fa461ca43d72b45120c97cd02cd29734913be2474f4ba9612b13d1028076c2fc04b224d251bc51134b3529d2ce35a743f2d78be7a7b912869406a33892c3115bc3b2b4e3992fca6473cab0b00c1e9450554a13217178a9a71dfb4e3d0c7cbc4f0669e06fe49e4a3749a38e7e428d8a2e4737782f7493ed2caeca090d0356809de70c8613646784a3a2cd485e22631cfdb4df75b33b2a10447007d2e3847e3c674ca83b5dfc607a5e1b92ba796bc3335358093ebbbea71c8528a8211f8a6d92d9a144138d75795dade0d543a4a92c2316f684581155de8a634ee02844e580da770412fb509da70209b52976e6a058b725a87553c32d52bb477500901f032572cb98a8428defab411cc0ec8c23f6936d261be5fa5e42f360669381c713c78d0a7f47ba195b6c143d26011b69b7b3e738ab42a6d7ef878f518fb002f460002d4723f82446c1a63ecd245adf3e941289f2850bf2575fa0797d7bcb16ee9ec8b3353323858e59d04d87ac481741495cf293fced3934db7098f01e84edad2ecd7e7b8c3f1ce6c2f2a120221dadfe91c9559166727f62b45c5575abb9970450bd0e69ad985189cd12bc99b7c888263b913149f3d5253fe4d12cfebfc948f30f90e28daf75fc96cc1bf6745b4bc00c369f34b4b9eb97b51dffc079af8d1f8";
    const V_RUN: &str = "78da4b4c1c05a360140c7790969993aa509e9997925fce351fcc29c84be7820a54e564264125153e23a954004ba4a627e6e62642d8059915a939509209aa282535a724912b31a720034a828c8628074b81b98a49a940564946692ecc102401b0055c50b381ce5a033112622f44c92da84a002745c4b2";

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
    }

    /// Same generator as tools/gen_vectors.py.
    fn gen(n: usize, seed: u64) -> Vec<u8> {
        const WORDS: [&[u8]; 10] =
            [b"alpha", b"beta", b"gamma", b"delta", b"pixel", b"window", b"file", b"thumb", b"png", b"zlib"];
        let mut x = seed;
        let mut out = Vec::new();
        while out.len() < n {
            x = (x * 1103515245 + 12345) & 0x7FFF_FFFF;
            if (x >> 4) % 7 == 0 {
                out.push(((x >> 20) & 0xFF) as u8);
            } else {
                out.extend_from_slice(WORDS[((x >> 16) % 10) as usize]);
                out.push(if (x >> 8) & 3 != 0 { b' ' } else { b'\n' });
            }
        }
        out.truncate(n);
        out
    }

    fn vectors() -> Vec<(&'static str, Vec<u8>, Vec<u8>)> {
        let mut run = vec![b'a'; 1000];
        run.extend(gen(200, 6));
        let mut mixed = gen(300, 3);
        mixed.extend(gen(800, 4));
        mixed.extend(gen(3000, 5));
        vec![
            ("dynamic", hex(V_DYNAMIC), gen(6000, 1)),
            ("fixed", hex(V_FIXED), gen(1500, 2)),
            ("mixed", hex(V_MIXED), mixed),
            ("run", hex(V_RUN), run),
        ]
    }

    /// LSB-first bit writer for hand-made DEFLATE streams.
    struct BitWriter {
        out: Vec<u8>,
        acc: u64,
        n: u32,
    }

    impl BitWriter {
        fn new() -> Self {
            BitWriter { out: Vec::new(), acc: 0, n: 0 }
        }
        fn put(&mut self, v: u32, k: u32) {
            self.acc |= (v as u64) << self.n;
            self.n += k;
            while self.n >= 8 {
                self.out.push(self.acc as u8);
                self.acc >>= 8;
                self.n -= 8;
            }
        }
        /// Writes a Huffman code (stored most significant bit first).
        fn code(&mut self, code: u32, len: u32) {
            self.put(code.reverse_bits() >> (32 - len), len);
        }
        fn fixed_lit(&mut self, sym: u32) {
            match sym {
                0..=143 => self.code(0x30 + sym, 8),
                144..=255 => self.code(0x190 + sym - 144, 9),
                256..=279 => self.code(sym - 256, 7),
                _ => self.code(0xC0 + sym - 280, 8),
            }
        }
        fn fixed_match(&mut self, len: usize, dist: usize) {
            let li = LEN_BASE.iter().rposition(|&b| b as usize <= len).unwrap();
            self.fixed_lit(257 + li as u32);
            self.put((len - LEN_BASE[li] as usize) as u32, LEN_EXTRA[li] as u32);
            let di = DIST_BASE.iter().rposition(|&b| b as usize <= dist).unwrap();
            self.code(di as u32, 5);
            self.put((dist - DIST_BASE[di] as usize) as u32, DIST_EXTRA[di] as u32);
        }
        fn align(&mut self) {
            if self.n > 0 {
                self.put(0, 8 - self.n);
            }
        }
        fn finish(mut self) -> Vec<u8> {
            self.align();
            self.out
        }
    }

    fn zlib_wrap(raw: &[u8], content: &[u8]) -> Vec<u8> {
        let mut v = vec![0x78, 0x01];
        v.extend_from_slice(raw);
        v.extend_from_slice(&tu::adler32(content).to_be_bytes());
        v
    }

    #[test]
    fn python_vectors_decode() {
        let v = vectors();
        // Make sure each vector really starts with the block type it is meant to cover.
        assert_eq!((v[0].1[2] >> 1) & 3, 2, "dynamic vector must start with a dynamic block");
        assert_eq!((v[1].1[2] >> 1) & 3, 1, "fixed vector must start with a fixed block");
        assert_eq!((v[2].1[2] >> 1) & 3, 0, "mixed vector must start with a stored block");
        for (name, z, want) in &v {
            let got = zlib_decompress(z, 1 << 20).unwrap_or_else(|| panic!("{} failed", name));
            assert_eq!(&got, want, "{}", name);
            // Exact output limit works, one byte less fails.
            assert!(zlib_decompress(z, want.len()).is_some(), "{} at exact limit", name);
            assert!(zlib_decompress(z, want.len() - 1).is_none(), "{} over limit", name);
            // Raw DEFLATE body without header/checksum.
            let raw = inflate(&z[2..z.len() - 4], 1 << 20).unwrap();
            assert_eq!(&raw, want, "{} raw", name);
            // Every truncation fails cleanly.
            for cut in 0..z.len() {
                assert!(zlib_decompress(&z[..cut], 1 << 20).is_none(), "{} cut at {}", name, cut);
            }
            // A wrong checksum is rejected; the known-size variant (used for PNG)
            // accepts it, and also a stream whose checksum is missing.
            let mut bad = z.clone();
            *bad.last_mut().unwrap() ^= 1;
            assert!(zlib_decompress(&bad, 1 << 20).is_none(), "{} bad adler", name);
            assert_eq!(zlib_decompress_known_size(&bad, want.len()).as_ref(), Some(want), "{} unchecked", name);
            assert_eq!(
                zlib_decompress_known_size(&z[..z.len() - 4], want.len()).as_ref(),
                Some(want),
                "{} no adler",
                name
            );
            assert!(zlib_decompress_known_size(&z[..z.len() - 5], want.len()).is_none(), "{} cut deflate", name);
            assert!(zlib_decompress_known_size(z, want.len() - 1).is_none(), "{} known size too small", name);
        }
    }

    #[test]
    fn stored_blocks() {
        for n in [0usize, 1, 2, 7, 100, 65535, 65536, 70000, 200_000] {
            let data = gen(n, n as u64 + 7);
            let z = tu::zlib_stored(&data);
            assert_eq!(zlib_decompress(&z, n).as_deref(), Some(&data[..]), "stored {}", n);
        }
        // NLEN must be the complement of LEN.
        let mut z = tu::zlib_stored(b"hello");
        z[5] ^= 1;
        assert!(zlib_decompress(&z, 100).is_none());
    }

    #[test]
    fn fixed_blocks_random_round_trip() {
        // Random literals and matches (all lengths/distances) through a fixed-Huffman encoder.
        let mut rng = tu::Rng(0x1234_5678);
        for round in 0..300 {
            let mut w = BitWriter::new();
            let mut want: Vec<u8> = Vec::new();
            w.put(1, 1);
            w.put(1, 2);
            let ops = 1 + rng.below(400);
            for _ in 0..ops {
                if want.is_empty() || rng.below(3) == 0 {
                    let b = rng.below(256) as u8;
                    w.fixed_lit(b as u32);
                    want.push(b);
                } else {
                    let len = 3 + rng.below(256);
                    let dist = 1 + rng.below(want.len().min(32768));
                    w.fixed_match(len, dist);
                    for _ in 0..len {
                        let b = want[want.len() - dist];
                        want.push(b);
                    }
                }
            }
            w.fixed_lit(256);
            let raw = w.finish();
            assert_eq!(inflate(&raw, want.len()).as_deref(), Some(&want[..]), "round {}", round);
            let z = zlib_wrap(&raw, &want);
            assert_eq!(zlib_decompress(&z, want.len()).as_deref(), Some(&want[..]), "round {}", round);
        }
    }

    #[test]
    fn output_grows_beyond_first_buffer() {
        // 4 MB of zeros from ~25 KB of input: far beyond the first guess (4 x input +
        // 64 KB), so the buffer has to grow several times, also right up to the limit.
        let mut w = BitWriter::new();
        w.put(1, 1);
        w.put(1, 2);
        w.fixed_lit(0);
        let mut n = 1;
        while n + 258 <= 4 << 20 {
            w.fixed_match(258, 1);
            n += 258;
        }
        w.fixed_lit(256);
        let raw = w.finish();
        let want = vec![0u8; n];
        assert_eq!(inflate(&raw, 1 << 30).as_deref(), Some(&want[..]));
        assert_eq!(inflate(&raw, n).as_deref(), Some(&want[..]));
        assert!(inflate(&raw, n - 1).is_none());
        let z = zlib_wrap(&raw, &want);
        assert_eq!(zlib_decompress(&z, 1 << 30).as_deref(), Some(&want[..]));
        assert_eq!(zlib_decompress_known_size(&z, n).as_deref(), Some(&want[..]));
    }

    #[test]
    fn longest_distance_and_length() {
        // 32768 stored bytes, then a 258-byte copy from exactly 32768 bytes back.
        let first = gen(32768, 99);
        let mut w = BitWriter::new();
        w.put(0, 1);
        w.put(0, 2);
        w.align();
        w.put(32768, 16);
        w.put(!32768u32 & 0xFFFF, 16);
        for &b in &first {
            w.put(b as u32, 8);
        }
        w.put(1, 1);
        w.put(1, 2);
        w.fixed_match(258, 32768);
        w.fixed_match(258, 1);
        w.fixed_lit(256);
        let raw = w.finish();
        let mut want = first.clone();
        want.extend_from_slice(&first[..258]);
        let last = *want.last().unwrap();
        want.extend(std::iter::repeat(last).take(258));
        assert_eq!(inflate(&raw, 1 << 20).as_deref(), Some(&want[..]));
    }

    /// Starts a dynamic block header: final bit, type 2, HLIT/HDIST/HCLEN fields.
    fn dynamic_header(w: &mut BitWriter, hlit: u32, hdist: u32, clen: &[u32]) {
        w.put(1, 1);
        w.put(2, 2);
        w.put(hlit - 257, 5);
        w.put(hdist - 1, 5);
        w.put(clen.len() as u32 - 4, 4);
        for &l in clen {
            w.put(l, 3);
        }
    }

    #[test]
    fn error_cases() {
        let ok = hex(V_FIXED);
        assert!(zlib_decompress(&ok, 1 << 20).is_some());
        // Headers: wrong method, bad check bits, window > 32K, preset dictionary.
        for head in [[0x77u8, 0x01], [0x78, 0x02], [0x88, 0x1C], [0x78, 0xBB]] {
            let mut z = ok.clone();
            z[..2].copy_from_slice(&head);
            assert!(zlib_decompress(&z, 1 << 20).is_none(), "header {:02X?}", head);
        }
        assert!(zlib_decompress(&[], 10).is_none());
        assert!(zlib_decompress(&[0x78], 10).is_none());
        assert!(zlib_decompress(&[0x78, 0x01], 10).is_none());
        assert!(inflate(&[], 10).is_none());

        // Block type 3.
        assert!(inflate(&[0x07, 0, 0, 0], 10).is_none());

        // Distance further back than the output so far.
        let mut w = BitWriter::new();
        w.put(1, 1);
        w.put(1, 2);
        w.fixed_lit(b'a' as u32);
        w.fixed_match(3, 2);
        w.fixed_lit(256);
        assert!(inflate(&w.finish(), 100).is_none(), "distance too far");

        // Invalid literal/length symbols 286 and 287.
        for sym in [286, 287] {
            let mut w = BitWriter::new();
            w.put(1, 1);
            w.put(1, 2);
            w.fixed_lit(b'a' as u32);
            w.fixed_lit(sym);
            w.put(0, 16);
            w.fixed_lit(256);
            assert!(inflate(&w.finish(), 1000).is_none(), "length symbol {}", sym);
        }
        // Invalid distance symbols 30 and 31.
        for d in [30, 31] {
            let mut w = BitWriter::new();
            w.put(1, 1);
            w.put(1, 2);
            w.fixed_lit(b'a' as u32);
            w.fixed_lit(257);
            w.code(d, 5);
            w.put(0, 16);
            w.fixed_lit(256);
            assert!(inflate(&w.finish(), 1000).is_none(), "distance symbol {}", d);
        }

        // HLIT > 286 and HDIST > 30.
        let mut w = BitWriter::new();
        dynamic_header(&mut w, 287, 1, &[0, 0, 0, 0]);
        assert!(inflate(&w.finish(), 1000).is_none(), "hlit 287");
        let mut w = BitWriter::new();
        dynamic_header(&mut w, 257, 31, &[0, 0, 0, 0]);
        assert!(inflate(&w.finish(), 1000).is_none(), "hdist 31");

        // Repeat code 16 with no previous length. Code-length code: 16 -> '0', 17 -> '1'.
        let mut w = BitWriter::new();
        dynamic_header(&mut w, 257, 1, &[1, 1, 0, 0]);
        w.code(0, 1);
        w.put(0, 2);
        w.put(0, 32);
        assert!(inflate(&w.finish(), 1000).is_none(), "repeat first");

        // Over-subscribed code-length code: three codes of length 1.
        let mut w = BitWriter::new();
        dynamic_header(&mut w, 257, 1, &[1, 1, 1, 0]);
        w.put(0, 32);
        assert!(inflate(&w.finish(), 1000).is_none(), "oversubscribed");

        // All lengths zero, so no end-of-block code. Code-length code: 0 -> '0', 18 -> '1'.
        let mut w = BitWriter::new();
        dynamic_header(&mut w, 257, 1, &[0, 0, 1, 1]);
        w.code(1, 1);
        w.put(127, 7); // 138 zeros
        w.code(1, 1);
        w.put(109, 7); // 120 zeros -> 258 lengths in total
        w.put(0, 32);
        assert!(inflate(&w.finish(), 1000).is_none(), "missing end-of-block");

        // Too many repeated lengths (past HLIT + HDIST).
        let mut w = BitWriter::new();
        dynamic_header(&mut w, 257, 1, &[0, 0, 1, 1]);
        for _ in 0..3 {
            w.code(1, 1);
            w.put(127, 7);
        }
        w.put(0, 32);
        assert!(inflate(&w.finish(), 1000).is_none(), "too many lengths");

        // Output limit.
        assert!(zlib_decompress(&ok, 0).is_none());
    }

    #[test]
    fn fuzz_vectors_no_panic() {
        let mut files: Vec<(String, Vec<u8>)> = vectors().into_iter().map(|(n, z, _)| (n.to_string(), z)).collect();
        // Real zlib streams (dynamic blocks) from 30 sample PNGs.
        for p in tu::samples().png.iter().step_by(10) {
            if let Ok(d) = std::fs::read(p) {
                files.push((tu::short(p), idat(&d)));
            }
        }
        // Random garbage, too (half of it behind a valid zlib header).
        let mut rng = tu::Rng(77);
        for i in 0..20 {
            let mut g: Vec<u8> = (0..1 + rng.below(3000)).map(|_| rng.next() as u8).collect();
            if i % 2 == 0 && g.len() > 2 {
                g[0] = 0x78;
                g[1] = 0x9C;
            }
            files.push((format!("garbage{}", i), g));
        }
        let f = |d: &[u8]| {
            let _ = zlib_decompress(d, 1 << 20);
            let _ = inflate(d, 1 << 20);
            let _ = inflate(d.get(2..).unwrap_or(&[]), 1 << 20);
        };
        let (n, cases, fails) = tu::fuzz_files(&files, &f);
        println!("inflate fuzz: {} inputs, {} cases, {} panics", n, cases, fails.len());
        assert!(fails.is_empty(), "{:#?}", fails);
    }

    /// Returns the concatenated IDAT data of a PNG file.
    fn idat(png: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut pos = 8;
        while pos + 8 <= png.len() {
            let len = u32::from_be_bytes(png[pos..pos + 4].try_into().unwrap()) as usize;
            let end = (pos + 8 + len).min(png.len());
            if &png[pos + 4..pos + 8] == b"IDAT" {
                out.extend_from_slice(&png[pos + 8..end]);
            }
            pos = pos + 12 + len;
        }
        out
    }

    #[test]
    #[cfg_attr(debug_assertions, ignore = "timing: run with --release")]
    fn speed_inflate_literals_and_matches() {
        // Worst cases for the decoder loop: only literals, and only short matches.
        let n = 8 << 20;
        let mut rng = tu::Rng(9);
        let mut w = BitWriter::new();
        w.put(1, 1);
        w.put(1, 2);
        for _ in 0..n {
            w.fixed_lit(rng.below(256) as u32);
        }
        w.fixed_lit(256);
        let literals = w.finish();
        let mut w = BitWriter::new();
        w.put(1, 1);
        w.put(1, 2);
        for _ in 0..100 {
            w.fixed_lit(rng.below(256) as u32);
        }
        let (mut m, mut count) = (100, 0);
        while m < n {
            let len = 3 + rng.below(10);
            w.fixed_match(len, 1 + rng.below(100));
            m += len;
            count += 1;
        }
        w.fixed_lit(256);
        let matches = w.finish();
        for (name, raw, size, symbols) in
            [("only literals", &literals, n, n), ("only 3-12 byte matches", &matches, m, count)]
        {
            let reps = 10;
            let t = std::time::Instant::now();
            for _ in 0..reps {
                assert_eq!(inflate(raw, size).map(|v| v.len()), Some(size));
            }
            let secs = t.elapsed().as_secs_f64() / reps as f64;
            println!(
                "inflate: {} ({} symbols) -> {} bytes in {:.2} ms = {:.0} MB/s (output), {:.1} ns/symbol",
                name,
                symbols,
                size,
                secs * 1e3,
                size as f64 / secs / 1e6,
                secs * 1e9 / symbols as f64
            );
        }
    }

    #[test]
    #[cfg_attr(debug_assertions, ignore = "timing: run with --release")]
    fn speed_inflate() {
        // The zlib streams of the largest sample PNGs.
        let mut files: Vec<_> = tu::samples().png.iter().filter_map(|p| std::fs::read(p).ok()).collect();
        files.sort_by_key(|d| std::cmp::Reverse(d.len()));
        for data in files.iter().take(3) {
            let z = idat(data);
            let out = zlib_decompress(&z, 1 << 30).expect("sample stream");
            let reps = 20;
            let t = std::time::Instant::now();
            for _ in 0..reps {
                std::hint::black_box(zlib_decompress_known_size(&z, out.len()));
            }
            let known = t.elapsed().as_secs_f64() / reps as f64;
            let t = std::time::Instant::now();
            for _ in 0..reps {
                std::hint::black_box(zlib_decompress(&z, 1 << 30));
            }
            let generic = t.elapsed().as_secs_f64() / reps as f64;
            println!(
                "inflate: sample PNG stream {} -> {} bytes: known size (PNG path) {:.2} ms = {:.0} MB/s; \
                 zlib_decompress with Adler-32 and growing buffer {:.2} ms = {:.0} MB/s",
                z.len(),
                out.len(),
                known * 1e3,
                out.len() as f64 / known / 1e6,
                generic * 1e3,
                out.len() as f64 / generic / 1e6
            );
        }
        // A large random fixed-Huffman stream (literals and short matches).
        let mut rng = tu::Rng(5);
        let mut w = BitWriter::new();
        let mut n = 0usize;
        w.put(1, 1);
        w.put(1, 2);
        while n < 8 << 20 {
            if n < 100 || rng.below(2) == 0 {
                w.fixed_lit(rng.below(256) as u32);
                n += 1;
            } else {
                let len = 3 + rng.below(20);
                w.fixed_match(len, 1 + rng.below(n.min(32768)));
                n += len;
            }
        }
        w.fixed_lit(256);
        let raw = w.finish();
        let reps = 10;
        let t = std::time::Instant::now();
        for _ in 0..reps {
            assert_eq!(inflate(&raw, n).map(|v| v.len()), Some(n));
        }
        let secs = t.elapsed().as_secs_f64() / reps as f64;
        println!(
            "inflate: synthetic fixed-Huffman stream {} -> {} bytes in {:.2} ms = {:.0} MB/s (output)",
            raw.len(),
            n,
            secs * 1e3,
            n as f64 / secs / 1e6
        );
    }
}
