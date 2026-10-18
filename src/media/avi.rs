// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! AVI (RIFF AVI, including OpenDML).
use super::*;

/// Most of the hdrl list that is read.
pub(super) const AVI_HDRL_CAP: u64 = 256 * 1024;

/// The chunks in a buffer of RIFF chunks, as (id, data). Data is cut at the buffer end.
pub(super) fn riff_chunks(b: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while out.len() < 256 {
        let (Some(id), Some(size)) = (get::<4>(b, at), at.checked_add(4).and_then(|p| le32(b, p))) else { break };
        let Some(size) = usize::try_from(size).ok() else { break };
        let Some(start) = at.checked_add(8) else { break };
        let end = start.saturating_add(size).min(b.len());
        out.push((id, b.get(start..end).unwrap_or(&[])));
        match start.checked_add(size).and_then(|e| e.checked_add(size & 1)) {
            Some(next) => at = next,
            None => break,
        }
    }
    out
}

/// A stream header (strh): kind and the stream length in units of scale/rate seconds.
#[derive(Clone, Copy)]
pub(super) struct AviStream {
    pub(super) scale: u32,
    pub(super) rate: u32,
    pub(super) length: u32,
}

impl AviStream {
    pub(super) fn duration_ms(&self, length: u32) -> Option<u64> {
        if self.scale == 0 || length == 0 {
            return None;
        }
        muldiv(length.into(), u64::from(self.scale) * 1000, self.rate.into())
    }
}

pub(super) fn avi<R: Read + Seek>(src: &mut Src<R>, info: &mut MediaInfo) -> Option<()> {
    // Find LIST 'hdrl' among the chunks of the first RIFF.
    let mut pos = 12u64;
    let mut hdrl = None;
    for _ in 0..64 {
        let h = src.read_at(pos, 12)?;
        let id = get::<4>(&h, 0)?;
        let size = u64::from(le32(&h, 4)?);
        if &id == b"LIST" && h.get(8..12) == Some(b"hdrl") {
            hdrl = Some((pos.checked_add(12)?, size.saturating_sub(4)));
            break;
        }
        pos = pos.checked_add(8 + size + (size & 1))?;
    }
    let (at, len) = hdrl?;
    let b = src.read_at(at, len.min(AVI_HDRL_CAP) as usize)?;

    let (mut usec_per_frame, mut avih_frames, mut odml_frames) = (0u32, 0u32, 0u32);
    let (mut video, mut audio): (Option<AviStream>, Option<AviStream>) = (None, None);
    for (id, d) in riff_chunks(&b) {
        match (&id, d.get(..4)) {
            (b"avih", _) => {
                usec_per_frame = le32(d, 0).unwrap_or(0);
                avih_frames = le32(d, 16).unwrap_or(0);
                info.width = le32(d, 32).and_then(positive);
                info.height = le32(d, 36).and_then(positive);
            }
            (b"LIST", Some(b"strl")) => {
                let (mut strh, mut strf) = (None, None);
                for (cid, c) in riff_chunks(d.get(4..).unwrap_or(&[])) {
                    match &cid {
                        b"strh" => strh = Some(c),
                        b"strf" => strf = Some(c),
                        _ => {}
                    }
                }
                let Some(h) = strh else { continue };
                let (Some(kind), Some(scale), Some(rate), Some(length)) =
                    (get::<4>(h, 0), le32(h, 20), le32(h, 24), le32(h, 32))
                else {
                    continue;
                };
                let stream = AviStream { scale, rate, length };
                let f = strf.unwrap_or(&[]);
                match &kind {
                    b"vids" if video.is_none() => {
                        info.has_video = true;
                        video = Some(stream);
                        // BITMAPINFOHEADER: width and height (negative for top-down pictures).
                        if info.width.is_none() || info.height.is_none() {
                            info.width = le32(f, 4).map(|w| (w as i32).unsigned_abs()).and_then(positive);
                            info.height = le32(f, 8).map(|h| (h as i32).unsigned_abs()).and_then(positive);
                        }
                    }
                    b"auds" if audio.is_none() => {
                        info.has_audio = true;
                        audio = Some(stream);
                        // WAVEFORMATEX: channels, samples per second.
                        info.channels = le16(f, 2).and_then(positive);
                        info.sample_rate = le32(f, 4).and_then(positive);
                    }
                    _ => {}
                }
            }
            (b"LIST", Some(b"odml")) => {
                // OpenDML: the frame count of the whole file (avih counts the first RIFF only).
                for (cid, c) in riff_chunks(d.get(4..).unwrap_or(&[])) {
                    if &cid == b"dmlh" {
                        odml_frames = le32(c, 0).unwrap_or(0);
                    }
                }
            }
            _ => {}
        }
    }
    let frames = avih_frames.max(odml_frames);
    info.duration_ms = video
        .and_then(|v| v.duration_ms(v.length.max(odml_frames)))
        .or_else(|| positive(frames).and_then(|n| muldiv(usec_per_frame.into(), n.into(), 1000)).and_then(positive))
        .or_else(|| audio.and_then(|a| a.duration_ms(a.length)));
    Some(())
}
