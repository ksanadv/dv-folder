// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! MP4 family (ISO BMFF / QuickTime boxes).
use super::*;

/// Most box headers looked at in one file.
pub(super) const MP4_MAX_BOXES: u32 = 1024;
/// Most tracks looked at.
pub(super) const MP4_MAX_TRACKS: usize = 64;

pub(super) struct Mp4Box {
    pub(super) kind: [u8; 4],
    pub(super) body: u64,
    pub(super) end: u64,
}

/// Reads the box header at `pos` inside a parent ending at `end`; also returns where the next
/// box starts.
pub(super) fn mp4_box<R: Read + Seek>(src: &mut Src<R>, pos: u64, end: u64) -> Option<(Mp4Box, u64)> {
    if pos.checked_add(8)? > end {
        return None;
    }
    let h = src.read_at(pos, 16)?;
    let size32 = be32(&h, 0)?;
    let kind = get::<4>(&h, 4)?;
    let (head_len, size) = match size32 {
        0 => (8, end - pos),     // runs to the end of the parent
        1 => (16, be64(&h, 8)?), // 64-bit size follows the type
        s => (8, u64::from(s)),
    };
    if size < head_len {
        return None;
    }
    let next = pos.checked_add(size)?;
    // A box running past its parent (a cut-off file) is read as far as the parent goes.
    Some((Mp4Box { kind, body: pos + head_len, end: next.min(end) }, next))
}

/// The child boxes of `parent`, while the shared box allowance lasts.
pub(super) fn mp4_children<R: Read + Seek>(src: &mut Src<R>, parent: &Mp4Box, left: &mut u32) -> Vec<Mp4Box> {
    let mut out = Vec::new();
    let mut pos = parent.body;
    while *left > 0 {
        *left -= 1;
        let Some((b, next)) = mp4_box(src, pos, parent.end) else { break };
        out.push(b);
        pos = next;
    }
    out
}

/// What one `trak` box says.
#[derive(Default)]
pub(super) struct Mp4Track {
    pub(super) handler: [u8; 4],
    pub(super) tkhd_width: u32,
    pub(super) tkhd_height: u32,
    pub(super) timescale: u32,
    /// First bytes of the stsd box (the first sample description).
    pub(super) stsd: Vec<u8>,
}

pub(super) fn mp4<R: Read + Seek>(src: &mut Src<R>, info: &mut MediaInfo) -> Option<()> {
    let mut left = MP4_MAX_BOXES;
    // moov may be anywhere among the top-level boxes, often after a huge mdat.
    let mut pos = 0u64;
    let moov = loop {
        left = left.checked_sub(1)?;
        let (b, next) = mp4_box(src, pos, src.len)?;
        if &b.kind == b"moov" {
            break b;
        }
        pos = next;
    };
    let mut timescale = 0u64;
    let mut duration = None;
    let mut fragment_duration = None;
    let mut tracks = Vec::new();
    for b in mp4_children(src, &moov, &mut left) {
        match &b.kind {
            b"mvhd" => {
                if let Some(d) = src.read_at(b.body, 32) {
                    // Version 0 has 32-bit times, version 1 64-bit; all ones means unknown.
                    let (ts, dur) = match d.first() {
                        Some(0) => (be32(&d, 12), be32(&d, 16).filter(|&v| v != u32::MAX).map(u64::from)),
                        Some(1) => (be32(&d, 20), be64(&d, 24).filter(|&v| v != u64::MAX)),
                        _ => (None, None),
                    };
                    timescale = ts.map_or(0, u64::from);
                    duration = dur;
                }
            }
            b"trak" if tracks.len() < MP4_MAX_TRACKS => tracks.push(mp4_track(src, &b, &mut left)),
            b"mvex" => {
                // Fragmented files may give the length only in mvex > mehd.
                for m in mp4_children(src, &b, &mut left) {
                    if &m.kind == b"mehd" {
                        fragment_duration = src.read_at(m.body, 12).and_then(|d| match d.first() {
                            Some(0) => be32(&d, 4).map(u64::from),
                            Some(1) => be64(&d, 4),
                            _ => None,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    let units = duration.filter(|&d| d > 0).or(fragment_duration);
    info.duration_ms = units.and_then(|d| muldiv(d, 1000, timescale));

    for t in &tracks {
        match &t.handler {
            b"vide" if !info.has_video => {
                info.has_video = true;
                // tkhd sizes are 16.16 fixed point; the sample description has whole pixels.
                let (w, h) = if t.tkhd_width > 0 && t.tkhd_height > 0 {
                    (t.tkhd_width, t.tkhd_height)
                } else {
                    (be16(&t.stsd, 40).map_or(0, u32::from), be16(&t.stsd, 42).map_or(0, u32::from))
                };
                info.width = positive(w);
                info.height = positive(h);
            }
            b"soun" if !info.has_audio => {
                info.has_audio = true;
                // Audio sample entry: channels at 32, rate (16.16) at 40. QuickTime version 2
                // entries keep the real values further on, as a float rate and a 32-bit count.
                let (rate, channels) = if be16(&t.stsd, 24) == Some(2) {
                    let rate = be64(&t.stsd, 48).map(f64::from_bits).filter(|r| r.is_finite() && *r >= 1.0 && *r < 1e7);
                    (rate.map(|r| r.round() as u32), be32(&t.stsd, 56).and_then(|c| u16::try_from(c).ok()))
                } else {
                    (be16(&t.stsd, 40).map(u32::from), be16(&t.stsd, 32))
                };
                info.sample_rate = rate.filter(|&r| r >= 1000).or(positive(t.timescale));
                info.channels = channels.and_then(positive);
            }
            _ => {}
        }
    }
    Some(())
}

/// Reads tkhd, and mdia > hdlr, mdhd and minf > stbl > stsd, of one track.
pub(super) fn mp4_track<R: Read + Seek>(src: &mut Src<R>, trak: &Mp4Box, left: &mut u32) -> Mp4Track {
    let mut t = Mp4Track::default();
    for b in mp4_children(src, trak, left) {
        match &b.kind {
            b"tkhd" => {
                if let Some(d) = src.read_at(b.body, 96) {
                    let at = match d.first() {
                        Some(0) => 76,
                        Some(1) => 88,
                        _ => continue,
                    };
                    t.tkhd_width = be32(&d, at).map_or(0, |v| v >> 16);
                    t.tkhd_height = be32(&d, at + 4).map_or(0, |v| v >> 16);
                }
            }
            b"mdia" => {
                for m in mp4_children(src, &b, left) {
                    match &m.kind {
                        // Only the hdlr under mdia names the track type (QuickTime has another in minf).
                        b"hdlr" => {
                            if let Some(h) = src.read_at(m.body, 12).and_then(|d| get::<4>(&d, 8)) {
                                t.handler = h;
                            }
                        }
                        b"mdhd" => {
                            if let Some(d) = src.read_at(m.body, 24) {
                                let at = if d.first() == Some(&1) { 20 } else { 12 };
                                t.timescale = be32(&d, at).unwrap_or(0);
                            }
                        }
                        b"minf" => {
                            for s in mp4_children(src, &m, left) {
                                if &s.kind != b"stbl" {
                                    continue;
                                }
                                for x in mp4_children(src, &s, left) {
                                    if &x.kind == b"stsd" {
                                        let n = x.end.saturating_sub(x.body).min(64) as usize;
                                        t.stsd = src.read_at(x.body, n).unwrap_or_default();
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    t
}
