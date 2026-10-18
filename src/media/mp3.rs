// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! MP3 (MPEG audio layers I-III).
use super::*;

/// How far past the ID3 tag (or the file start) the first frame is looked for.
pub(super) const MP3_SCAN: usize = 64 * 1024;

/// Bitrates in kbps for bitrate index 1..=14, per layer (I, II, III).
pub(super) const MPEG1_KBPS: [[u16; 14]; 3] = [
    [32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448],
    [32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384],
    [32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320],
];
/// MPEG 2 and 2.5 share one table for layers II and III.
pub(super) const MPEG2_KBPS: [[u16; 14]; 3] = [
    [32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256],
    [8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
    [8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
];

/// One MPEG audio frame header, decoded.
#[derive(Clone, Copy, Debug)]
pub(super) struct Frame {
    /// 3 = MPEG 1, 2 = MPEG 2, 0 = MPEG 2.5 (the raw version bits).
    pub(super) version: u8,
    pub(super) layer: u8,
    pub(super) bitrate: u32,
    pub(super) sample_rate: u32,
    pub(super) samples: u32,
    pub(super) len: u32,
    pub(super) mono: bool,
    pub(super) crc: bool,
}

/// Decodes a 4-byte frame header; `None` if it is not a valid one.
pub(super) fn mpeg_header(h: [u8; 4]) -> Option<Frame> {
    let [b0, b1, b2, b3] = h;
    if b0 != 0xFF || b1 & 0xE0 != 0xE0 {
        return None;
    }
    let version = (b1 >> 3) & 3;
    let layer = match (b1 >> 1) & 3 {
        3 => 1,
        2 => 2,
        1 => 3,
        _ => return None,
    };
    if version == 1 || b3 & 3 == 2 {
        return None; // reserved version or emphasis
    }
    let table = if version == 3 { &MPEG1_KBPS } else { &MPEG2_KBPS };
    // Index 0 is "free format" and 15 is invalid; neither gives a usable bitrate.
    let br_index = usize::from(b2 >> 4).checked_sub(1)?;
    let kbps = *table.get(usize::from(layer - 1))?.get(br_index)?;
    let base = *[44100u32, 48000, 32000].get(usize::from((b2 >> 2) & 3))?;
    let sample_rate = match version {
        3 => base,
        2 => base / 2,
        _ => base / 4,
    };
    let samples = match layer {
        1 => 384,
        2 => 1152,
        _ if version == 3 => 1152,
        _ => 576,
    };
    let bitrate = u32::from(kbps) * 1000;
    let pad = u32::from((b2 >> 1) & 1);
    let len =
        if layer == 1 { (12 * bitrate / sample_rate + pad) * 4 } else { samples / 8 * bitrate / sample_rate + pad };
    Some(Frame { version, layer, bitrate, sample_rate, samples, len, mono: b3 >> 6 == 3, crc: b1 & 1 == 0 })
}

/// Finds the first frame whose successor (or the end of the file) confirms it.
pub(super) fn find_frame(buf: &[u8], buf_ends_file: bool) -> Option<(usize, Frame)> {
    for i in 0..buf.len().min(MP3_SCAN) {
        if buf.get(i) != Some(&0xFF) {
            continue;
        }
        let Some(f) = get::<4>(buf, i).and_then(mpeg_header) else { continue };
        let next = i.checked_add(usize::try_from(f.len).ok()?)?;
        // `rest` is `None` when the frame runs past the buffer.
        let rest = buf.get(next..);
        if let Some(g) = rest.and_then(|r| get::<4>(r, 0)).and_then(mpeg_header) {
            if g.version == f.version && g.layer == f.layer && g.sample_rate == f.sample_rate {
                return Some((i, f));
            }
        }
        // The last, whole frame of the file, possibly followed by a tag.
        if let Some(r) = rest.filter(|_| buf_ends_file) {
            if r.len() < 4 || r.starts_with(b"TAG") || r.starts_with(b"APETAGEX") {
                return Some((i, f));
            }
        }
    }
    None
}

pub(super) fn mp3<R: Read + Seek>(src: &mut Src<R>, start: u64, info: &mut MediaInfo) -> Option<()> {
    let buf = src.read_at(start, MP3_SCAN + 4096)?;
    let ends_file = start.saturating_add(buf.len() as u64) >= src.len;
    let (off, f) = find_frame(&buf, ends_file)?;
    let frame_pos = start.checked_add(off as u64)?;
    info.has_audio = true;
    info.sample_rate = Some(f.sample_rate);
    info.channels = Some(if f.mono { 1 } else { 2 });
    let audio_bytes = mp3_audio_end(src).saturating_sub(frame_pos);
    let frame = buf.get(off..).unwrap_or(&[]);
    let vbr = if f.layer == 3 { xing(frame, &f).or_else(|| vbri(frame)) } else { None };
    match vbr {
        Some((frames, bytes)) if frames > 0 => {
            // VBR (or a LAME "Info" CBR header): the frame count is exact.
            let ms = muldiv(u64::from(frames) * u64::from(f.samples), 1000, f.sample_rate.into());
            let bytes = bytes.filter(|&b| b > 0).map_or(audio_bytes, u64::from);
            info.duration_ms = ms;
            info.bitrate_kbps = ms.and_then(|ms| kbps(bytes, ms));
        }
        _ => {
            // No VBR header: assume a constant bitrate, as the first frame states.
            info.duration_ms = muldiv(audio_bytes, 8000, f.bitrate.into());
            info.bitrate_kbps = Some(f.bitrate / 1000);
        }
    }
    Some(())
}

/// Frame count and byte count from a Xing or LAME "Info" header in the first frame.
pub(super) fn xing(frame: &[u8], f: &Frame) -> Option<(u32, Option<u32>)> {
    // The header follows the side information, whose size depends on version and channels.
    let side = match (f.version == 3, f.mono) {
        (true, false) => 32,
        (true, true) | (false, false) => 17,
        (false, true) => 9,
    };
    let offsets: &[usize] = if f.crc { &[4 + side, 6 + side] } else { &[4 + side] };
    for &at in offsets {
        let tag = frame.get(at..at + 4);
        if tag != Some(b"Xing") && tag != Some(b"Info") {
            continue;
        }
        let flags = be32(frame, at + 4)?;
        let mut p = at + 8;
        let frames = if flags & 1 != 0 {
            p += 4;
            be32(frame, p - 4)
        } else {
            None
        };
        let bytes = if flags & 2 != 0 { be32(frame, p) } else { None };
        return Some((frames?, bytes));
    }
    None
}

/// Frame count and byte count from a Fraunhofer VBRI header (always 32 bytes after the header).
pub(super) fn vbri(frame: &[u8]) -> Option<(u32, Option<u32>)> {
    if frame.get(36..40) != Some(b"VBRI") {
        return None;
    }
    Some((be32(frame, 50)?, be32(frame, 46)))
}

/// Where the audio ends: the file length minus an ID3v1 tag and an APEv2 tag at the end.
pub(super) fn mp3_audio_end<R: Read + Seek>(src: &mut Src<R>) -> u64 {
    let mut end = src.len;
    if end >= 128 && src.read_at(end - 128, 3).is_some_and(|t| t == b"TAG") {
        end -= 128;
    }
    if end >= 32 {
        if let Some(footer) = src.read_at(end - 32, 32) {
            if footer.starts_with(b"APETAGEX") {
                let size = le32(&footer, 12).map_or(0, u64::from); // includes the footer
                let header = if le32(&footer, 20).is_some_and(|f| f & 0x8000_0000 != 0) { 32 } else { 0 };
                end = end.checked_sub(size + header).unwrap_or(end);
            }
        }
    }
    end
}
