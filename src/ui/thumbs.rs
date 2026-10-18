// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Big pictures for Large icons and the preview pane: photo and video thumbnails, made on four worker threads for the
//! items on screen (newest wish first) and the screens around them. Each is read from dvfiles' own cache, else
//! decoded by dvfiles (JPEG, PNG, GIF, BMP), else asked of Windows through IShellItemImageFactory (which reads the
//! cache Explorer fills, and runs thumbnail makers in their own process); a cloud file is only shown if Windows has
//! its thumbnail cached, since making one would download the file. What is kept in memory is capped, and let go when
//! the window goes idle.
use crate::ui::{post_box, wstr};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HWND, SIZE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::WM_APP;

pub const WM_THUMB: u32 = WM_APP + 23;

/// premultiplied BGRA pixels, top row first
#[derive(Debug)]
pub struct Image {
    pub w: i32,
    pub h: i32,
    pub px: Box<[u32]>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Key {
    // a path, or `.ext` for a type's icon
    pub what: String,
    // the box it is made to fit, keeping its shape
    pub size: (i32, i32),
}

struct Req {
    key: Key,
    path: String,
    cached_only: bool,
    // the file's size and modified time, which find its thumbnail in dvfiles' own cache
    stamp: (u64, u64),
}

pub struct Reply {
    key: Key,
    image: Option<Image>,
}

struct Shared {
    queue: Mutex<VecDeque<Req>>,
    cv: Condvar,
    pack: Mutex<dvfiles::thumbcache::Cache>,
}

pub struct Thumbs {
    hwnd: HWND,
    cache: HashMap<Key, Option<Rc<Image>>>,
    // oldest first, for letting go of the least recently made when over the cap
    order: VecDeque<Key>,
    bytes: usize,
    pending: HashSet<Key>,
    shared: Option<Arc<Shared>>,
}

const CAP_BYTES: usize = 24 << 20;
const QUEUE_MAX: usize = 300;

/// the types whose content can be pictured (photos, videos, PDFs, music with cover art)
pub fn pictured(ext: &str) -> bool {
    matches!(
        ext,
        "jpg"
            | "jpeg"
            | "jpe"
            | "jfif"
            | "png"
            | "gif"
            | "bmp"
            | "dib"
            | "tif"
            | "tiff"
            | "ico"
            | "webp"
            | "heic"
            | "heif"
            | "avif"
            | "jxr"
            | "wdp"
            | "dds"
            | "tga"
            | "psd"
            | "svg"
            | "cr2"
            | "cr3"
            | "nef"
            | "arw"
            | "dng"
            | "orf"
            | "rw2"
            | "raf"
            | "srw"
            | "pef"
            | "mp4"
            | "m4v"
            | "mov"
            | "avi"
            | "wmv"
            | "mkv"
            | "webm"
            | "mpg"
            | "mpeg"
            | "3gp"
            | "ts"
            | "m2ts"
            | "flv"
            | "pdf"
            | "mp3"
            | "flac"
            | "m4a"
            | "wma"
    )
}

impl Thumbs {
    pub fn new(hwnd: HWND) -> Thumbs {
        Thumbs { hwnd, cache: HashMap::new(), order: VecDeque::new(), bytes: 0, pending: HashSet::new(), shared: None }
    }

    /// The image for a key if it is ready: Some(Some) a picture, Some(None) there is none (draw the usual icon),
    /// None not yet (it is asked for).
    pub fn get(&mut self, key: Key, path: &str, cached_only: bool, stamp: (u64, u64)) -> Option<Option<Rc<Image>>> {
        if let Some(v) = self.cache.get(&key) {
            return Some(v.clone());
        }
        self.ask(key, path, cached_only, stamp, false);
        None
    }

    /// asks for a picture just off screen, after everything on it
    pub fn ahead(&mut self, key: Key, path: &str, cached_only: bool, stamp: (u64, u64)) {
        if !self.cache.contains_key(&key) {
            self.ask(key, path, cached_only, stamp, true);
        }
    }

