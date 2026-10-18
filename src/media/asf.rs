// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! ASF (WMA / WMV).
use super::*;

/// A GUID as stored on disk: the first three fields little-endian.
pub(super) const fn guid(a: u32, b: u16, c: u16, d: [u8; 8]) -> [u8; 16] {
    let a = a.to_le_bytes();
    let b = b.to_le_bytes();
    let c = c.to_le_bytes();
    [a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]]
}

pub(super) const ASF_HEADER: [u8; 16] =
    guid(0x75B2_2630, 0x668E, 0x11CF, [0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C]);
pub(super) const ASF_FILE_PROPERTIES: [u8; 16] =
    guid(0x8CAB_DCA1, 0xA947, 0x11CF, [0x8E, 0xE4, 0x00, 0xC0, 0x0C, 0x20, 0x53, 0x65]);
pub(super) const ASF_STREAM_PROPERTIES: [u8; 16] =
    guid(0xB7DC_0791, 0xA9B7, 0x11CF, [0x8E, 0xE6, 0x00, 0xC0, 0x0C, 0x20, 0x53, 0x65]);
pub(super) const ASF_AUDIO_MEDIA: [u8; 16] =
    guid(0xF869_9E40, 0x5B4D, 0x11CF, [0xA8, 0xFD, 0x00, 0x80, 0x5F, 0x5C, 0x44, 0x2B]);
pub(super) const ASF_VIDEO_MEDIA: [u8; 16] =
    guid(0xBC19_EFC0, 0x5B4D, 0x11CF, [0xA8, 0xFD, 0x00, 0x80, 0x5F, 0x5C, 0x44, 0x2B]);

/// Most of the ASF header object that is read.
pub(super) const ASF_HEADER_CAP: u64 = 256 * 1024;

pub(super) fn asf<R: Read + Seek>(src: &mut Src<R>, info: &mut MediaInfo) -> Option<()> {
    let h = src.read_at(0, 30)?;
    let header_size = le64(&h, 16)?;
    let count = le32(&h, 24)?;
    let b = src.read_at(0, header_size.min(ASF_HEADER_CAP) as usize)?;
    let mut at = 30usize;
    for _ in 0..count.min(256) {
        let id = get::<16>(&b, at)?;
        let size = usize::try_from(le64(&b, at + 16)?).ok()?;
        if size < 24 {
            break;
        }
        let obj = b.get(at..at.saturating_add(size).min(b.len()))?;
        if id == ASF_FILE_PROPERTIES {
            // Play duration (100 ns units) includes the preroll (ms). Broadcast files (flag
            // bit 0) have no valid duration.
            if let (Some(play), Some(preroll), Some(flags)) = (le64(obj, 64), le64(obj, 80), le32(obj, 88)) {
                if flags & 1 == 0 {
                    info.duration_ms = (play / 10_000).checked_sub(preroll);
                }
            }
        } else if id == ASF_STREAM_PROPERTIES {
            let kind = get::<16>(obj, 24);
            let data = obj.get(78..).unwrap_or(&[]);
            if kind == Some(ASF_AUDIO_MEDIA) && !info.has_audio {
                // WAVEFORMATEX
                info.has_audio = true;
                info.channels = le16(data, 2).and_then(positive);
                info.sample_rate = le32(data, 4).and_then(positive);
            } else if kind == Some(ASF_VIDEO_MEDIA) && !info.has_video {
                info.has_video = true;
                info.width = le32(data, 0).and_then(positive);
                info.height = le32(data, 4).and_then(positive);
            }
        }
        at = at.checked_add(size)?;
    }
    Some(())
}
