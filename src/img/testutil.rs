// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Test-only helpers shared by the decoder tests:
//! - WIC (Windows Imaging Component) as the reference decoder and as an encoder for
//!   synthetic samples,
//! - discovery of sample files under C:\Windows (read-only),
//! - deterministic fuzzing (truncations and single-byte flips) with panic capture,
//! - pixel comparison.
//!
//! In the real crate this file sits next to the decoders (e.g. `img/testutil.rs`) and is
//! declared with `#[cfg(test)] mod testutil;` in the parent module.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashSet};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use windows::core::{GUID, HSTRING, PWSTR};
use windows::Win32::Foundation::{GENERIC_WRITE, VARIANT_FALSE, VARIANT_TRUE};
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::StructuredStorage::PROPBAG2;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::System::Variant::{VARIANT, VT_BOOL, VT_UI1};

// ---------------------------------------------------------------- paths

/// Folder for files the tests create, in TEMP (dvfiles_imgtests).
pub fn testdata_dir(sub: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("dvfiles_imgtests").join(sub);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

// ---------------------------------------------------------------- WIC

fn factory() -> windows::core::Result<IWICImagingFactory> {
    unsafe {
        // Every test thread needs COM; repeated calls on a thread are harmless.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)
    }
}

/// An image decoded by WIC and converted to 32bppPBGRA.
pub struct WicImage {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u32>,
    /// The frame's own pixel format before conversion.
    pub native: GUID,
}

/// Decodes frame 0 with WIC and converts it to premultiplied BGRA.
pub fn wic_decode(data: &[u8]) -> windows::core::Result<WicImage> {
    unsafe {
        let f = factory()?;
        let stream = f.CreateStream()?;
        stream.InitializeFromMemory(data)?;
        let dec = f.CreateDecoderFromStream(&stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?;
        let frame = dec.GetFrame(0)?;
        let native = frame.GetPixelFormat()?;
        let conv = f.CreateFormatConverter()?;
        conv.Initialize(
            &frame,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;
        let (mut w, mut h) = (0u32, 0u32);
        conv.GetSize(&mut w, &mut h)?;
        let mut buf = vec![0u8; w as usize * h as usize * 4];
        conv.CopyPixels(std::ptr::null(), w * 4, &mut buf)?;
        let px = buf.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        Ok(WicImage { w, h, px, native })
    }
}

/// Short name of a WIC pixel format, for reports.
pub fn format_name(g: &GUID) -> String {
    let table: [(&GUID, &str); 22] = [
        (&GUID_WICPixelFormat1bppIndexed, "1bppIndexed"),
        (&GUID_WICPixelFormat2bppIndexed, "2bppIndexed"),
        (&GUID_WICPixelFormat4bppIndexed, "4bppIndexed"),
        (&GUID_WICPixelFormat8bppIndexed, "8bppIndexed"),
        (&GUID_WICPixelFormatBlackWhite, "BlackWhite"),
        (&GUID_WICPixelFormat2bppGray, "2bppGray"),
        (&GUID_WICPixelFormat4bppGray, "4bppGray"),
        (&GUID_WICPixelFormat8bppGray, "8bppGray"),
        (&GUID_WICPixelFormat16bppGray, "16bppGray"),
        (&GUID_WICPixelFormat16bppBGR555, "16bppBGR555"),
        (&GUID_WICPixelFormat16bppBGR565, "16bppBGR565"),
        (&GUID_WICPixelFormat24bppBGR, "24bppBGR"),
        (&GUID_WICPixelFormat24bppRGB, "24bppRGB"),
        (&GUID_WICPixelFormat32bppBGR, "32bppBGR"),
        (&GUID_WICPixelFormat32bppBGRA, "32bppBGRA"),
        (&GUID_WICPixelFormat32bppPBGRA, "32bppPBGRA"),
        (&GUID_WICPixelFormat32bppRGBA, "32bppRGBA"),
        (&GUID_WICPixelFormat48bppRGB, "48bppRGB"),
        (&GUID_WICPixelFormat64bppRGBA, "64bppRGBA"),
        (&GUID_WICPixelFormat16bppBGRA5551, "16bppBGRA5551"),
        (&GUID_WICPixelFormat64bppPRGBA, "64bppPRGBA"),
        (&GUID_WICPixelFormat32bppGrayFloat, "32bppGrayFloat"),
    ];
    for (k, v) in table {
        if k == g {
            return v.to_string();
        }
    }
    format!("{:?}", g)
}

/// A value for an encoder option written to the WIC property bag.
#[derive(Clone, Copy)]
pub enum Opt {
    Bool(bool),
    U8(u8),
}

fn variant(v: Opt) -> VARIANT {
    let mut var = VARIANT::default();
    unsafe {
        let inner = &mut *var.Anonymous.Anonymous;
        match v {
            Opt::Bool(b) => {
                inner.vt = VT_BOOL;
                inner.Anonymous.boolVal = if b { VARIANT_TRUE } else { VARIANT_FALSE };
            }
            Opt::U8(x) => {
                inner.vt = VT_UI1;
                inner.Anonymous.bVal = x;
            }
        }
    }
    var
}

/// Encodes one frame with a WIC encoder and writes it to `path`.
/// Fails (with a reason) if the encoder does not accept `format` exactly.
pub fn wic_encode(
    path: &Path,
    container: &GUID,
    format: &GUID,
    w: u32,
    h: u32,
    stride: u32,
    pixels: &[u8],
    palette: Option<&[u32]>,
    options: &[(&str, Opt)],
) -> Result<(), String> {
    let r: windows::core::Result<Result<(), String>> = unsafe {
        (|| {
            let f = factory()?;
            let stream = f.CreateStream()?;
            let _ = std::fs::remove_file(path);
            stream.InitializeFromFilename(&HSTRING::from(path), GENERIC_WRITE.0)?;
            let enc = f.CreateEncoder(container, std::ptr::null())?;
            enc.Initialize(&stream, WICBitmapEncoderNoCache)?;
            let mut frame: Option<IWICBitmapFrameEncode> = None;
            let mut bag = None;
            enc.CreateNewFrame(&mut frame, &mut bag)?;
            let frame = frame.ok_or_else(|| windows::core::Error::from_hresult(windows::core::HRESULT(-1)))?;
            if let Some(bag) = &bag {
                for (name, value) in options {
                    let mut wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
                    let prop = PROPBAG2 { pstrName: PWSTR(wide.as_mut_ptr()), ..Default::default() };
                    let var = variant(*value);
                    if let Err(e) = bag.Write(1, &prop, &var) {
                        return Ok(Err(format!("option {} rejected: {}", name, e)));
                    }
                }
            }
            frame.Initialize(bag.as_ref())?;
            frame.SetSize(w, h)?;
            let mut fmt = *format;
            frame.SetPixelFormat(&mut fmt)?;
            if fmt != *format {
                return Ok(Err(format!("encoder changed {} to {}", format_name(format), format_name(&fmt))));
            }
            if let Some(colors) = palette {
                let pal = f.CreatePalette()?;
                pal.InitializeCustom(colors)?;
                frame.SetPalette(&pal)?;
            }
            frame.WritePixels(h, stride, pixels)?;
            frame.Commit()?;
            enc.Commit()?;
            Ok(Ok(()))
        })()
    };
    match r {
        Ok(inner) => inner,
        Err(e) => Err(e.to_string()),
    }
}

// ---------------------------------------------------------------- comparison

/// Result of comparing two premultiplied BGRA images channel by channel.
#[derive(Default, Debug, Clone)]
pub struct Diff {
    /// Largest channel difference.
    pub max: u32,
    /// Number of pixels with any difference.
    pub pixels: usize,
    /// First pixel with the largest difference: (x, y, ours, theirs).
    pub worst: Option<(u32, u32, u32, u32)>,
}

/// Compares a `w` x `h` window of `ours` (stride `ours_w`, origin `ox`,`oy`) with `theirs`.
pub fn compare_region(ours: &[u32], ours_w: u32, ox: u32, oy: u32, theirs: &[u32], w: u32, h: u32) -> Diff {
    let mut d = Diff::default();
    for y in 0..h {
        for x in 0..w {
            let a = ours[((oy + y) * ours_w + ox + x) as usize];
            let b = theirs[(y * w + x) as usize];
            if a == b {
                continue;
            }
            d.pixels += 1;
            let m = (0..4).map(|k| ((a >> (8 * k)) & 255).abs_diff((b >> (8 * k)) & 255)).max().unwrap_or(0);
            if m > d.max {
                d.max = m;
                d.worst = Some((x, y, a, b));
            }
        }
    }
    d
}

/// Compares two whole images of the same size.
pub fn compare(ours: &[u32], theirs: &[u32], w: u32, h: u32) -> Diff {
    compare_region(ours, w, 0, 0, theirs, w, h)
}

/// Tally of comparison outcomes for a set of files.
#[derive(Default)]
pub struct Tally {
    pub exact: usize,
    pub within1: usize,
    pub within1_names: Vec<String>,
    pub differ: Vec<String>,
    pub ours_failed: Vec<String>,
    pub wic_failed: Vec<String>,
    pub size_mismatch: Vec<String>,
}

impl Tally {
    pub fn add_diff(&mut self, name: &str, d: &Diff, note: &str) {
        if d.max == 0 {
            self.exact += 1;
        } else if d.max <= 1 {
            self.within1 += 1;
            self.within1_names.push(format!("{} ({} px) [{}]", name, d.pixels, note));
        } else {
            let worst = d
                .worst
                .map(|(x, y, a, b)| format!(" worst at ({},{}) ours {:08X} wic {:08X}", x, y, a, b))
                .unwrap_or_default();
            self.differ.push(format!("{}: max diff {}, {} px differ{} [{}]", name, d.max, d.pixels, worst, note));
        }
    }

    pub fn print(&self, title: &str) {
        println!(
            "== {}: exact {}, within 1 {}, differ {}, our decoder failed {}, WIC failed {}, size mismatch {}",
            title,
            self.exact,
            self.within1,
            self.differ.len(),
            self.ours_failed.len(),
            self.wic_failed.len(),
            self.size_mismatch.len()
        );
        for (label, list) in [
            ("within 1", &self.within1_names),
            ("DIFFER", &self.differ),
            ("OURS FAILED", &self.ours_failed),
            ("WIC FAILED", &self.wic_failed),
            ("SIZE MISMATCH", &self.size_mismatch),
        ] {
            for s in list {
                println!("   {}: {}", label, s);
            }
        }
    }
}

// ---------------------------------------------------------------- samples

/// Sample files found under C:\Windows (read-only), deduplicated by content.
pub struct Samples {
    pub png: Vec<PathBuf>,
    pub bmp: Vec<PathBuf>,
    pub gif: Vec<PathBuf>,
}

const MAX_PNG: usize = 300;
const MAX_BMP: usize = 100;
const MAX_GIF: usize = 200;
const MAX_FILE: u64 = 40 << 20;

/// The sample lists, built once per test process (and cached in testdata/samples.txt).
pub fn samples() -> &'static Samples {
    static S: OnceLock<Samples> = OnceLock::new();
    S.get_or_init(|| {
        // samples from all over C:\Windows only when asked for (DVFILES_SHELLTEST=1): finding them takes a while
        if std::env::var_os("DVFILES_SHELLTEST").is_none() {
            return Samples { png: vec![], bmp: vec![], gif: vec![] };
        }
        let cache = testdata_dir("").join("samples.txt");
        if let Ok(text) = std::fs::read_to_string(&cache) {
            let mut s = Samples { png: vec![], bmp: vec![], gif: vec![] };
            for line in text.lines() {
                if let Some((kind, path)) = line.split_once('|') {
                    let p = PathBuf::from(path);
                    match kind {
                        "png" => s.png.push(p),
                        "bmp" => s.bmp.push(p),
                        "gif" => s.gif.push(p),
                        _ => {}
                    }
                }
            }
            if !s.png.is_empty() {
                return s;
            }
        }
        let s = find_samples();
        let mut text = String::new();
        for (kind, list) in [("png", &s.png), ("bmp", &s.bmp), ("gif", &s.gif)] {
            for p in list {
                text.push_str(&format!("{}|{}\n", kind, p.display()));
            }
        }
        let _ = std::fs::write(&cache, text);
        s
    })
}

/// FNV-1a hash, used to deduplicate files and to order candidates.
pub fn fnv(data: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for &b in data {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    h
}

/// Walks a directory tree (not following links or reparse points), collecting files
/// by extension.
fn walk(root: &Path, found: &mut BTreeMap<String, Vec<PathBuf>>) {
    use std::os::windows::fs::MetadataExt;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let Ok(md) = entry.metadata() else { continue };
            if md.file_attributes() & 0x400 != 0 {
                continue; // reparse point (symlink / junction)
            }
            let path = entry.path();
            if md.is_dir() {
                stack.push(path);
            } else if md.len() > 0 && md.len() <= MAX_FILE {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    let ext = ext.to_ascii_lowercase();
                    if ext == "png" || ext == "bmp" || ext == "gif" {
                        found.entry(ext).or_default().push(path);
                    }
                }
            }
        }
    }
}

