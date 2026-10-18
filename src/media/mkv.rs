// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Matroska / WebM (EBML).
use super::*;

pub(super) const MKV_EBML: u32 = 0x1A45_DFA3;
pub(super) const MKV_SEGMENT: u32 = 0x1853_8067;
pub(super) const MKV_SEEKHEAD: u32 = 0x114D_9B74;
pub(super) const MKV_SEEK: u32 = 0x4DBB;
pub(super) const MKV_SEEK_ID: u32 = 0x53AB;
pub(super) const MKV_SEEK_POSITION: u32 = 0x53AC;
pub(super) const MKV_INFO: u32 = 0x1549_A966;
pub(super) const MKV_TIMECODE_SCALE: u32 = 0x2A_D7B1;
pub(super) const MKV_DURATION: u32 = 0x4489;
pub(super) const MKV_TRACKS: u32 = 0x1654_AE6B;
pub(super) const MKV_TRACK_ENTRY: u32 = 0xAE;
pub(super) const MKV_TRACK_TYPE: u32 = 0x83;
pub(super) const MKV_CODEC_ID: u32 = 0x86;
pub(super) const MKV_VIDEO: u32 = 0xE0;
pub(super) const MKV_PIXEL_WIDTH: u32 = 0xB0;
pub(super) const MKV_PIXEL_HEIGHT: u32 = 0xBA;
pub(super) const MKV_AUDIO: u32 = 0xE1;
pub(super) const MKV_SAMPLING_FREQUENCY: u32 = 0xB5;
pub(super) const MKV_CHANNELS: u32 = 0x9F;
pub(super) const MKV_CLUSTER: u32 = 0x1F43_B675;

/// Most of an Info or SeekHead element that is read.
pub(super) const MKV_SMALL_CAP: u64 = 64 * 1024;
/// Most of a Tracks element that is read (codec private data can make it large).
pub(super) const MKV_TRACKS_CAP: u64 = 1 << 20;

/// Decodes an element header at `at`: (id, header length, payload size; `None` = unknown size).
pub(super) fn ebml_head(b: &[u8], at: usize) -> Option<(u32, usize, Option<u64>)> {
    // The ID keeps its length marker: 1 to 4 bytes.
    let first = *b.get(at)?;
    let id_len = first.leading_zeros() as usize + 1;
    if id_len > 4 {
        return None;
    }
    let mut id = 0u32;
    for k in 0..id_len {
        id = id << 8 | u32::from(*b.get(at.checked_add(k)?)?);
    }
    // The size drops its marker: 1 to 8 bytes; all value bits set means "unknown".
    let size_at = at.checked_add(id_len)?;
    let s0 = *b.get(size_at)?;
    let size_len = s0.leading_zeros() as usize + 1;
    if size_len > 8 {
        return None;
    }
    let mask = (0xFFu16 >> size_len) as u8; // u16: shifting a u8 by 8 would overflow
    let mut size = u64::from(s0 & mask);
    let mut all_ones = s0 & mask == mask;
    for k in 1..size_len {
        let v = *b.get(size_at.checked_add(k)?)?;
        size = size << 8 | u64::from(v);
        all_ones &= v == 0xFF;
    }
    Some((id, id_len + size_len, (!all_ones).then_some(size)))
}

/// The child elements in a buffer, as (id, payload). Payloads are cut at the buffer end.
pub(super) fn ebml_children(b: &[u8]) -> Vec<(u32, &[u8])> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < b.len() && out.len() < 1024 {
        let Some((id, head_len, size)) = ebml_head(b, at) else { break };
        let Some(start) = at.checked_add(head_len).filter(|&s| s <= b.len()) else { break };
        let end = match size.map(usize::try_from) {
            Some(Ok(s)) => start.saturating_add(s).min(b.len()),
            _ => b.len(),
        };
        out.push((id, b.get(start..end).unwrap_or(&[])));
        at = end;
    }
    out
}

