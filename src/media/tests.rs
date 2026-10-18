// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Tests of the media readers, with good and damaged files built in memory.
use super::*;
use std::io::{self, Cursor};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

// ----- Test readers -----

/// Wraps a reader, counts the bytes read, and hands them out at most `piece` at a time.
struct Counting<R> {
    inner: R,
    bytes: u64,
    piece: usize,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = buf.len().min(self.piece);
        let got = self.inner.read(&mut buf[..n])?;
        self.bytes += got as u64;
        Ok(got)
    }
}

impl<R: Seek> Seek for Counting<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

/// A file of `len` bytes that is all zeros except for `pieces`; the zeros take no memory.
struct Sparse {
    len: u64,
    pos: u64,
    pieces: Vec<(u64, Vec<u8>)>,
}

impl Read for Sparse {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.len {
            return Ok(0);
        }
        let n = (buf.len() as u64).min(self.len - self.pos) as usize;
        let out = &mut buf[..n];
        out.fill(0);
        let (from, to) = (self.pos, self.pos + n as u64);
        for (at, data) in &self.pieces {
            let s = from.max(*at);
            let e = to.min(at + data.len() as u64);
            if s < e {
                out[(s - from) as usize..(e - from) as usize]
                    .copy_from_slice(&data[(s - at) as usize..(e - at) as usize]);
            }
        }
        self.pos = to;
        Ok(n)
    }
}

impl Seek for Sparse {
    fn seek(&mut self, p: SeekFrom) -> io::Result<u64> {
        let new = match p {
            SeekFrom::Start(x) => x as i128,
            SeekFrom::End(d) => self.len as i128 + d as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
        };
        if new < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before start"));
        }
        self.pos = new as u64;
        Ok(self.pos)
    }
}

/// Parses `bytes` (handed out in small pieces) and checks that only a bounded part was read.
fn parse(bytes: &[u8], ext: &str) -> Option<MediaInfo> {
    let mut r = Counting { inner: Cursor::new(bytes), bytes: 0, piece: 4093 };
    let info = read(&mut r, ext);
    assert!(r.bytes <= READ_BUDGET, "read {} bytes", r.bytes);
    info
}

fn le16b(v: u16) -> [u8; 2] {
    v.to_le_bytes()
}
fn le32b(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}
fn le64b(v: u64) -> [u8; 8] {
    v.to_le_bytes()
}
fn be16b(v: u16) -> [u8; 2] {
    v.to_be_bytes()
}
fn be32b(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}
fn be64b(v: u64) -> [u8; 8] {
    v.to_be_bytes()
}

// ----- MP4 builders -----

fn bx(kind: &[u8; 4], parts: &[&[u8]]) -> Vec<u8> {
    let body = parts.concat();
    let mut v = be32b(body.len() as u32 + 8).to_vec();
    v.extend_from_slice(kind);
    v.extend(body);
    v
}

/// A "full box" body: version, 24-bit flags, then the rest.
fn full(version: u8, flags: u32, rest: &[u8]) -> Vec<u8> {
    let mut v = vec![version];
    v.extend_from_slice(&be32b(flags)[1..]);
    v.extend_from_slice(rest);
    v
}

fn unity_matrix() -> Vec<u8> {
    [0x10000u32, 0, 0, 0, 0x10000, 0, 0, 0, 0x4000_0000].iter().flat_map(|v| be32b(*v)).collect()
}

fn mvhd(version: u8, timescale: u32, duration: u64) -> Vec<u8> {
    let mut r = Vec::new();
    if version == 0 {
        r.extend([0u8; 8]);
        r.extend(be32b(timescale));
        r.extend(be32b(duration as u32));
    } else {
        r.extend([0u8; 16]);
        r.extend(be32b(timescale));
        r.extend(be64b(duration));
    }
    r.extend(be32b(0x0001_0000)); // rate 1.0
    r.extend(be16b(0x0100)); // volume 1.0
    r.extend([0u8; 10]);
    r.extend(unity_matrix());
    r.extend([0u8; 24]);
    r.extend(be32b(3)); // next track id
    bx(b"mvhd", &[&full(version, 0, &r)])
}

fn tkhd(version: u8, id: u32, duration: u64, width: u32, height: u32, audio: bool) -> Vec<u8> {
    let mut r = Vec::new();
    if version == 0 {
        r.extend([0u8; 8]);
        r.extend(be32b(id));
        r.extend([0u8; 4]);
        r.extend(be32b(duration as u32));
    } else {
        r.extend([0u8; 16]);
        r.extend(be32b(id));
        r.extend([0u8; 4]);
        r.extend(be64b(duration));
    }
    r.extend([0u8; 8]);
    r.extend(be16b(0)); // layer
    r.extend(be16b(if audio { 1 } else { 0 })); // alternate group
    r.extend(be16b(if audio { 0x0100 } else { 0 })); // volume
    r.extend([0u8; 2]);
    r.extend(unity_matrix());
    r.extend(be32b(width << 16));
    r.extend(be32b(height << 16));
    bx(b"tkhd", &[&full(version, 7, &r)])
}

fn mdhd(timescale: u32, duration: u32) -> Vec<u8> {
    let mut r = vec![0u8; 8];
    r.extend(be32b(timescale));
    r.extend(be32b(duration));
    r.extend(be16b(0x55C4)); // "und"
    r.extend(be16b(0));
    bx(b"mdhd", &[&full(0, 0, &r)])
}

fn hdlr(kind: &[u8; 4], name: &str) -> Vec<u8> {
    let mut r = vec![0u8; 4];
    r.extend_from_slice(kind);
    r.extend([0u8; 12]);
    r.extend(name.as_bytes());
    r.push(0);
    bx(b"hdlr", &[&full(0, 0, &r)])
}

fn video_entry(w: u16, h: u16) -> Vec<u8> {
    let mut r = vec![0u8; 6];
    r.extend(be16b(1)); // data reference index
    r.extend([0u8; 16]);
    r.extend(be16b(w));
    r.extend(be16b(h));
    r.extend(be32b(0x0048_0000));
    r.extend(be32b(0x0048_0000));
    r.extend([0u8; 4]);
    r.extend(be16b(1)); // frame count
    r.extend([0u8; 32]); // compressor name
    r.extend(be16b(0x18));
    r.extend(be16b(0xFFFF));
    bx(b"mp4v", &[&r])
}

fn audio_entry(channels: u16, rate: u32) -> Vec<u8> {
    let mut r = vec![0u8; 6];
    r.extend(be16b(1));
    r.extend([0u8; 8]);
    r.extend(be16b(channels));
    r.extend(be16b(16));
    r.extend([0u8; 4]);
    r.extend(be32b(rate << 16));
    bx(b"mp4a", &[&r])
}

fn trak(tkhd: Vec<u8>, timescale: u32, duration: u32, handler: &[u8; 4], entry: Vec<u8>) -> Vec<u8> {
    let media_header = if handler == b"vide" {
        bx(b"vmhd", &[&full(0, 1, &[0u8; 8])])
    } else {
        bx(b"smhd", &[&full(0, 0, &[0u8; 4])])
    };
    let dinf = bx(b"dinf", &[&bx(b"dref", &[&full(0, 0, &be32b(1)), &bx(b"url ", &[&full(0, 1, &[])])])]);
    let stbl = bx(
        b"stbl",
        &[
            &bx(b"stsd", &[&full(0, 0, &be32b(1)), &entry]),
            &bx(b"stts", &[&full(0, 0, &be32b(0))]),
            &bx(b"stsc", &[&full(0, 0, &be32b(0))]),
            &bx(b"stsz", &[&full(0, 0, &[0u8; 8])]),
            &bx(b"stco", &[&full(0, 0, &be32b(0))]),
        ],
    );
    let minf = bx(b"minf", &[&media_header, &dinf, &stbl]);
    let mdia = bx(b"mdia", &[&mdhd(timescale, duration), &hdlr(handler, "dec_media"), &minf]);
    bx(b"trak", &[&tkhd, &mdia])
}