fn find_samples() -> Samples {
    let mut found = BTreeMap::new();
    walk(Path::new(r"C:\Windows"), &mut found);
    let take = |ext: &str| -> Vec<PathBuf> {
        let mut v = found.get(ext).cloned().unwrap_or_default();
        // SystemApps first (as requested), then everything else; within each group
        // a hash order spreads the picks over many directories.
        v.sort_by_key(|p| {
            let s = p.to_string_lossy().to_ascii_lowercase();
            (!s.starts_with(r"c:\windows\systemapps\"), fnv(s.as_bytes()))
        });
        v
    };

    // PNG: bucket by (color type, bit depth, interlace) and pick round-robin so rare
    // variants are all included.
    let mut buckets: BTreeMap<(u8, u8, u8), Vec<PathBuf>> = BTreeMap::new();
    for p in take("png") {
        let mut head = [0u8; 29];
        let ok = std::fs::File::open(&p).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head)).is_ok();
        if ok && &head[12..16] == b"IHDR" {
            buckets.entry((head[25], head[24], head[28])).or_default().push(p);
        }
    }
    let mut seen = HashSet::new();
    let mut png = Vec::new();
    let mut cursors: Vec<(usize, Vec<PathBuf>)> = buckets.into_values().map(|v| (0, v)).collect();
    while png.len() < MAX_PNG && cursors.iter().any(|(i, v)| *i < v.len()) {
        for (i, v) in cursors.iter_mut() {
            while *i < v.len() {
                let p = v[*i].clone();
                *i += 1;
                if let Ok(data) = std::fs::read(&p) {
                    if seen.insert(fnv(&data)) {
                        png.push(p);
                        break;
                    }
                }
            }
            if png.len() >= MAX_PNG {
                break;
            }
        }
    }

    let unique = |ext: &str, max: usize, seen: &mut HashSet<u64>| -> Vec<PathBuf> {
        let mut out = Vec::new();
        for p in take(ext) {
            if out.len() >= max {
                break;
            }
            if let Ok(data) = std::fs::read(&p) {
                if seen.insert(fnv(&data)) {
                    out.push(p);
                }
            }
        }
        out
    };
    let bmp = unique("bmp", MAX_BMP, &mut seen);
    let gif = unique("gif", MAX_GIF, &mut seen);
    Samples { png, bmp, gif }
}

