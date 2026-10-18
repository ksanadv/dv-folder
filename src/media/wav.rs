// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! WAV (RIFF/WAVE, RF64, BW64).
use super::*;

pub(super) fn wav<R: Read + Seek>(src: &mut Src<R>, info: &mut MediaInfo) -> Option<()> {
    info.has_audio = true;
    let mut fmt: Option<Vec<u8>> = None;
    let mut data_len: Option<u64> = None;
    let mut fact: Option<u32> = None;
    let mut ds64_data: Option<u64> = None; // RF64: the real data size
    let mut pos = 12u64;
    for _ in 0..512 {
        let h = src.read_at(pos, 8)?;
        let (Some(id), Some(size32)) = (get::<4>(&h, 0), le32(&h, 4)) else { break };
        let body = pos.checked_add(8)?;
        let mut size = u64::from(size32);
        match &id {
            b"fmt " => fmt = src.read_at(body, size.min(64) as usize),
            b"fact" => fact = src.read_at(body, 4).and_then(|b| le32(&b, 0)),
            b"ds64" => ds64_data = src.read_at(body, 16).and_then(|b| le64(&b, 8)),
            b"data" => {
                // 0xFFFFFFFF means "see ds64" (RF64) or "unknown" (a stream that was never
                // finished); either way the data cannot run past the end of the file.
                if size32 == u32::MAX {
                    size = ds64_data.unwrap_or(u64::MAX);
                }
                size = size.min(src.len.saturating_sub(body));
                data_len = Some(size);
            }
            _ => {}
        }
        if fmt.is_some() && data_len.is_some() {
            break;
        }
        // Chunks are padded to an even size.
        pos = body.checked_add(size)?.checked_add(size & 1)?;
    }
    let f = fmt?;
    let tag = le16(&f, 0)?;
    let channels = le16(&f, 2)?;
    let rate = le32(&f, 4)?;
    let byte_rate = le32(&f, 8)?;
    info.channels = positive(channels);
    info.sample_rate = positive(rate);
    info.bitrate_kbps = muldiv(byte_rate.into(), 8, 1000).and_then(|k| u32::try_from(k).ok());
    // WAVE_FORMAT_EXTENSIBLE keeps the real format code in the first two bytes of SubFormat.
    let code = if tag == 0xFFFE { le16(&f, 24).unwrap_or(tag) } else { tag };
    let pcm = code == 1 || code == 3; // integer PCM or IEEE float
    info.duration_ms = match (fact, data_len) {
        // Compressed data: the fact chunk holds the number of samples.
        (Some(samples), _) if !pcm && rate > 0 => muldiv(samples.into(), 1000, rate.into()),
        (_, Some(len)) => muldiv(len, 1000, byte_rate.into()),
        _ => None,
    };
    Some(())
}