fn ftyp(brand: &[u8; 4]) -> Vec<u8> {
    bx(b"ftyp", &[brand, &be32b(0), brand, b"isom"])
}

/// moov with mvhd v0 (600 units/s, 83.5 s), a 640x360 video track and a stereo 44.1 kHz audio track.
fn moov_v0() -> Vec<u8> {
    bx(
        b"moov",
        &[
            &mvhd(0, 600, 50_100),
            &trak(tkhd(0, 1, 50_100, 640, 360, false), 600, 50_100, b"vide", video_entry(640, 360)),
            &trak(tkhd(0, 2, 50_100, 0, 0, true), 44_100, 3_682_350, b"soun", audio_entry(2, 44_100)),
        ],
    )
}

fn mp4_v0() -> Vec<u8> {
    [ftyp(b"isom"), moov_v0(), bx(b"mdat", &[&[0u8; 2000]])].concat()
}

/// mvhd v1 with a 64-bit duration, moov after a 64-bit-sized mdat.
fn mp4_v1_moov_last() -> Vec<u8> {
    let payload = vec![0u8; 70_000];
    let mut mdat = be32b(1).to_vec();
    mdat.extend(b"mdat");
    mdat.extend(be64b(16 + payload.len() as u64));
    mdat.extend(payload);
    let moov = bx(
        b"moov",
        &[
            &mvhd(1, 1_000_000, 5_000_000_000),
            &trak(tkhd(1, 1, 5_000_000_000, 1920, 1080, false), 90_000, 0, b"vide", video_entry(1920, 1080)),
        ],
    );
    [ftyp(b"mp42"), mdat, moov].concat()
}

/// An audio-only .m4a: 187.5 s at 44.1 kHz.
fn m4a() -> Vec<u8> {
    let d = 44_100 * 187 + 22_050;
    let moov = bx(
        b"moov",
        &[
            &mvhd(0, 44_100, d as u64),
            &trak(tkhd(0, 1, d as u64, 0, 0, true), 44_100, d, b"soun", audio_entry(2, 44_100)),
        ],
    );
    [ftyp(b"M4A "), moov, bx(b"mdat", &[&[0u8; 3000]])].concat()
}

// ----- MP3 builders -----

fn frames(header: [u8; 4], len: usize, count: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..count {
        let mut f = vec![0u8; len];
        f[..4].copy_from_slice(&header);
        out.extend(f);
    }
    out
}

/// 300 MPEG-1 layer III frames at 44.1 kHz with changing bitrates, after a Xing or VBRI frame.
fn mp3_vbr(tag: &[u8; 4]) -> Vec<u8> {
    // (third header byte, frame length): 128, 192, 256, 320 kbps without padding.
    let rates = [(0x90u8, 417usize), (0xB0, 626), (0xC0, 835), (0xE0, 1044)];
    let mut audio = Vec::new();
    for i in 0..300 {
        let (b2, len) = rates[i % 4];
        audio.extend(frames([0xFF, 0xFB, b2, 0x00], len, 1));
    }
    let mut first = frames([0xFF, 0xFB, 0x90, 0x00], 417, 1);
    let total = (first.len() + audio.len()) as u32;
    first[36..40].copy_from_slice(tag);
    if tag == b"Xing" {
        first[40..44].copy_from_slice(&be32b(3)); // frames and bytes present
        first[44..48].copy_from_slice(&be32b(300));
        first[48..52].copy_from_slice(&be32b(total));
    } else {
        first[40..42].copy_from_slice(&be16b(1)); // version
        first[42..44].copy_from_slice(&be16b(0x2C0)); // delay
        first[44..46].copy_from_slice(&be16b(75)); // quality
        first[46..50].copy_from_slice(&be32b(total));
        first[50..54].copy_from_slice(&be32b(300));
    }
    [first, audio].concat()
}

/// 250 CBR frames (128 kbps, 48 kHz) between an ID3v2.4 tag and an ID3v1 tag: exactly 6 s.
fn mp3_cbr_id3() -> Vec<u8> {
    let mut tag = b"ID3\x04\x00\x00".to_vec();
    tag.extend([0, 0, 7, 94]); // 990, synchsafe
    tag.extend(b"TIT2");
    tag.extend([0, 0, 0, 6, 0, 0, 3]);
    tag.extend(b"Hello");
    tag.resize(1000, 0);
    let mut v = tag;
    v.extend(frames([0xFF, 0xFB, 0x94, 0x00], 384, 250));
    v.extend(b"TAG");
    v.extend([0u8; 125]);
    v
}

// ----- FLAC builder -----

/// 44.1 kHz stereo 16-bit; 1 234 567 samples is 27.994 s.
fn flac_file() -> Vec<u8> {
    flac_with(1_234_567)
}

fn flac_with(total_samples: u64) -> Vec<u8> {
    let mut v = b"fLaC".to_vec();
    v.extend([0x00, 0, 0, 34]); // STREAMINFO, more blocks follow
    v.extend(be16b(4096));
    v.extend(be16b(4096));
    v.extend([0u8; 6]);
    let x: u64 = (44_100u64 << 44) | (1 << 41) | (15 << 36) | total_samples;
    v.extend(be64b(x));
    v.extend([0u8; 16]);
    v.extend([0x81, 0, 0, 100]); // PADDING, last block
    v.extend([0u8; 100]);
    v.extend(vec![0u8; 5000]);
    v
}

// ----- Ogg builders -----

fn ogg_crc(data: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in data {
        crc ^= u32::from(b) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
        }
    }
    crc
}

fn page(flags: u8, granule: u64, serial: u32, seq: u32, packets: &[&[u8]]) -> Vec<u8> {
    let mut lacing = Vec::new();
    for p in packets {
        let mut n = p.len();
        while n >= 255 {
            lacing.push(255u8);
            n -= 255;
        }
        lacing.push(n as u8);
    }
    assert!(lacing.len() <= 255);
    let mut v = b"OggS".to_vec();
    v.push(0);
    v.push(flags);
    v.extend(le64b(granule));
    v.extend(le32b(serial));
    v.extend(le32b(seq));
    v.extend([0u8; 4]);
    v.push(lacing.len() as u8);
    v.extend(lacing);
    for p in packets {
        v.extend_from_slice(p);
    }
    let crc = ogg_crc(&v);
    v[22..26].copy_from_slice(&le32b(crc));
    v
}

fn vorbis_id(channels: u8, rate: u32) -> Vec<u8> {
    let mut v = b"\x01vorbis".to_vec();
    v.extend(le32b(0));
    v.push(channels);
    v.extend(le32b(rate));
    v.extend(le32b(0));
    v.extend(le32b(128_000));
    v.extend(le32b(0));
    v.push(0xB8);
    v.push(1);
    v
}

fn vorbis_comment() -> Vec<u8> {
    let mut v = b"\x03vorbis".to_vec();
    v.extend(le32b(9));
    v.extend(b"dec_media");
    v.extend(le32b(0));
    v.push(1);
    v
}

