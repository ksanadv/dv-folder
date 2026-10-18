// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Ogg (Vorbis, Opus, FLAC, Speex; Theora for the picture size).
use super::*;

/// How much of the start and of the end of an Ogg file is read.
pub(super) const OGG_WINDOW: usize = 64 * 1024;

pub(super) struct OggPage {
    pub(super) flags: u8,
    pub(super) granule: u64,
    pub(super) serial: u32,
    pub(super) header_len: usize,
    pub(super) body_len: usize,
}

/// Parses the page header at `at`; the lacing table must be inside `b`.
pub(super) fn ogg_page(b: &[u8], at: usize) -> Option<OggPage> {
    if b.get(at..at.checked_add(4)?)? != b"OggS" || *b.get(at.checked_add(4)?)? != 0 {
        return None;
    }
    let segments = usize::from(*b.get(at.checked_add(26)?)?);
    let lacing_at = at.checked_add(27)?;
    let lacing = b.get(lacing_at..lacing_at.checked_add(segments)?)?;
    Some(OggPage {
        flags: *b.get(at + 5)?,
        granule: le64(b, at + 6)?,
        serial: le32(b, at + 14)?,
        header_len: 27 + segments,
        body_len: lacing.iter().map(|&l| usize::from(l)).sum(),
    })
}

/// The audio stream of an Ogg file: how to turn its last granule position into time.
pub(super) struct OggAudio {
    pub(super) serial: u32,
    pub(super) granule_rate: u32,
    pub(super) pre_skip: u64,
}

/// Reads a codec identification header (the first packet of a stream).
pub(super) fn ogg_audio_header(p: &[u8], serial: u32, info: &mut MediaInfo) -> Option<OggAudio> {
    let (rate, channels, granule_rate, pre_skip) = if p.starts_with(b"\x01vorbis") {
        let rate = le32(p, 12)?;
        // Nominal bitrate, when the encoder states one.
        if let Some(n) = le32(p, 20).and_then(|n| i32::try_from(n).ok()).filter(|&n| n > 0) {
            info.bitrate_kbps = u32::try_from(n / 1000).ok().and_then(positive);
        }
        (rate, u16::from(*p.get(11)?), rate, 0)
    } else if p.starts_with(b"OpusHead") {
        // Opus always decodes at 48 kHz; the first `pre_skip` samples are not played.
        (48_000, u16::from(*p.get(9)?), 48_000, u64::from(le16(p, 10)?))
    } else if p.starts_with(b"\x7fFLAC") {
        let (rate, channels, _) = streaminfo(p.get(17..)?)?;
        (rate, channels, rate, 0)
    } else if p.starts_with(b"Speex   ") {
        let rate = le32(p, 36)?;
        (rate, u16::try_from(le32(p, 48)?).ok()?, rate, 0)
    } else {
        return None;
    };
    if rate == 0 {
        return None;
    }
    info.has_audio = true;
    info.sample_rate = Some(rate);
    info.channels = positive(channels);
    Some(OggAudio { serial, granule_rate, pre_skip })
}

pub(super) fn ogg<R: Read + Seek>(src: &mut Src<R>, info: &mut MediaInfo) -> Option<()> {
    // The first pages are the "beginning of stream" pages, one per logical stream, each holding
    // that stream's identification header.
    let head = src.read_at(0, OGG_WINDOW)?;
    let mut audio: Option<OggAudio> = None;
    let mut at = 0usize;
    for _ in 0..32 {
        let Some(page) = ogg_page(&head, at) else { break };
        if page.flags & 0x02 == 0 {
            break;
        }
        let body_at = at.checked_add(page.header_len)?;
        let body = head.get(body_at..).unwrap_or(&[]);
        let body = body.get(..page.body_len).unwrap_or(body);
        if audio.is_none() {
            audio = ogg_audio_header(body, page.serial, info);
        }
        if body.starts_with(b"\x80theora") {
            info.has_video = true;
            info.width = be24(body, 14).and_then(positive);
            info.height = be24(body, 17).and_then(positive);
        }
        at = body_at.checked_add(page.body_len)?;
    }
    let audio = audio?;
    // The length is the granule position (a sample count) of the stream's last page.
    let tail_at = src.len.saturating_sub(OGG_WINDOW as u64);
    let tail = src.read_at(tail_at, OGG_WINDOW)?;
    let mut last = None;
    for i in 0..tail.len() {
        if tail.get(i) != Some(&b'O') {
            continue;
        }
        if let Some(p) = ogg_page(&tail, i) {
            // A granule of -1 means "no packet ends on this page"; other negatives are invalid.
            if p.serial == audio.serial && p.granule <= i64::MAX as u64 {
                last = Some(p.granule);
            }
        }
    }
    let samples = last?.saturating_sub(audio.pre_skip);
    info.duration_ms = muldiv(samples, 1000, audio.granule_rate.into());
    Some(())
}