    fn ask(&mut self, key: Key, path: &str, cached_only: bool, stamp: (u64, u64), later: bool) {
        if self.pending.insert(key.clone()) {
            let shared = self.shared.get_or_insert_with(|| start(self.hwnd)).clone();
            let mut q = shared.queue.lock().unwrap();
            let req = Req { key, path: path.to_string(), cached_only, stamp };
            // the newest wish is taken first: what is on screen goes to the back, what is ahead to the front
            if later {
                q.push_front(req);
            } else {
                q.push_back(req);
            }
            // scrolled past: the oldest wishes go first
            while q.len() > QUEUE_MAX {
                if let Some(old) = q.pop_front() {
                    self.pending.remove(&old.key);
                }
            }
            shared.cv.notify_one();
        }
    }

    pub fn apply(&mut self, r: Reply) {
        self.pending.remove(&r.key);
        if self.pending.is_empty() {
            crate::trace::mark("thumbnails all in");
        }
        let img = r.image.map(Rc::new);
        if let Some(i) = &img {
            self.bytes += i.px.len() * 4;
        }
        self.order.push_back(r.key.clone());
        self.cache.insert(r.key, img);
        while self.bytes > CAP_BYTES {
            let Some(k) = self.order.pop_front() else { break };
            if let Some(Some(i)) = self.cache.remove(&k) {
                self.bytes -= i.px.len() * 4;
            }
        }
    }

    /// lets go of everything (idle, or the theme or DPI changed)
    pub fn clear(&mut self) {
        self.cache.clear();
        self.order.clear();
        self.bytes = 0;
        if let Some(s) = &self.shared {
            s.queue.lock().unwrap().clear();
        }
        self.pending.clear();
    }