/// Short display name for a sample path.
pub fn short(p: &Path) -> String {
    let s: Vec<char> = p.to_string_lossy().chars().collect();
    if s.len() > 90 {
        format!("...{}", s[s.len() - 87..].iter().collect::<String>())
    } else {
        s.iter().collect()
    }
}

// ---------------------------------------------------------------- fuzzing

/// xorshift64* generator (deterministic).
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

/// Runs `f` on 100 evenly spaced truncations and 300 single-byte flips of `data`.
/// Returns the number of cases run and a description of every case that panicked.
pub fn fuzz_one(name: &str, data: &[u8], seed: u64, f: &(dyn Fn(&[u8]) + Sync)) -> (usize, Vec<String>) {
    let mut failures = Vec::new();
    let mut cases = 0;
    for i in 0..100 {
        let len = data.len() * i / 100;
        cases += 1;
        if catch_unwind(AssertUnwindSafe(|| f(&data[..len]))).is_err() {
            failures.push(format!("{}: truncated to {} bytes", name, len));
        }
    }
    let mut rng = Rng(seed | 1);
    let mut copy = data.to_vec();
    for _ in 0..300 {
        if copy.is_empty() {
            break;
        }
        let pos = rng.below(copy.len());
        let xor = (rng.below(255) + 1) as u8;
        copy[pos] ^= xor;
        cases += 1;
        if catch_unwind(AssertUnwindSafe(|| f(&copy))).is_err() {
            failures.push(format!("{}: byte {} xor {:02X}", name, pos, xor));
        }
        copy[pos] ^= xor;
    }
    (cases, failures)
}

