// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! FLAC.
use super::*;

/// Sample rate, channels and total samples from the body of a FLAC STREAMINFO block.
pub(super) fn streaminfo(si: &[u8]) -> Option<(u32, u16, u64)> {
    // After four sizes (10 bytes): 20 bits sample rate, 3 bits channels - 1,
    // 5 bits bits per sample - 1, 36 bits total samples.
    let x = be64(si, 10)?;
    let rate = u32::try_from(x >> 44).ok()?;
    let channels = u16::try_from((x >> 41) & 7).ok()? + 1;
    let total = x & 0xF_FFFF_FFFF;
    (rate > 0).then_some((rate, channels, total))
}

pub(super) fn flac<R: Read + Seek>(src: &mut Src<R>, at: u64, info: &mut MediaInfo) -> Option<()> {
    info.has_audio = true;
    let b = src.read_at(at, 42)?;
    // STREAMINFO is always the first metadata block and at least 34 bytes long.
    if !b.starts_with(b"fLaC") || b.get(4)? & 0x7F != 0 || be24(&b, 5)? < 34 {
        return None;
    }
    let (rate, channels, total) = streaminfo(b.get(8..)?)?;
    info.sample_rate = Some(rate);
    info.channels = Some(channels);
    // A total of 0 means "unknown".
    info.duration_ms = positive(total).and_then(|t| muldiv(t, 1000, rate.into()));
    Some(())
}