/// Vorbis, 44.1 kHz stereo, 12.078 s, about 240 KB so the last page is found in the tail.
fn ogg_vorbis() -> Vec<u8> {
    let serial = 0x1234_5678;
    let mut v = page(2, 0, serial, 0, &[&vorbis_id(2, 44_100)]);
    let setup = [b"\x05vorbis".as_slice(), &[0u8; 40]].concat();
    v.extend(page(0, 0, serial, 1, &[&vorbis_comment(), &setup]));
    let big = vec![0x55u8; 60_000];
    for i in 0..4u32 {
        v.extend(page(0, 44_100 * 3 * u64::from(i + 1), serial, 2 + i, &[&big]));
    }
    v.extend(page(4, 44_100 * 12 + 3456, serial, 6, &[&[0x55u8; 1000]]));
    v
}

fn ogg_opus() -> Vec<u8> {
    let serial = 0x0BAD_CAFE;
    let mut head = b"OpusHead".to_vec();
    head.push(1);
    head.push(2);
    head.extend(le16b(312));
    head.extend(le32b(44_100));
    head.extend(le16b(0));
    head.push(0);
    let mut tags = b"OpusTags".to_vec();
    tags.extend(le32b(9));
    tags.extend(b"dec_media");
    tags.extend(le32b(0));
    let mut v = page(2, 0, serial, 0, &[&head]);
    v.extend(page(0, 0, serial, 1, &[&tags]));
    let pkt = vec![0xFCu8; 30_000];
    for i in 0..3u32 {
        v.extend(page(0, 48_000 + 312 + u64::from(i) * 48_000, serial, 2 + i, &[&pkt]));
    }
    v.extend(page(4, 48_000 * 5 + 312, serial, 5, &[&[0xFCu8; 500]]));
    v
}