pub(super) fn ebml_uint(d: &[u8]) -> Option<u64> {
    if d.len() > 8 {
        return None;
    }
    Some(d.iter().fold(0u64, |acc, &b| acc << 8 | u64::from(b)))
}

pub(super) fn ebml_float(d: &[u8]) -> Option<f64> {
    let v = match d.len() {
        0 => 0.0,
        4 => f64::from(f32::from_be_bytes(d.try_into().ok()?)),
        8 => f64::from_be_bytes(d.try_into().ok()?),
        _ => return None,
    };
    v.is_finite().then_some(v)
}

/// Reads the element header at `pos` in the file: (id, payload position, payload size).
pub(super) fn ebml_at<R: Read + Seek>(src: &mut Src<R>, pos: u64) -> Option<(u32, u64, Option<u64>)> {
    let b = src.read_at(pos, 12)?;
    let (id, head_len, size) = ebml_head(&b, 0)?;
    Some((id, pos.checked_add(head_len as u64)?, size))
}

/// Reads an element's payload, at most `cap` bytes of it.
pub(super) fn ebml_payload<R: Read + Seek>(src: &mut Src<R>, at: u64, size: Option<u64>, cap: u64) -> Option<Vec<u8>> {
    src.read_at(at, size.unwrap_or(cap).min(cap) as usize)
}

pub(super) fn matroska<R: Read + Seek>(src: &mut Src<R>, info: &mut MediaInfo) -> Option<()> {
    let (id, data, size) = ebml_at(src, 0)?;
    if id != MKV_EBML {
        return None;
    }
    // Find the Segment after the EBML header.
    let mut pos = data.checked_add(size?)?;
    let mut segment = None;
    for _ in 0..16 {
        let (id, data, size) = ebml_at(src, pos)?;
        if id == MKV_SEGMENT {
            segment = Some((data, size));
            break;
        }
        pos = data.checked_add(size?)?;
    }
    let (seg, seg_size) = segment?;
    let seg_end = seg_size.and_then(|s| seg.checked_add(s)).map_or(src.len, |e| e.min(src.len));

    // Info and Tracks normally come before the first Cluster; if not, the SeekHead says where.
    let (mut have_info, mut have_tracks) = (false, false);
    let (mut seek_info, mut seek_tracks) = (None, None);
    let mut pos = seg;
    for _ in 0..256 {
        if pos >= seg_end || (have_info && have_tracks) {
            break;
        }
        let Some((id, data, size)) = ebml_at(src, pos) else { break };
        match id {
            MKV_CLUSTER => break,
            MKV_SEEKHEAD => {
                if let Some(b) = ebml_payload(src, data, size, MKV_SMALL_CAP) {
                    for (target, at) in mkv_seeks(&b) {
                        match target {
                            MKV_INFO => seek_info = seek_info.or(Some(at)),
                            MKV_TRACKS => seek_tracks = seek_tracks.or(Some(at)),
                            _ => {}
                        }
                    }
                }
            }
            MKV_INFO => {
                if let Some(b) = ebml_payload(src, data, size, MKV_SMALL_CAP) {
                    mkv_info(&b, info);
                    have_info = true;
                }
            }
            MKV_TRACKS => {
                if let Some(b) = ebml_payload(src, data, size, MKV_TRACKS_CAP) {
                    mkv_tracks(&b, info);
                    have_tracks = true;
                }
            }
            _ => {}
        }
        // An element of unknown size cannot be stepped over.
        let Some(next) = size.and_then(|s| data.checked_add(s)) else { break };
        if next <= pos {
            break;
        }
        pos = next;
    }
    // Seek positions are relative to the start of the Segment's payload.
    if !have_info {
        if let Some((MKV_INFO, data, size)) = seek_info.and_then(|p| ebml_at(src, seg.checked_add(p)?)) {
            if let Some(b) = ebml_payload(src, data, size, MKV_SMALL_CAP) {
                mkv_info(&b, info);
            }
        }
    }
    if !have_tracks {
        if let Some((MKV_TRACKS, data, size)) = seek_tracks.and_then(|p| ebml_at(src, seg.checked_add(p)?)) {
            if let Some(b) = ebml_payload(src, data, size, MKV_TRACKS_CAP) {
                mkv_tracks(&b, info);
            }
        }
    }
    Some(())
}

