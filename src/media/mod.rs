// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Length and basic stream facts of audio and video files, read from their headers.
//!
//! Supported: WAV (RIFF, WAVE_FORMAT_EXTENSIBLE, RF64/BW64), MP3 (MPEG 1/2/2.5, layers I-III,
//! Xing/Info and VBRI headers), the MP4 family (.mp4 .m4a .m4v .mov .3gp), FLAC, Ogg (Vorbis,
//! Opus, FLAC, Speex; Theora for the picture size), Matroska/WebM, AVI (including OpenDML) and
//! ASF (.wma .wmv).
//!
//! Only the needed parts of a file are read through `Read + Seek`: a few KB for most files, the
//! last 64 KB as well for Ogg, and never more than `READ_BUDGET` bytes in total. Parsing works on
//! bounded buffers with checked arithmetic and capped loops, so damaged or hostile input gives
//! `None` (or fewer facts) and never a panic or a hang.

mod asf;
mod avi;
mod flac;
mod mkv;
mod mp3;
mod mp4;
mod ogg;
#[cfg(test)]
mod tests;
mod wav;

use asf::*;
use avi::*;
use flac::*;
use mkv::*;
use mp3::*;
use mp4::*;
use ogg::*;
use wav::*;

use std::io::{Read, Seek, SeekFrom};

/// What could be read about a media file. Facts that are unknown are `None`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MediaInfo {
    pub duration_ms: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub bitrate_kbps: Option<u32>, // when the format states it or it follows from size and length
    pub has_video: bool,
    pub has_audio: bool,
}

/// Reads what it can from the start (and, for some formats, the end) of a file. `ext` is the lower-case extension
/// without the dot, used only as a hint; the content decides.
///
/// Returns `None` when the content is not a recognized format. A recognized but damaged file
/// gives `Some` with the facts that could not be read left as `None`.
pub fn read<R: std::io::Read + std::io::Seek>(r: &mut R, ext: &str) -> Option<MediaInfo> {
    let mut src = Src::new(r)?;
    let kind = sniff(&mut src, ext)?;
    let mut info = MediaInfo::default();
    // Each parser fills in what it can and stops at the first fact it cannot read; that early
    // stop is reported as `None`, which is ignored here (except for MP3, see below).
    let _ = match kind {
        Kind::Wav => wav(&mut src, &mut info),
        Kind::Avi => avi(&mut src, &mut info),
        // MP3 has no magic number, so "recognized" means a valid frame was found.
        Kind::Mp3(at) => Some(mp3(&mut src, at, &mut info)?),
        Kind::Flac(at) => flac(&mut src, at, &mut info),
        Kind::Ogg => ogg(&mut src, &mut info),
        Kind::Matroska => matroska(&mut src, &mut info),
        Kind::Mp4 => mp4(&mut src, &mut info),
        Kind::Asf => asf(&mut src, &mut info),
    };
    if info.duration_ms.is_some_and(|d| d > MAX_DURATION_MS) {
        info.duration_ms = None;
    }
    if info.bitrate_kbps.is_none() {
        // Overall bitrate from the file size and the length.
        info.bitrate_kbps = info.duration_ms.and_then(|ms| kbps(src.len, ms));
    }
    Some(info)
}

/// A duration as Windows Explorer writes it: "00:03:25" (hours always shown, as Explorer does).
pub fn format_duration(ms: u64) -> String {
    let s = ms / 1000;
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

// ---------------------------------------------------------------------------------------------
// Limits and small helpers
// ---------------------------------------------------------------------------------------------

/// Most bytes one `read` call may read from the file, all reads together.
const READ_BUDGET: u64 = 4 << 20;
/// Most separate reads one `read` call may make.
const MAX_READS: u32 = 2048;
/// Durations above this (10 000 hours) come from damaged headers and are dropped.
const MAX_DURATION_MS: u64 = 10_000 * 3600 * 1000;

/// The file being read, with a cap on how much may be read from it.
struct Src<'a, R> {
    r: &'a mut R,
    len: u64,
    budget: u64,
    reads: u32,
}

impl<'a, R: Read + Seek> Src<'a, R> {
    fn new(r: &'a mut R) -> Option<Self> {
        let len = r.seek(SeekFrom::End(0)).ok()?;
        Some(Src { r, len, budget: READ_BUDGET, reads: MAX_READS })
    }

    /// Reads up to `n` bytes at `pos`, fewer near the end of the file. `None` on an I/O error or
    /// when the read budget is used up.
    fn read_at(&mut self, pos: u64, n: usize) -> Option<Vec<u8>> {
        self.reads = self.reads.checked_sub(1)?;
        let want = u64::try_from(n).ok()?.min(self.len.saturating_sub(pos));
        if want == 0 {
            return Some(Vec::new());
        }
        self.budget = self.budget.checked_sub(want)?;
        self.r.seek(SeekFrom::Start(pos)).ok()?;
        let mut buf = Vec::with_capacity(usize::try_from(want).ok()?);
        Read::take(&mut *self.r, want).read_to_end(&mut buf).ok()?;
        Some(buf)
    }
}