/// Theora (640x480) and Vorbis in one file; the Theora stream's last page has a large
/// granule that must not be taken for the audio length.
fn ogg_theora_vorbis() -> Vec<u8> {
    let (tv, av) = (0x1111_1111u32, 0x2222_2222u32);
    let mut th = b"\x80theora".to_vec();
    th.extend([3, 2, 1]);
    th.extend(be16b(40));
    th.extend(be16b(30));
    th.extend(&be32b(640)[1..]);
    th.extend(&be32b(480)[1..]);
    th.extend([0, 0]);
    th.extend(be32b(30));
    th.extend(be32b(1));
    th.extend([0, 0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
    let mut v = page(2, 0, tv, 0, &[&th]);
    v.extend(page(2, 0, av, 0, &[&vorbis_id(1, 22_050)]));
    v.extend(page(0, 0, av, 1, &[&vorbis_comment()]));
    v.extend(page(0, 22_050 * 4, av, 2, &[&[1u8; 3000]]));
    v.extend(page(4, 22_050 * 9, av, 3, &[&[1u8; 3000]]));
    v.extend(page(4, 0x7FFF_0000, tv, 1, &[&[2u8; 3000]]));
    v
}

// ----- Matroska builders -----

fn el(id: u32, body: &[u8]) -> Vec<u8> {
    let idb = be32b(id);
    let skip = idb.iter().take_while(|&&b| b == 0).count();
    let mut v = idb[skip..].to_vec();
    v.push(0x01); // 8-byte size
    v.extend(&be64b(body.len() as u64)[1..]);
    v.extend_from_slice(body);
    v
}

fn el_uint(id: u32, n: u64) -> Vec<u8> {
    let b = be64b(n);
    let skip = b.iter().take_while(|&&x| x == 0).count().min(7);
    el(id, &b[skip..])
}

fn ebml_header(doc_type: &str) -> Vec<u8> {
    el(
        MKV_EBML,
        &[
            el_uint(0x4286, 1),
            el_uint(0x42F7, 1),
            el_uint(0x42F2, 4),
            el_uint(0x42F3, 8),
            el(0x4282, doc_type.as_bytes()),
            el_uint(0x4287, 4),
            el_uint(0x4285, 2),
        ]
        .concat(),
    )
}

fn mkv_tracks_el() -> Vec<u8> {
    let video = el(
        MKV_TRACK_ENTRY,
        &[
            el_uint(0xD7, 1),
            el_uint(0x73C5, 1),
            el_uint(MKV_TRACK_TYPE, 1),
            el(MKV_CODEC_ID, b"V_VP9"),
            el(MKV_VIDEO, &[el_uint(MKV_PIXEL_WIDTH, 1280), el_uint(MKV_PIXEL_HEIGHT, 720)].concat()),
        ]
        .concat(),
    );
    let audio = el(
        MKV_TRACK_ENTRY,
        &[
            el_uint(0xD7, 2),
            el_uint(0x73C5, 2),
            el_uint(MKV_TRACK_TYPE, 2),
            el(MKV_CODEC_ID, b"A_OPUS"),
            el(MKV_AUDIO, &[el(MKV_SAMPLING_FREQUENCY, &48_000f64.to_be_bytes()), el_uint(MKV_CHANNELS, 2)].concat()),
        ]
        .concat(),
    );
    el(MKV_TRACKS, &[video, audio].concat())
}

fn mkv_cluster() -> Vec<u8> {
    el(MKV_CLUSTER, &[el_uint(0xE7, 0), el(0xA3, &[0x81, 0, 0, 0x80, 1, 2, 3, 4])].concat())
}

/// Known-size Segment: Info (1 ms scale, 65.432 s as f64), Tracks, Cluster.
fn mkv_simple() -> Vec<u8> {
    let info = el(
        MKV_INFO,
        &[
            el_uint(MKV_TIMECODE_SCALE, 1_000_000),
            el(MKV_DURATION, &65_432f64.to_be_bytes()),
            el(0x4D80, b"dec_media"),
            el(0x5741, b"dec_media"),
        ]
        .concat(),
    );
    [ebml_header("webm"), el(MKV_SEGMENT, &[info, mkv_tracks_el(), mkv_cluster()].concat())].concat()
}

/// Unknown-size Segment; Info and Tracks come after a Cluster and are found by the SeekHead;
/// Duration (f32) comes before TimecodeScale (0.1 ms).
fn mkv_seekhead() -> Vec<u8> {
    mkv_seekhead_with(false)
}

fn mkv_seekhead_with(known_size: bool) -> Vec<u8> {
    let info =
        el(MKV_INFO, &[el(MKV_DURATION, &123_456f32.to_be_bytes()), el_uint(MKV_TIMECODE_SCALE, 100_000)].concat());
    let tracks = mkv_tracks_el();
    let cluster = mkv_cluster();
    let seek =
        |id: u32, pos: u32| el(MKV_SEEK, &[el(MKV_SEEK_ID, &be32b(id)), el(MKV_SEEK_POSITION, &be32b(pos))].concat());
    let seekhead_len = el(MKV_SEEKHEAD, &[seek(0, 0), seek(0, 0)].concat()).len();
    let info_at = (seekhead_len + cluster.len()) as u32;
    let tracks_at = info_at + info.len() as u32;
    let seekhead = el(MKV_SEEKHEAD, &[seek(MKV_INFO, info_at), seek(MKV_TRACKS, tracks_at)].concat());
    let body = [seekhead, cluster, info, tracks].concat();
    if known_size {
        return [ebml_header("matroska"), el(MKV_SEGMENT, &body)].concat();
    }
    let mut v = ebml_header("matroska");
    v.extend([0x18, 0x53, 0x80, 0x67, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
    v.extend(body);
    v
}

// ----- AVI builders -----

fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend(le32b(body.len() as u32));
    v.extend_from_slice(body);
    if body.len() % 2 == 1 {
        v.push(0);
    }
    v
}

fn list(kind: &[u8; 4], parts: &[Vec<u8>]) -> Vec<u8> {
    chunk(b"LIST", &[kind.to_vec(), parts.concat()].concat())
}

fn avih(usec: u32, frames: u32, streams: u32, w: u32, h: u32) -> Vec<u8> {
    let f = [usec, 1_000_000, 0, 0x10, frames, 0, streams, 0x10000, w, h, 0, 0, 0, 0];
    chunk(b"avih", &f.iter().flat_map(|v| le32b(*v)).collect::<Vec<_>>())
}

fn strh(kind: &[u8; 4], handler: &[u8; 4], scale: u32, rate: u32, length: u32, sample_size: u32) -> Vec<u8> {
    let mut v = kind.to_vec();
    v.extend(handler);
    v.extend(le32b(0)); // flags
    v.extend([0u8; 4]); // priority, language
    v.extend(le32b(0)); // initial frames
    v.extend(le32b(scale));
    v.extend(le32b(rate));
    v.extend(le32b(0)); // start
    v.extend(le32b(length));
    v.extend(le32b(0x10000));
    v.extend(le32b(u32::MAX)); // quality
    v.extend(le32b(sample_size));
    v.extend([0u8; 8]);
    chunk(b"strh", &v)
}

fn bitmap_info(w: u32, h: u32) -> Vec<u8> {
    let mut v = le32b(40).to_vec();
    v.extend(le32b(w));
    v.extend(le32b(h));
    v.extend(le16b(1));
    v.extend(le16b(24));
    v.extend(b"MJPG");
    v.extend(le32b(w * h * 3));
    v.extend([0u8; 16]);
    v
}

fn wave_format(tag: u16, channels: u16, rate: u32) -> Vec<u8> {
    let mut v = le16b(tag).to_vec();
    v.extend(le16b(channels));
    v.extend(le32b(rate));
    v.extend(le32b(rate * u32::from(channels) * 2));
    v.extend(le16b(channels * 2));
    v.extend(le16b(16));
    v.extend(le16b(0));
    v
}

fn riff(form: &[u8; 4], parts: &[Vec<u8>]) -> Vec<u8> {
    let body = [form.to_vec(), parts.concat()].concat();
    let mut v = b"RIFF".to_vec();
    v.extend(le32b(body.len() as u32));
    v.extend(body);
    v
}

/// 250 frames at 25 fps (10 s), 320x240, with a PCM audio stream.
fn avi_simple() -> Vec<u8> {
    let hdrl = list(
        b"hdrl",
        &[
            avih(40_000, 250, 2, 320, 240),
            list(b"strl", &[strh(b"vids", b"MJPG", 1, 25, 250, 0), chunk(b"strf", &bitmap_info(320, 240))]),
            list(b"strl", &[strh(b"auds", &[0; 4], 1, 44_100, 441_000, 4), chunk(b"strf", &wave_format(1, 2, 44_100))]),
        ],
    );
    let movi = list(b"movi", &[chunk(b"00dc", &[0u8; 100]), chunk(b"01wb", &[0u8; 64])]);
    let idx = chunk(b"idx1", &[b"00dc".as_slice(), &le32b(0x10), &le32b(4), &le32b(100)].concat());
    riff(b"AVI ", &[hdrl, movi, idx])
}

/// OpenDML: avih counts 100 frames (first RIFF only); strh and dmlh count all 1000 frames
/// at 29.97 fps: 33.366 s. `strh_frames` lets a test model writers that put the
/// first-RIFF count in strh as well.
fn avi_odml_with(strh_frames: u32) -> Vec<u8> {
    let mut dmlh = le32b(1000).to_vec();
    dmlh.extend([0u8; 244]);
    let hdrl = list(
        b"hdrl",
        &[
            avih(33_367, 100, 1, 640, 480),
            list(
                b"strl",
                &[strh(b"vids", b"MJPG", 1001, 30_000, strh_frames, 0), chunk(b"strf", &bitmap_info(640, 480))],
            ),
            list(b"odml", &[chunk(b"dmlh", &dmlh)]),
        ],
    );
    riff(b"AVI ", &[hdrl, chunk(b"JUNK", &[0u8; 7]), list(b"movi", &[chunk(b"00dc", &[0u8; 10])])])
}

fn avi_odml() -> Vec<u8> {
    avi_odml_with(1000)
}

#[test]
fn avi_header_list_after_other_chunks() {
    // An odd-sized JUNK chunk (padded) before LIST hdrl.
    let v = avi_simple();
    let moved = [&v[..12], &chunk(b"JUNK", &[0u8; 7])[..], &v[12..]].concat();
    check("avi junk first", parse(&moved, "avi"), &info(10_000, Some((320, 240)), Some((44_100, 2))));
    // OpenDML file whose strh counts only the first RIFF: dmlh gives the whole length.
    check("avi odml short strh", parse(&avi_odml_with(100), "avi"), &info(33_366, Some((640, 480)), None));
}

// ----- ASF builder -----

fn asf_obj(id: [u8; 16], body: &[u8]) -> Vec<u8> {
    let mut v = id.to_vec();
    v.extend(le64b(24 + body.len() as u64));
    v.extend_from_slice(body);
    v
}

/// WMV-like file: 12.345 s plus 3 s preroll; stereo 44.1 kHz audio and 320x240 video.
fn asf_file() -> Vec<u8> {
    let no_error_correction = guid(0x20FB_5700, 0x5B55, 0x11CF, [0xA8, 0xFD, 0x00, 0x80, 0x5F, 0x5C, 0x44, 0x2B]);
    let stream = |kind: [u8; 16], number: u16, specific: &[u8]| {
        let mut v = kind.to_vec();
        v.extend(no_error_correction);
        v.extend(le64b(0));
        v.extend(le32b(specific.len() as u32));
        v.extend(le32b(0));
        v.extend(le16b(number));
        v.extend(le32b(0));
        v.extend_from_slice(specific);
        asf_obj(ASF_STREAM_PROPERTIES, &v)
    };
    let audio = stream(ASF_AUDIO_MEDIA, 1, &wave_format(0x0161, 2, 44_100));
    let mut vs = le32b(320).to_vec();
    vs.extend(le32b(240));
    vs.push(2);
    vs.extend(le16b(40));
    vs.extend(bitmap_info(320, 240));
    let video = stream(ASF_VIDEO_MEDIA, 2, &vs);
    let ext = {
        let mut v = guid(0xABD3_D211, 0xA9BA, 0x11CF, [0x8E, 0xE6, 0x00, 0xC0, 0x0C, 0x20, 0x53, 0x65]).to_vec();
        v.extend(le16b(6));
        v.extend(le32b(0));
        asf_obj(guid(0x5FBF_03B5, 0xA92E, 0x11CF, [0x8E, 0xE3, 0x00, 0xC0, 0x0C, 0x20, 0x53, 0x65]), &v)
    };
    let data = {
        let mut v = vec![0u8; 16];
        v.extend(le64b(0));
        v.extend([1, 1]);
        asf_obj(guid(0x75B2_2636, 0x668E, 0x11CF, [0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C]), &v)
    };
    let file_props = |total: u64| {
        let mut v = vec![0x42u8; 16]; // file id
        v.extend(le64b(total));
        v.extend(le64b(0)); // creation date
        v.extend(le64b(0)); // packets
        v.extend(le64b((12_345 + 3_000) * 10_000)); // play duration, 100 ns
        v.extend(le64b(12_345 * 10_000)); // send duration
        v.extend(le64b(3_000)); // preroll, ms
        v.extend(le32b(2)); // seekable
        v.extend(le32b(3200));
        v.extend(le32b(3200));
        v.extend(le32b(128_000));
        asf_obj(ASF_FILE_PROPERTIES, &v)
    };
    let build = |total: u64| {
        let objects = [file_props(total), audio.clone(), video.clone(), ext.clone()].concat();
        let mut v = ASF_HEADER.to_vec();
        v.extend(le64b(30 + objects.len() as u64));
        v.extend(le32b(4));
        v.extend([1, 2]);
        v.extend(objects);
        v.extend(data.clone());
        v
    };
    let size = build(0).len() as u64;
    build(size)
}

// ----- WAV builder (RF64) -----

fn wav_rf64_head(data_len: u64) -> Vec<u8> {
    let mut v = b"RF64".to_vec();
    v.extend(le32b(u32::MAX));
    v.extend(b"WAVE");
    let mut ds64 = le64b(data_len + 64).to_vec();
    ds64.extend(le64b(data_len));
    ds64.extend(le64b(data_len / 4));
    ds64.extend(le32b(0));
    v.extend(chunk(b"ds64", &ds64));
    // WAVE_FORMAT_EXTENSIBLE, 48 kHz, stereo, 16-bit, SubFormat = PCM.
    let mut fmt = wave_format(0xFFFE, 2, 48_000);
    fmt.truncate(16);
    fmt.extend(le16b(22));
    fmt.extend(le16b(16));
    fmt.extend(le32b(3));
    fmt.extend(guid(1, 0, 0x10, [0x80, 0, 0, 0xAA, 0, 0x38, 0x9B, 0x71]));
    v.extend(chunk(b"fmt ", &fmt));
    v.extend(b"data");
    v.extend(le32b(u32::MAX));
    v
}

// ----- The sample set -----

struct Sample {
    name: &'static str,
    ext: &'static str,
    bytes: Vec<u8>,
    /// Expected result; a `None` bitrate is not checked.
    want: MediaInfo,
}

fn info(duration: u64, size: Option<(u32, u32)>, sound: Option<(u32, u16)>) -> MediaInfo {
    MediaInfo {
        duration_ms: Some(duration),
        width: size.map(|s| s.0),
        height: size.map(|s| s.1),
        sample_rate: sound.map(|s| s.0),
        channels: sound.map(|s| s.1),
        bitrate_kbps: None,
        has_video: size.is_some(),
        has_audio: sound.is_some(),
    }
}

fn samples() -> Vec<Sample> {
    let s = |name, ext, bytes, want| Sample { name, ext, bytes, want };
    let mut xing_want = info(7836, None, Some((44_100, 2)));
    let xing = mp3_vbr(b"Xing");
    xing_want.bitrate_kbps = Some((xing.len() as u64 * 8 / 7836) as u32);
    let mut cbr_want = info(6000, None, Some((48_000, 2)));
    cbr_want.bitrate_kbps = Some(128);
    let mut vorbis_want = info(12_078, None, Some((44_100, 2)));
    vorbis_want.bitrate_kbps = Some(128);
    let mut theora_want = info(9000, Some((640, 480)), Some((22_050, 1)));
    theora_want.bitrate_kbps = Some(128);
    vec![
        s("mp4_v0", "mp4", mp4_v0(), info(83_500, Some((640, 360)), Some((44_100, 2)))),
        s("mp4_v1_moov_last", "mov", mp4_v1_moov_last(), info(5_000_000, Some((1920, 1080)), None)),
        s("m4a", "m4a", m4a(), info(187_500, None, Some((44_100, 2)))),
        s("mp3_xing", "mp3", xing, xing_want),
        s("mp3_vbri", "mp3", mp3_vbr(b"VBRI"), info(7836, None, Some((44_100, 2)))),
        s("mp3_cbr_id3", "mp3", mp3_cbr_id3(), cbr_want),
        s("flac", "flac", flac_file(), info(27_994, None, Some((44_100, 2)))),
        s("ogg_vorbis", "ogg", ogg_vorbis(), vorbis_want),
        s("ogg_opus", "opus", ogg_opus(), info(5000, None, Some((48_000, 2)))),
        s("ogg_theora_vorbis", "ogv", ogg_theora_vorbis(), theora_want),
        s("mkv_simple", "webm", mkv_simple(), info(65_432, Some((1280, 720)), Some((48_000, 2)))),
        s("mkv_seekhead", "mkv", mkv_seekhead(), info(12_346, Some((1280, 720)), Some((48_000, 2)))),
        s("avi_simple", "avi", avi_simple(), info(10_000, Some((320, 240)), Some((44_100, 2)))),
        s("avi_odml", "avi", avi_odml(), info(33_366, Some((640, 480)), None)),
        s("asf", "wmv", asf_file(), info(12_345, Some((320, 240)), Some((44_100, 2)))),
    ]
}

fn check(name: &str, got: Option<MediaInfo>, want: &MediaInfo) {
    let mut got = got.unwrap_or_else(|| panic!("{name}: not recognized"));
    if want.bitrate_kbps.is_none() {
        got.bitrate_kbps = None;
    }
    assert_eq!(&got, want, "{name}");
}

#[test]
fn synthetic_samples_give_the_expected_facts() {
    for s in samples() {
        check(s.name, parse(&s.bytes, s.ext), &s.want);
        // The extension is only a hint: a wrong one changes nothing.
        check(s.name, parse(&s.bytes, "bin"), &s.want);
    }
}

#[test]
fn mpeg_audio_versions_and_layers() {
    // (header, frame length, frame count, expected ms, rate, channels)
    let cases: [([u8; 4], usize, usize, u64, u32, u16); 5] = [
        ([0xFF, 0xFF, 0xC4, 0x00], 384, 100, 800, 48_000, 2), // MPEG-1 layer I, 384 kbps
        ([0xFF, 0xFD, 0xA4, 0x00], 576, 100, 2400, 48_000, 2), // MPEG-1 layer II, 192 kbps
        ([0xFF, 0xF3, 0x44, 0x00], 96, 100, 2400, 24_000, 2), // MPEG-2 layer III, 32 kbps
        ([0xFF, 0xE3, 0x18, 0xC0], 72, 50, 3600, 8_000, 1),   // MPEG-2.5 layer III, 8 kbps, mono
        ([0xFF, 0xFA, 0x94, 0x00], 384, 125, 3000, 48_000, 2), // MPEG-1 layer III with CRC
    ];
    for (h, len, count, ms, rate, ch) in cases {
        let got = parse(&frames(h, len, count), "mp3").expect("mp3");
        assert_eq!((got.duration_ms, got.sample_rate, got.channels), (Some(ms), Some(rate), Some(ch)), "{h:02X?}");
    }
    // Xing after mono MPEG-1 side info (offset 21), stereo MPEG-2 side info (offset 21)
    // and mono MPEG-2 side info (offset 13).
    let xing_cases: [([u8; 4], usize, usize, u64); 3] = [
        ([0xFF, 0xFB, 0x90, 0xC0], 417, 21, 2612), // 100 frames * 1152 / 44100
        ([0xFF, 0xF3, 0x44, 0x00], 96, 21, 4800),  // 200 frames * 576 / 24000
        ([0xFF, 0xF3, 0x44, 0xC0], 96, 13, 4800),
    ];
    for (h, len, at, ms) in xing_cases {
        let n = if h[1] == 0xFB { 100u32 } else { 200 };
        let mut v = frames(h, len, 20);
        v[at..at + 4].copy_from_slice(b"Xing");
        v[at + 4..at + 8].copy_from_slice(&be32b(1));
        v[at + 8..at + 12].copy_from_slice(&be32b(n));
        assert_eq!(parse(&v, "mp3").and_then(|i| i.duration_ms), Some(ms), "{h:02X?}");
    }
    // Junk before the first frame is skipped; a lone sync word is not taken for a frame.
    let mut v = vec![0xFF, 0xFB, 0x00, 0x13, 0x37];
    v.extend(frames([0xFF, 0xFB, 0x94, 0x00], 384, 125));
    assert_eq!(parse(&v, "mp3").and_then(|i| i.duration_ms), Some(3000));
    assert_eq!(parse(&[0xFF, 0xFB, 0x94, 0x00, 1, 2, 3, 4, 5, 6, 7, 8], "bin"), None);
}

#[test]
fn wav_variants() {
    // Plain PCM with fmt after an odd-sized LIST chunk (padding), then a cut-off data chunk.
    let mut v = riff(
        b"WAVE",
        &[chunk(b"LIST", &[0u8; 5]), chunk(b"fmt ", &wave_format(1, 2, 44_100)), chunk(b"data", &vec![0u8; 176_400])],
    );
    let got = parse(&v, "wav").unwrap();
    assert_eq!(
        (got.duration_ms, got.sample_rate, got.channels, got.bitrate_kbps),
        (Some(1000), Some(44_100), Some(2), Some(1411))
    );
    v.truncate(v.len() - 88_200);
    assert_eq!(parse(&v, "wav").unwrap().duration_ms, Some(500));
    // Compressed (IMA ADPCM) with a fact chunk: the sample count decides.
    let mut fmt = wave_format(0x11, 1, 22_050);
    fmt[8..12].copy_from_slice(&le32b(11_100));
    let v = riff(b"WAVE", &[chunk(b"fmt ", &fmt), chunk(b"fact", &le32b(44_100)), chunk(b"data", &[0u8; 1000])]);
    assert_eq!(parse(&v, "wav").unwrap().duration_ms, Some(2000));
    // RF64 with WAVE_FORMAT_EXTENSIBLE: the size comes from ds64.
    let mut v = wav_rf64_head(192_000);
    v.extend(vec![0u8; 192_000]);
    let got = parse(&v, "wav").unwrap();
    assert_eq!((got.duration_ms, got.sample_rate, got.channels), (Some(1000), Some(48_000), Some(2)));
}

#[test]
fn asf_guid_bytes_are_mixed_endian() {
    assert_eq!(
        ASF_HEADER,
        [0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C]
    );
}

#[test]
fn format_duration_shape() {
    assert_eq!(format_duration(0), "00:00:00");
    assert_eq!(format_duration(205_000), "00:03:25");
    assert_eq!(format_duration(3_600_000 * 2 + 61_999), "02:01:01");
    assert_eq!(format_duration(3_600_000 * 123), "123:00:00");
}

/// Huge files: only a few KB are read, and 64-bit positions work.
#[test]
fn big_files_are_read_sparingly() {
    const GIB: u64 = 1 << 30;
    let run = |name: &str, len: u64, pieces: Vec<(u64, Vec<u8>)>, ext: &str| {
        let mut r = Counting { inner: Sparse { len, pos: 0, pieces }, bytes: 0, piece: usize::MAX };
        let got = read(&mut r, ext);
        println!("{name}: {} bytes read, {got:?}", r.bytes);
        assert!(r.bytes < 300_000, "{name}: {} bytes read", r.bytes);
        got.unwrap_or_else(|| panic!("{name}: not recognized"))
    };
    // MP4: a 5 GiB mdat with a 64-bit size, moov after it.
    let mdat_len = 5 * GIB + 16;
    let mut mdat_head = be32b(1).to_vec();
    mdat_head.extend(b"mdat");
    mdat_head.extend(be64b(mdat_len));
    let head = [ftyp(b"isom"), mdat_head].concat();
    let moov_at = head.len() as u64 + mdat_len - 16;
    let moov = moov_v0();
    let file_len = moov_at + moov.len() as u64;
    let got = run("mp4 5 GiB", file_len, vec![(0, head), (moov_at, moov)], "mp4");
    assert_eq!((got.duration_ms, got.width, got.height), (Some(83_500), Some(640), Some(360)));
    assert_eq!(got.bitrate_kbps, Some((file_len * 8 / 83_500) as u32));
    // CBR MP3 of 3 GiB: estimated from the first frame and the size.
    let len = 3 * GIB;
    let got = run("mp3 3 GiB", len, vec![(0, frames([0xFF, 0xFB, 0x94, 0x00], 384, 200))], "mp3");
    assert_eq!(got.duration_ms, Some(len * 8 / 128));
    // RF64 WAV with 6 GiB of data.
    let data = 6 * GIB;
    let head = wav_rf64_head(data);
    let got = run("rf64 6 GiB", head.len() as u64 + data, vec![(0, head)], "wav");
    assert_eq!(got.duration_ms, Some(data * 1000 / 192_000));
    // Ogg Vorbis of 2 GiB: the last page is found in the tail.
    let first = [page(2, 0, 7, 0, &[&vorbis_id(2, 48_000)]), page(0, 0, 7, 1, &[&vorbis_comment()])].concat();
    let last = page(4, 48_000 * 3600 * 3, 7, 999, &[&[0u8; 100]]);
    let len = 2 * GIB;
    let got = run("ogg 2 GiB", len, vec![(0, first), (len - last.len() as u64 - 50, last)], "ogg");
    assert_eq!(got.duration_ms, Some(3600 * 3 * 1000));
}

/// Inputs built to make a careless parser loop or read a lot.
#[test]
fn pathological_inputs_stop_quickly() {
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        // 200 000 empty boxes before moov.
        ("many boxes", [ftyp(b"isom"), b"\0\0\0\x08free".repeat(200_000), moov_v0()].concat(), "mp4"),
        // A RIFF WAVE of 100 000 tiny chunks.
        ("many chunks", riff(b"WAVE", &[b"junk\0\0\0\0".repeat(100_000)]), "wav"),
        // An ID3 tag that claims to be 256 MB long.
        ("huge id3", [b"ID3\x04\0\0\x7F\x7F\x7F\x7F".as_slice(), &[0u8; 100]].concat(), "mp3"),
        // 1 MB of sync bytes.
        ("all FF", vec![0xFF; 1 << 20], "mp3"),
        // 1 MB of "OggS".
        ("all OggS", b"OggS".repeat(1 << 18), "ogg"),
        // EBML elements of unknown size, nested deeply.
        ("mkv unknown sizes", [ebml_header("webm"), [0x18, 0x53, 0x80, 0x67, 0xFF].repeat(50_000)].concat(), "mkv"),
        // moov of size 0 holding a trak of huge size.
        ("mp4 odd sizes", [ftyp(b"isom"), b"\0\0\0\0moov\xFF\xFF\xFF\xFFtrak\0\0\0\x01mdia".to_vec()].concat(), "mp4"),
    ];
    for (name, bytes, ext) in cases {
        let t = Instant::now();
        let got = parse(&bytes, ext);
        println!("{name}: {got:?} in {:?}", t.elapsed());
        assert!(t.elapsed() < Duration::from_secs(2), "{name}");
    }
}

// ----- Robustness: truncation and byte flips -----

struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Parses 100 truncated copies and 300 copies with random bytes changed; returns the slowest parse.
fn shake(data: &mut [u8], ext: &str, rng: &mut XorShift) -> Duration {
    let mut slowest = Duration::ZERO;
    let mut timed = |bytes: &[u8]| {
        let t = Instant::now();
        let _ = parse(bytes, ext);
        slowest = slowest.max(t.elapsed());
    };
    for i in 0..100 {
        timed(&data[..data.len() * i / 100]);
    }
    let len = data.len();
    for _ in 0..300 {
        let mut changed = Vec::new();
        for _ in 0..1 + rng.below(4) {
            // Aim most changes at the bytes parsers read: the start and the end.
            let pos = match rng.below(4) {
                0 | 1 => rng.below(len.min(4096)),
                2 => len - 1 - rng.below(len.min(65_536)),
                _ => rng.below(len),
            };
            changed.push((pos, data[pos]));
            data[pos] ^= 1 + rng.below(255) as u8;
        }
        timed(data);
        for (pos, old) in changed.into_iter().rev() {
            data[pos] = old;
        }
    }
    slowest
}

#[test]
fn fuzz_synthetic_samples() {
    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    for mut s in samples() {
        let slowest = shake(&mut s.bytes, s.ext, &mut rng);
        println!("{}: 400 variants, slowest parse {slowest:?}", s.name);
        assert!(slowest < Duration::from_secs(1), "{}", s.name);
    }
}

#[test]
fn fuzz_random_bytes_behind_each_signature() {
    let mut rng = XorShift(0x0123_4567_89AB_CDEF);
    let heads: [(&[u8], &str); 10] = [
        (b"RIFF\xFF\xFF\xFF\xFFWAVE", "wav"),
        (b"RIFF\xFF\xFF\xFF\xFFAVI LIST\x00\x01\x00\x00hdrl", "avi"),
        (b"fLaC\x00\x00\x00\x22", "flac"),
        (b"OggS\x00\x02", "ogg"),
        (&[0x1A, 0x45, 0xDF, 0xA3, 0x80, 0x18, 0x53, 0x80, 0x67, 0xFF], "mkv"),
        (&ASF_HEADER, "wma"),
        (b"ID3\x03\x00\x00\x00\x00\x00\x10", "mp3"),
        (b"\x00\x00\x00\x10ftypisom\x00\x00\x00\x00", "mp4"),
        (b"\x00\x00\x10\x00moov", "mov"),
        (&[0xFF, 0xFB, 0x90, 0x00], "mp3"),
    ];
    for (head, ext) in heads {
        for _ in 0..300 {
            let mut v = head.to_vec();
            let n = rng.below(16 * 1024);
            // Mostly small values, so that sizes and counts are plausible.
            v.extend((0..n).map(|_| if rng.below(3) == 0 { rng.next() as u8 } else { (rng.next() % 4) as u8 }));
            let t = Instant::now();
            let _ = parse(&v, ext);
            assert!(t.elapsed() < Duration::from_secs(1));
        }
    }
}

// ----- Windows as the reference -----

mod win {
    use std::path::Path;
    use windows::core::HSTRING;
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::Storage::EnhancedStorage::{
        PKEY_Audio_ChannelCount, PKEY_Audio_EncodingBitrate, PKEY_Audio_SampleRate, PKEY_Media_Duration,
        PKEY_Video_FrameHeight, PKEY_Video_FrameWidth, PKEY_Video_TotalBitrate,
    };
    use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToUInt64, PROPVARIANT};
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
    use windows::Win32::System::Variant::{VT_EMPTY, VT_UI8};
    use windows::Win32::UI::Shell::PropertiesSystem::{
        IPropertyStore, PSFormatForDisplay, SHGetPropertyStoreFromParsingName, GPS_DEFAULT, PDFF_DEFAULT,
    };

    fn init() {
        // Test threads are fresh; an "already initialized" result is fine.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
    }

    /// What Windows' property handler says about a file.
    #[derive(Debug, Default)]
    pub struct Facts {
        pub duration_ms: Option<u64>,
        pub width: Option<u64>,
        pub height: Option<u64>,
        pub sample_rate: Option<u64>,
        pub channels: Option<u64>,
        pub audio_kbps: Option<u64>,
        pub total_kbps: Option<u64>,
    }

    fn value(store: &IPropertyStore, key: &PROPERTYKEY) -> Option<u64> {
        unsafe {
            let mut v = store.GetValue(key).ok()?;
            let result = if v.Anonymous.Anonymous.vt == VT_EMPTY { None } else { PropVariantToUInt64(&v).ok() };
            let _ = PropVariantClear(&mut v);
            result
        }
    }

    pub fn facts(path: &Path) -> Facts {
        init();
        let Ok(store) = (unsafe {
            SHGetPropertyStoreFromParsingName::<_, _, IPropertyStore>(&HSTRING::from(path), None, GPS_DEFAULT)
        }) else {
            return Facts::default();
        };
        let get = |key| value(&store, key);
        Facts {
            duration_ms: get(&PKEY_Media_Duration).map(|t| t / 10_000),
            width: get(&PKEY_Video_FrameWidth),
            height: get(&PKEY_Video_FrameHeight),
            sample_rate: get(&PKEY_Audio_SampleRate),
            channels: get(&PKEY_Audio_ChannelCount),
            audio_kbps: get(&PKEY_Audio_EncodingBitrate).map(|b| b / 1000),
            total_kbps: get(&PKEY_Video_TotalBitrate).map(|b| b / 1000),
        }
    }

    /// How the property system displays a duration of `ms`.
    pub fn format(ms: u64) -> String {
        init();
        unsafe {
            let mut v = PROPVARIANT::default();
            (*v.Anonymous.Anonymous).vt = VT_UI8;
            (*v.Anonymous.Anonymous).Anonymous.uhVal = ms * 10_000;
            let mut buf = [0u16; 128];
            PSFormatForDisplay(&PKEY_Media_Duration, &v, PDFF_DEFAULT, &mut buf).expect("PSFormatForDisplay");
            let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            String::from_utf16_lossy(&buf[..n])
        }
    }
}

fn close_enough(a: u64, b: u64) -> bool {
    let diff = a.abs_diff(b);
    diff <= 50 || diff * 100 <= a.max(b)
}

/// Reads a file on disk with `read` and through Windows' property store, prints both, and
/// returns where they disagree. Facts Windows does not give are not compared, and bitrates
/// are only printed (Windows defines them differently per format).
fn compare_with_windows(path: &Path, ext: &str) -> (Option<MediaInfo>, win::Facts, Vec<String>) {
    let ours = read(&mut std::fs::File::open(path).expect("open"), ext);
    let w = win::facts(path);
    let o = ours.clone().unwrap_or_default();
    let mut problems = Vec::new();
    if let Some(t) = w.duration_ms {
        match o.duration_ms {
            Some(d) if close_enough(d, t) => {}
            // Windows' FLAC handler cuts the length to whole seconds.
            Some(d) if ext == "flac" && d / 1000 * 1000 == t => {}
            d => problems.push(format!("duration {d:?} vs {t}")),
        }
    }
    let mut same = |name: &str, ours: Option<u64>, theirs: Option<u64>| {
        if let Some(t) = theirs.filter(|&t| t > 0) {
            if ours != Some(t) {
                problems.push(format!("{name} {ours:?} vs {t}"));
            }
        }
    };
    same("width", o.width.map(u64::from), w.width);
    same("height", o.height.map(u64::from), w.height);
    same("sample rate", o.sample_rate.map(u64::from), w.sample_rate);
    same("channels", o.channels.map(u64::from), w.channels);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let name: String = name.chars().take(44).collect();
    let size = |w: Option<u64>, h: Option<u64>| match (w, h) {
        (Some(w), Some(h)) => format!("{w}x{h}"),
        _ => "-".to_string(),
    };
    let opt = |v: Option<u64>| v.map_or("-".to_string(), |v| v.to_string());
    println!(
        "{name:<44} ms {:>8}/{:<8} size {:>9}/{:<9} rate {:>5}/{:<5} ch {}/{} kbps {:>5}/{}a/{}t {}",
        opt(o.duration_ms),
        opt(w.duration_ms),
        size(o.width.map(u64::from), o.height.map(u64::from)),
        size(w.width, w.height),
        opt(o.sample_rate.map(u64::from)),
        opt(w.sample_rate),
        opt(o.channels.map(u64::from)),
        opt(w.channels),
        opt(o.bitrate_kbps.map(u64::from)),
        opt(w.audio_kbps),
        opt(w.total_kbps),
        if problems.is_empty() { "ok" } else { "DISAGREES" }
    );
    (ours, w, problems)
}

fn windows_media_wavs() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(r"C:\Windows\Media")
        .map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default();
    v.retain(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav")));
    v.sort();
    v
}

/// Media files (other than WAV) that ship with Windows, found at run time. They are only
/// read, in place. Files over 64 MB are left out because the fuzz test loads them whole.
fn windows_shipped_media() -> Vec<(PathBuf, String)> {
    const EXTS: [&str; 14] =
        ["mp3", "mp4", "m4a", "m4v", "mov", "3gp", "webm", "mkv", "wma", "wmv", "avi", "flac", "ogg", "opus"];
    fn walk(dir: &Path, depth: u32, out: &mut Vec<(PathBuf, String)>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let Ok(kind) = e.file_type() else { continue };
            let p = e.path();
            if kind.is_dir() && depth > 0 {
                walk(&p, depth - 1, out);
            } else if kind.is_file() {
                let ext = p.extension().map(|x| x.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
                let small = e.metadata().is_ok_and(|m| m.len() <= 64 << 20);
                if EXTS.contains(&ext.as_str()) && small {
                    out.push((p, ext));
                }
            }
        }
    }
    let mut out = Vec::new();
    for root in [
        r"C:\Windows\ImmersiveControlPanel",
        r"C:\Windows\SystemApps",
        r"C:\Windows\SystemResources",
        r"C:\Windows\Media",
    ] {
        walk(Path::new(root), 8, &mut out);
    }
    // The retail demo video (a real ASF file) sits in a WinSxS component folder; walking all
    // of WinSxS would take minutes, so only those folders are looked into.
    if let Ok(entries) = std::fs::read_dir(r"C:\Windows\WinSxS") {
        for e in entries.flatten() {
            if e.file_name().to_string_lossy().contains("content-neutral") {
                walk(&e.path(), 1, &mut out);
            }
        }
    }
    out.sort();
    out
}

#[test]
fn windows_media_wavs_match_the_property_store() {
    // reads Windows' own files and asks its property store: run with DVFILES_SHELLTEST=1
    if std::env::var_os("DVFILES_SHELLTEST").is_none() {
        return;
    }
    let paths = windows_media_wavs();
    assert!(!paths.is_empty(), "no WAV files in C:\\Windows\\Media");
    let (mut compared, mut problems) = (0, Vec::new());
    for p in &paths {
        let (ours, w, bad) = compare_with_windows(p, "wav");
        assert!(ours.and_then(|i| i.duration_ms).is_some(), "{}: no duration", p.display());
        compared += usize::from(w.duration_ms.is_some());
        problems.extend(bad.into_iter().map(|b| format!("{}: {b}", p.display())));
    }
    println!("{} WAV files, {compared} with a Windows duration", paths.len());
    assert!(compared > 0, "the property store gave no durations");
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn windows_shipped_media_files_match_the_property_store() {
    // reads Windows' own files and asks its property store: run with DVFILES_SHELLTEST=1
    if std::env::var_os("DVFILES_SHELLTEST").is_none() {
        return;
    }
    let files = windows_shipped_media();
    let mut problems = Vec::new();
    for (p, ext) in &files {
        let (ours, _, bad) = compare_with_windows(p, ext);
        assert!(ours.is_some(), "{}: not recognized", p.display());
        problems.extend(bad.into_iter().map(|b| format!("{}: {b}", p.display())));
    }
    println!("{} media files shipped with Windows compared", files.len());
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn format_duration_matches_the_property_system() {
    // reads Windows' own files and asks its property store: run with DVFILES_SHELLTEST=1
    if std::env::var_os("DVFILES_SHELLTEST").is_none() {
        return;
    }
    for ms in [0, 499, 500, 999, 1000, 1499, 1500, 59_499, 59_500, 61_000, 3_599_500, 3_600_000, 360_000_000] {
        let theirs = win::format(ms);
        // Keep only digits and separators (the shell may add direction marks).
        let theirs: String = theirs.chars().filter(|c| c.is_ascii_digit() || *c == ':').collect();
        println!("{ms:>10} ms: ours {} windows {}", format_duration(ms), theirs);
        assert_eq!(format_duration(ms), theirs, "{ms} ms");
    }
}

/// Writes the synthetic samples to disk and asks Windows about them too. Not every handler
/// accepts these minimal files; where Windows gives a value, it must agree.
#[test]
fn windows_cross_check_of_synthetic_samples() {
    // reads Windows' own files and asks its property store: run with DVFILES_SHELLTEST=1
    if std::env::var_os("DVFILES_SHELLTEST").is_none() {
        return;
    }
    // Under the crate's target folder, which builds already own (and version control ignores).
    let dir = std::env::temp_dir().join(format!("dvfiles_media_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create testfiles");
    let mut all = samples();
    // A second FLAC just under 11 s shows how Windows rounds FLAC lengths.
    all.push(Sample {
        name: "flac_10999",
        ext: "flac",
        bytes: flac_with(44_100 * 11 - 1),
        want: info(10_999, None, Some((44_100, 2))),
    });
    // The SeekHead layout with a known Segment size, which Windows' handler may accept.
    all.push(Sample {
        name: "mkv_seekhead_sized",
        ext: "mkv",
        bytes: mkv_seekhead_with(true),
        want: info(12_346, Some((1280, 720)), Some((48_000, 2))),
    });
    let (mut with_duration, mut problems) = (0, Vec::new());
    for s in all {
        let path = dir.join(format!("{}.{}", s.name, s.ext));
        std::fs::write(&path, &s.bytes).expect("write sample");
        let (ours, w, bad) = compare_with_windows(&path, s.ext);
        // Read through `File` it gives the same as in memory.
        check(s.name, ours, &s.want);
        with_duration += usize::from(w.duration_ms.is_some());
        problems.extend(bad.into_iter().map(|b| format!("{}: {b}", s.name)));
    }
    println!("Windows gave a duration for {with_duration} samples");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn fuzz_windows_media_wavs() {
    // reads Windows' own files and asks its property store: run with DVFILES_SHELLTEST=1
    if std::env::var_os("DVFILES_SHELLTEST").is_none() {
        return;
    }
    let mut rng = XorShift(0xDEAD_BEEF_CAFE_F00D);
    let mut slowest = Duration::ZERO;
    let paths = windows_media_wavs();
    for p in &paths {
        let mut data = std::fs::read(p).expect("read wav");
        slowest = slowest.max(shake(&mut data, "wav", &mut rng));
    }
    println!("{} WAV files x 400 variants, slowest parse {slowest:?}", paths.len());
    assert!(slowest < Duration::from_secs(1));
}

#[test]
fn fuzz_windows_shipped_media_files() {
    // reads Windows' own files and asks its property store: run with DVFILES_SHELLTEST=1
    if std::env::var_os("DVFILES_SHELLTEST").is_none() {
        return;
    }
    let mut rng = XorShift(0x005E_ED0F_F00D_1234);
    let mut slowest = Duration::ZERO;
    let files = windows_shipped_media();
    for (p, ext) in &files {
        // A copy in memory; the file itself is never written.
        let mut data = std::fs::read(p).expect("read media file");
        slowest = slowest.max(shake(&mut data, ext, &mut rng));
    }
    println!("{} media files x 400 variants, slowest parse {slowest:?}", files.len());
    assert!(slowest < Duration::from_secs(1));
}