/// Fuzzes many files in parallel; returns (files, cases, failures).
pub fn fuzz_files(files: &[(String, Vec<u8>)], f: &(dyn Fn(&[u8]) + Sync)) -> (usize, usize, Vec<String>) {
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results = std::sync::Mutex::new((0usize, Vec::new()));
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some((name, data)) = files.get(i) else { break };
                let (cases, fails) = fuzz_one(name, data, fnv(name.as_bytes()), f);
                let mut r = results.lock().unwrap();
                r.0 += cases;
                r.1.extend(fails);
            });
        }
    });
    let (cases, fails) = results.into_inner().unwrap();
    (files.len(), cases, fails)
}

/// Runs `f` over items on all cores and collects the results in order.
pub fn par_map<T: Sync, R: Send>(items: &[T], f: &(dyn Fn(&T) -> R + Sync)) -> Vec<R> {
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let out: std::sync::Mutex<Vec<Option<R>>> = std::sync::Mutex::new((0..items.len()).map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(item) = items.get(i) else { break };
                let r = f(item);
                out.lock().unwrap()[i] = Some(r);
            });
        }
    });
    out.into_inner().unwrap().into_iter().map(|r| r.unwrap()).collect()
}

// ---------------------------------------------------------------- checksums for hand-made files

/// CRC-32 (PNG / zlib polynomial).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// Adler-32 (zlib).
pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// Wraps data in a zlib stream made of stored blocks (no compression).
pub fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut chunks = data.chunks(65535).peekable();
    if chunks.peek().is_none() {
        out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(c) = chunks.next() {
        out.push(if chunks.peek().is_none() { 1 } else { 0 });
        let len = c.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(c);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}