/// `N` bytes at `at`, or `None` if they are not all there.
fn get<const N: usize>(b: &[u8], at: usize) -> Option<[u8; N]> {
    b.get(at..at.checked_add(N)?)?.try_into().ok()
}
fn be16(b: &[u8], at: usize) -> Option<u16> {
    get(b, at).map(u16::from_be_bytes)
}
fn be24(b: &[u8], at: usize) -> Option<u32> {
    get::<3>(b, at).map(|[x, y, z]| u32::from_be_bytes([0, x, y, z]))
}
fn be32(b: &[u8], at: usize) -> Option<u32> {
    get(b, at).map(u32::from_be_bytes)
}
fn be64(b: &[u8], at: usize) -> Option<u64> {
    get(b, at).map(u64::from_be_bytes)
}
fn le16(b: &[u8], at: usize) -> Option<u16> {
    get(b, at).map(u16::from_le_bytes)
}
fn le32(b: &[u8], at: usize) -> Option<u32> {
    get(b, at).map(u32::from_le_bytes)
}
fn le64(b: &[u8], at: usize) -> Option<u64> {
    get(b, at).map(u64::from_le_bytes)
}

/// `a * b / c` without overflow; `None` when `c` is zero or the result does not fit.
fn muldiv(a: u64, b: u64, c: u64) -> Option<u64> {
    if c == 0 {
        return None;
    }
    u64::try_from(u128::from(a) * u128::from(b) / u128::from(c)).ok()
}

/// Kilobits per second for `bytes` played over `ms` milliseconds; `None` below 1 kbps.
fn kbps(bytes: u64, ms: u64) -> Option<u32> {
    u32::try_from(muldiv(bytes, 8, ms)?).ok().and_then(positive)
}

/// A count that must be positive to mean anything.
fn positive<T: Default + PartialEq>(v: T) -> Option<T> {
    (v != T::default()).then_some(v)
}

// ---------------------------------------------------------------------------------------------
// Recognizing the format
// ---------------------------------------------------------------------------------------------

enum Kind {
    Wav,
    Avi,
    /// MP3 audio starting at or after this offset (after any ID3v2 tags).
    Mp3(u64),
    /// FLAC stream starting at this offset (after any ID3v2 tags).
    Flac(u64),
    Ogg,
    Matroska,
    Mp4,
    Asf,
}

/// Box types that may open an ISO BMFF / QuickTime file.
const MP4_FIRST_BOXES: [&[u8; 4]; 9] =
    [b"ftyp", b"moov", b"mdat", b"free", b"skip", b"wide", b"pnot", b"styp", b"sidx"];

fn sniff<R: Read + Seek>(src: &mut Src<R>, ext: &str) -> Option<Kind> {
    let head = src.read_at(0, 16)?;
    let t0 = get::<4>(&head, 0)?;
    let t4 = get::<4>(&head, 4);
    let t8 = get::<4>(&head, 8);
    match &t0 {
        b"RIFF" | b"RF64" | b"BW64" => {
            return match t8.as_ref() {
                Some(b"WAVE") => Some(Kind::Wav),
                Some(b"AVI ") if &t0 == b"RIFF" => Some(Kind::Avi),
                _ => None,
            };
        }
        b"fLaC" => return Some(Kind::Flac(0)),
        b"OggS" => return Some(Kind::Ogg),
        [0x1A, 0x45, 0xDF, 0xA3] => return Some(Kind::Matroska),
        _ => {}
    }
    if head.get(..16) == Some(&ASF_HEADER[..]) {
        return Some(Kind::Asf);
    }
    if head.starts_with(b"ID3") {
        let at = skip_id3v2(src, 0)?;
        let next = src.read_at(at, 4)?;
        return Some(if next == b"fLaC" { Kind::Flac(at) } else { Kind::Mp3(at) });
    }
    if t4.is_some_and(|t| MP4_FIRST_BOXES.contains(&&t)) {
        return Some(Kind::Mp4);
    }
    if mpeg_header(t0).is_some() {
        return Some(Kind::Mp3(0));
    }
    // Nothing matched by content: let the extension pick a format without a fixed signature.
    match ext {
        "mp3" | "mp2" | "mp1" | "mpa" | "mpga" => Some(Kind::Mp3(0)),
        "mp4" | "m4a" | "m4v" | "m4b" | "mov" | "qt" | "3gp" | "3g2" | "f4v"
            if t4.is_some_and(|t| t.iter().all(|c| c.is_ascii_graphic() || *c == b' ')) =>
        {
            Some(Kind::Mp4)
        }
        _ => None,
    }
}

/// Skips the ID3v2 tags at `pos` and returns where the data after them starts.
fn skip_id3v2<R: Read + Seek>(src: &mut Src<R>, mut pos: u64) -> Option<u64> {
    for _ in 0..4 {
        let h = src.read_at(pos, 10)?;
        if !h.starts_with(b"ID3") {
            break;
        }
        let flags = *h.get(5)?;
        let s = get::<4>(&h, 6)?;
        if s.iter().any(|b| b & 0x80 != 0) {
            break; // not a synchsafe size
        }
        // The size is "synchsafe": 7 bits per byte. It excludes the header and the footer.
        let size = s.iter().fold(0u64, |acc, &b| acc << 7 | u64::from(b));
        let footer = if flags & 0x10 != 0 { 10 } else { 0 };
        pos = pos.checked_add(10 + size + footer)?;
    }
    Some(pos)
}