    /// a folder's contents changed: its items' pictures may have too
    pub fn forget_under(&mut self, dir: &str) {
        let dir = dir.to_lowercase();
        self.cache.retain(|k, _| !dvfiles::pathx::is_within(&k.what.to_lowercase(), &dir));
        self.order.retain(|k| !dvfiles::pathx::is_within(&k.what.to_lowercase(), &dir));
        self.bytes = self.cache.values().flatten().map(|i| i.px.len() * 4).sum();
    }
}

/// Four worker threads: decoding is the work now, and a slow video does not hold up the photos behind it. What
/// their decoding may take at once is bounded by `DECODE_BUDGET`.
fn start(hwnd: HWND) -> Arc<Shared> {
    let pack = dvfiles::thumbcache::Cache::default_dir().unwrap_or_else(|| std::env::temp_dir().join("dvfiles-thumbs"));
    let shared = Arc::new(Shared {
        queue: Mutex::new(VecDeque::new()),
        cv: Condvar::new(),
        pack: Mutex::new(dvfiles::thumbcache::Cache::new(pack)),
    });
    let h = hwnd.0 as isize;
    for _ in 0..4 {
        let s = shared.clone();
        std::thread::spawn(move || worker(HWND(h as *mut _), s));
    }
    shared
}

fn worker(hwnd: HWND, s: Arc<Shared>) {
    let mut com = false;
    // per folder: whether its pictures are on slow storage
    let mut slow: HashMap<String, bool> = HashMap::new();
    loop {
        let req = {
            let mut q = s.queue.lock().unwrap();
            loop {
                // newest first: what is on screen now
                if let Some(r) = q.pop_back() {
                    break r;
                }
                if com {
                    let (g, t) = s.cv.wait_timeout(q, std::time::Duration::from_secs(30)).unwrap();
                    q = g;
                    if t.timed_out() && q.is_empty() {
                        // half a minute without work: COM and what the shell loaded for it are let go
                        drop(q);
                        unsafe { CoUninitialize() };
                        com = false;
                        q = s.queue.lock().unwrap();
                    }
                } else {
                    q = s.cv.wait(q).unwrap();
                }
            }
        };
        // dvfiles' own cache first: a thumbnail made before, by dvfiles or by Windows, read back with no COM
        let (bw, bh) = (req.key.size.0.max(1) as u32, req.key.size.1.max(1) as u32);
        let ck = dvfiles::thumbcache::Key::new(&req.path, req.stamp.0, req.stamp.1, bw, bh);
        if let Some(img) = s.pack.lock().ok().and_then(|mut c| c.get(&ck)).and_then(from_pack) {
            post_box(hwnd, WM_THUMB, Reply { key: req.key, image: Some(img) });
            continue;
        }
        // A picture dvfiles reads itself (JPEG, PNG, GIF, BMP) is decoded by dvfiles, with no COM: 35 ms where Windows
        // took 68 ms to make one. On a local SSD that is as fast as reading Windows' cache even when it already has the
        // picture (about 160 ms against 190 ms for a window of photos, the files in memory), and asking it first loaded
        // the shell's thumbnail machinery: 170 ms more for a window of new photos, and 3 MB. On slow storage (a
        // network share, a spinning, removable or optical disk) Windows' cache is still asked first, as reading a 5 MB
        // photo there can take far longer than its thumbnail does from the cache. The rest (videos, PDFs, RAW, HEIC)
        // and a picture dvfiles cannot read are made by Windows, and a file that is only in the cloud only ever comes
        // from Windows' cache, as reading it would download it.
        let own_kind = !req.cached_only && dvfiles::img::readable(&dvfiles::naming::ext_lower(&req.path));
        let mut image = None;
        if own_kind {
            let dir = req.path.rsplit_once('\\').map_or(req.path.as_str(), |(d, _)| d);
            let on_slow = *slow.entry(dir.to_string()).or_insert_with(|| slow_storage(dir));
            if on_slow {
                com_on(&mut com);
                image = unsafe { fetch(&req.path, req.key.size, true) };
            }
            if image.is_none() {
                image = own(&req.path, req.key.size);
            }
        }
        if image.is_none() {
            com_on(&mut com);
            image = unsafe { fetch(&req.path, req.key.size, req.cached_only) };
        }
        // shown first, then kept for next time (the preview pane's big pictures are not)
        let keep = image.as_ref().filter(|_| bw.max(bh) <= 256).map(|i| Image { w: i.w, h: i.h, px: i.px.clone() });
        post_box(hwnd, WM_THUMB, Reply { key: req.key, image });
        if let Some(img) = keep {
            let (format, data) = to_pack(&img);
            if let Ok(mut c) = s.pack.lock() {
                c.put(&ck, img.w as u32, img.h as u32, format, &data);
            }
        }
    }
}

fn com_on(com: &mut bool) {
    if !*com {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        *com = true;
    }
}

/// whether a folder is on storage where reading whole pictures is slow next to Windows' thumbnail cache: a network
/// share, a removable or optical drive, a spinning disk, or a place Windows cannot say what it is on
fn slow_storage(dir: &str) -> bool {
    use dvfiles::fsx::{drive_kind, seek_penalty, volume_root, DriveKind};
    let Some(root) = volume_root(&format!("{dir}\\")) else { return true };
    match drive_kind(&root) {
        DriveKind::Fixed | DriveKind::Ram => seek_penalty(&root) == Some(true),
        _ => true,
    }
}

/// A thumbnail as dvfiles' cache keeps it: an opaque one as a JPEG (full colour, quality 90: about 3.7 KB for a
/// 120 px photo where its pixels take 43 KB), one with transparency as its pixels, which JPEG cannot keep.
fn to_pack(img: &Image) -> (u8, Vec<u8>) {
    if img.px.iter().all(|p| p >> 24 == 255) {
        use dvfiles::img::jpegenc::{encode, Sampling};
        if let Some(jpeg) = encode(&img.px, img.w as u32, img.h as u32, 90, Sampling::S444, 0) {
            return (dvfiles::thumbcache::JPEG, jpeg);
        }
    }
    let mut data = Vec::with_capacity(img.px.len() * 4);
    for p in img.px.iter() {
        data.extend_from_slice(&p.to_le_bytes());
    }
    (dvfiles::thumbcache::RAW, data)
}

/// a thumbnail read back from dvfiles' cache
fn from_pack(s: dvfiles::thumbcache::Stored) -> Option<Image> {
    match s.format {
        dvfiles::thumbcache::RAW if s.data.len() == (s.w * s.h * 4) as usize => {
            let px: Box<[u32]> = s.data.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
            Some(Image { w: s.w as i32, h: s.h as i32, px })
        }
        dvfiles::thumbcache::JPEG => {
            let d = dvfiles::img::jpeg::decode(&s.data, u32::MAX)?;
            (d.w == s.w && d.h == s.h).then(|| Image { w: d.w as i32, h: d.h as i32, px: d.px.into_boxed_slice() })
        }
        _ => None,
    }
}

/// A picture dvfiles decodes itself (see dvfiles::img), shrunk to fit the box. Very large ones (over 40 megapixels,
/// or files over 64 MB) are left to Windows' cached thumbnails rather than decoded whole.
fn own(path: &str, size: (i32, i32)) -> Option<Image> {
    use std::io::Read;
    if !dvfiles::img::readable(&dvfiles::naming::ext_lower(path)) {
        return None;
    }
    let (bw, bh) = (size.0.max(1) as u32, size.1.max(1) as u32);
    // the start first: a camera's own small picture may be all that is needed, else what decoding will cost
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len > 64 << 20 {
        return None;
    }
    const HEAD: u64 = 256 << 10;
    let mut data = Vec::with_capacity(len.min(HEAD) as usize + 1);
    (&mut file).take(HEAD).read_to_end(&mut data).ok()?;
    let d = match dvfiles::img::stored_thumbnail(&data, bw, bh) {
        Some(d) => d,
        None => {
            let cost = dvfiles::img::decode_cost(&data, len)?;
            // decodes at once stay within a budget; a picture that alone would exceed it is left to Windows
            let _held = Budget::take(cost)?;
            // the rest read into room made once for the whole file
            data.reserve_exact((len as usize + 1).saturating_sub(data.len()));
            file.read_to_end(&mut data).ok()?;
            dvfiles::img::thumbnail(&data, bw, bh)?
        }
    };
    Some(Image { w: d.w as i32, h: d.h as i32, px: d.px.into_boxed_slice() })
}

/// Memory the four workers may use at once for decoding (a PNG is decoded whole before it is shrunk: a 4K screenshot
/// takes about 100 MB on the way): a worker waits until what it needs is free.
const DECODE_BUDGET: u64 = 128 << 20;

struct Budget(u64);

static IN_USE: Mutex<u64> = Mutex::new(0);
static FREED: Condvar = Condvar::new();

impl Budget {
    fn take(cost: u64) -> Option<Budget> {
        if cost > DECODE_BUDGET {
            return None;
        }
        let mut used = IN_USE.lock().ok()?;
        while *used + cost > DECODE_BUDGET {
            used = FREED.wait(used).ok()?;
        }
        *used += cost;
        Some(Budget(cost))
    }
}

impl Drop for Budget {
    fn drop(&mut self) {
        if let Ok(mut used) = IN_USE.lock() {
            *used = used.saturating_sub(self.0);
        }
        FREED.notify_all();
    }
}

unsafe fn fetch(path: &str, size: (i32, i32), cached_only: bool) -> Option<Image> {
    let w = wstr(path);
    let factory: IShellItemImageFactory = SHCreateItemFromParsingName(PCWSTR(w.as_ptr()), None).ok()?;
    let mut flags = SIIGBF_THUMBNAILONLY;
    if cached_only {
        flags |= SIIGBF_INCACHEONLY;
    }
    let hbm = factory.GetImage(SIZE { cx: size.0, cy: size.1 }, flags).ok()?;
    let img = pixels(hbm);
    let _ = DeleteObject(hbm.into());
    img
}

/// the bitmap's pixels, premultiplied: the shell's bitmaps either have no alpha (all zero: opaque) or premultiplied
/// alpha; one that is not (a colour above its alpha) is premultiplied here
unsafe fn pixels(hbm: HBITMAP) -> Option<Image> {
    let mut bm = BITMAP::default();
    if GetObjectW(hbm.into(), std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _)) == 0 {
        return None;
    }
    let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
    if w <= 0 || h <= 0 || w > 4096 || h > 4096 {
        return None;
    }
    let mut bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut px = vec![0u32; (w * h) as usize];
    let dc = CreateCompatibleDC(None);
    let got = GetDIBits(dc, hbm, 0, h as u32, Some(px.as_mut_ptr() as *mut _), &mut bi, DIB_RGB_COLORS);
    let _ = DeleteDC(dc);
    if got == 0 {
        return None;
    }
    let any_alpha = px.iter().any(|p| p >> 24 != 0);
    if !any_alpha {
        for p in &mut px {
            *p |= 0xFF00_0000;
        }
    } else if px.iter().any(|&p| {
        let a = p >> 24;
        (p >> 16 & 0xFF) > a || (p >> 8 & 0xFF) > a || (p & 0xFF) > a
    }) {
        for p in &mut px {
            let a = *p >> 24;
            let m = |c: u32| (c * a + 127) / 255;
            *p = a << 24 | m(*p >> 16 & 0xFF) << 16 | m(*p >> 8 & 0xFF) << 8 | m(*p & 0xFF);
        }
    }
    Some(Image { w, h, px: px.into_boxed_slice() })
}