/// The (element id, position) pairs listed in a SeekHead.
pub(super) fn mkv_seeks(b: &[u8]) -> Vec<(u32, u64)> {
    let mut out = Vec::new();
    for (id, seek) in ebml_children(b) {
        if id != MKV_SEEK {
            continue;
        }
        let (mut target, mut at) = (None, None);
        for (cid, c) in ebml_children(seek) {
            match cid {
                MKV_SEEK_ID => target = ebml_uint(c).and_then(|v| u32::try_from(v).ok()),
                MKV_SEEK_POSITION => at = ebml_uint(c),
                _ => {}
            }
        }
        if let (Some(t), Some(a)) = (target, at) {
            out.push((t, a));
        }
    }
    out
}

/// Segment Info: the duration is a float in units of TimecodeScale nanoseconds.
pub(super) fn mkv_info(b: &[u8], info: &mut MediaInfo) {
    let mut scale = 1_000_000u64; // the default: milliseconds
    let mut duration = None;
    // Duration may come before TimecodeScale, so collect both first.
    for (id, c) in ebml_children(b) {
        match id {
            MKV_TIMECODE_SCALE => {
                if let Some(s) = ebml_uint(c).and_then(positive) {
                    scale = s;
                }
            }
            MKV_DURATION => duration = ebml_float(c),
            _ => {}
        }
    }
    if let Some(d) = duration {
        let ms = d * scale as f64 / 1e6;
        if ms.is_finite() && (0.0..MAX_DURATION_MS as f64).contains(&ms) {
            info.duration_ms = Some(ms.round() as u64);
        }
    }
}

/// Tracks: the first video track gives the picture size, the first audio track the sound.
pub(super) fn mkv_tracks(b: &[u8], info: &mut MediaInfo) {
    for (id, entry) in ebml_children(b) {
        if id != MKV_TRACK_ENTRY {
            continue;
        }
        let mut kind = None;
        let mut codec_kind = None;
        let (mut width, mut height, mut rate, mut channels) = (None, None, None, None);
        for (cid, c) in ebml_children(entry) {
            match cid {
                MKV_TRACK_TYPE => kind = ebml_uint(c),
                MKV_CODEC_ID if c.starts_with(b"V_") => codec_kind = Some(1),
                MKV_CODEC_ID if c.starts_with(b"A_") => codec_kind = Some(2),
                MKV_VIDEO => {
                    for (vid, v) in ebml_children(c) {
                        let n = ebml_uint(v).and_then(|n| u32::try_from(n).ok()).and_then(positive);
                        match vid {
                            MKV_PIXEL_WIDTH => width = n,
                            MKV_PIXEL_HEIGHT => height = n,
                            _ => {}
                        }
                    }
                }
                MKV_AUDIO => {
                    for (aid, a) in ebml_children(c) {
                        match aid {
                            MKV_SAMPLING_FREQUENCY => {
                                rate =
                                    ebml_float(a).filter(|f| *f >= 1.0 && *f < 10_000_000.0).map(|f| f.round() as u32);
                            }
                            MKV_CHANNELS => {
                                channels = ebml_uint(a).and_then(|n| u16::try_from(n).ok()).and_then(positive);
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
        match kind.or(codec_kind) {
            Some(1) => {
                info.has_video = true;
                if info.width.is_none() && info.height.is_none() {
                    info.width = width;
                    info.height = height;
                }
            }
            Some(2) => {
                info.has_audio = true;
                if info.sample_rate.is_none() && info.channels.is_none() {
                    // Matroska's default sampling frequency is 8000 Hz and channel count 1.
                    info.sample_rate = Some(rate.unwrap_or(8000));
                    info.channels = Some(channels.unwrap_or(1));
                }
            }
            _ => {}
        }
    }
}
