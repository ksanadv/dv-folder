// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! dvfiles' own thumbnail cache. Every thumbnail made, by dvfiles' decoders or by Windows, is kept in a pack file
//! under `%LOCALAPPDATA%\dvfiles\thumbs` and read back the next time in a fraction of a millisecond, with no COM,
//! where asking Windows' cache took about 3.5 ms. It holds small pictures of the user's files, as Windows' own
//! thumbnail cache does, and goes with the rest of dvfiles' data on uninstall.
//!
//! A thumbnail is found by its file's path, size and modified time (all three come with the listing, so a lookup never
//! touches the file) and the box it was made for; a changed or moved file simply misses. Records are appended to the
//! newest pack under a file lock, as each dvfiles window is its own process, and the pack is kept open between them
//! (opening it again, and listing the folder, for every thumbnail took as long as making the thumbnail). A lookup lists
//! the folder only once the newest pack is past full. Each record ends with a checksum, so one
//! cut off by a crash ends the pack there. When the newest pack passes `PACK_LIMIT` a new one is started and all but
//! the two newest are deleted, so the cache never grows past about twice that.
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::os::windows::fs::FileExt;
use std::path::PathBuf;

/// a pack is closed and a new one started past this size
pub const PACK_LIMIT: u64 = 32 << 20;

/// how a thumbnail is stored
pub const RAW: u8 = 0;
pub const JPEG: u8 = 1;

const MAGIC: [u8; 4] = *b"DVT1";
// magic 4, path hash 8, its check 8, size 8, mtime 8, box w 2, box h 2, w 2, h 2, payload length 4, format 1, spare 3
const HEADER: usize = 52;

/// which thumbnail: of which file, as it was, for which box
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Key {
    path: u64,
    // a second hash of the path, so two paths with the same first hash cannot show each other's picture
    check: u64,
    size: u64,
    mtime: u64,
    bw: u16,
    bh: u16,
}

impl Key {
    pub fn new(path: &str, size: u64, mtime: u64, bw: u32, bh: u32) -> Key {
        let lower = path.to_lowercase();
        let b = lower.as_bytes();
        let clamp = |v: u32| v.min(u16::MAX as u32) as u16;
        Key {
            path: fnv(b, 0xcbf2_9ce4_8422_2325),
            check: fnv(b, 0x8422_2325_cbf2_9ce4),
            size,
            mtime,
            bw: clamp(bw),
            bh: clamp(bh),
        }
    }
}

fn fnv(b: &[u8], seed: u64) -> u64 {
    b.iter().fold(seed, |h, &c| (h ^ c as u64).wrapping_mul(0x0000_0100_0000_01b3))
}

fn crc32(parts: &[&[u8]]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let t = TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            *e = c;
        }
        t
    });
    let mut c = !0u32;
    for p in parts {
        for &b in *p {
            c = t[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
        }
    }
    !c
}

/// a thumbnail as stored: its size, its form (RAW premultiplied BGRA, or JPEG) and its bytes
#[derive(Debug, PartialEq)]
pub struct Stored {
    pub w: u32,
    pub h: u32,
    pub format: u8,
    pub data: Vec<u8>,
}

struct Where {
    pack: u64,
    offset: u64,
    len: u32,
}

struct Pack {
    n: u64,
    file: File,
    // how far its records are in the index
    scanned: u64,
}

/// the cache: the two newest packs, indexed when first used, and the newest open for appending
pub struct Cache {
    dir: PathBuf,
    packs: Vec<Pack>,
    index: HashMap<Key, Where>,
    ready: bool,
    writer: Option<(u64, File)>,
}

fn header(key: &Key, w: u32, h: u32, len: u32, format: u8) -> [u8; HEADER] {
    let mut b = [0u8; HEADER];
    b[0..4].copy_from_slice(&MAGIC);
    b[4..12].copy_from_slice(&key.path.to_le_bytes());
    b[12..20].copy_from_slice(&key.check.to_le_bytes());
    b[20..28].copy_from_slice(&key.size.to_le_bytes());
    b[28..36].copy_from_slice(&key.mtime.to_le_bytes());
    b[36..38].copy_from_slice(&key.bw.to_le_bytes());
    b[38..40].copy_from_slice(&key.bh.to_le_bytes());
    b[40..42].copy_from_slice(&(w as u16).to_le_bytes());
    b[42..44].copy_from_slice(&(h as u16).to_le_bytes());
    b[44..48].copy_from_slice(&len.to_le_bytes());
    b[48] = format;
    b
}

/// a header's key, size, payload length and format
fn parse(b: &[u8; HEADER]) -> Option<(Key, u32, u32, u32, u8)> {
    if b[0..4] != MAGIC {
        return None;
    }
    let u64_at =
        |o: usize| u64::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3], b[o + 4], b[o + 5], b[o + 6], b[o + 7]]);
    let u16_at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
    let key =
        Key { path: u64_at(4), check: u64_at(12), size: u64_at(20), mtime: u64_at(28), bw: u16_at(36), bh: u16_at(38) };
    let len = u32::from_le_bytes([b[44], b[45], b[46], b[47]]);
    Some((key, u16_at(40) as u32, u16_at(42) as u32, len, b[48]))
}

impl Cache {
    pub fn new(dir: PathBuf) -> Cache {
        Cache { dir, packs: Vec::new(), index: HashMap::new(), ready: false, writer: None }
    }

    /// `%LOCALAPPDATA%\dvfiles\thumbs`
    pub fn default_dir() -> Option<PathBuf> {
        std::env::var_os("LOCALAPPDATA").map(|l| PathBuf::from(l).join("dvfiles").join("thumbs"))
    }

    fn pack_path(&self, n: u64) -> PathBuf {
        self.dir.join(format!("thumbs-{n:08}.pack"))
    }

    fn open_pack(&self, n: u64) -> std::io::Result<File> {
        OpenOptions::new().create(true).read(true).append(true).open(self.pack_path(n))
    }

    /// the packs on disk, oldest first
    fn numbers(&self) -> Vec<u64> {
        let mut v: Vec<u64> = std::fs::read_dir(&self.dir)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| {
                        e.file_name().to_string_lossy().strip_prefix("thumbs-")?.strip_suffix(".pack")?.parse().ok()
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort_unstable();
        v
    }

    /// opens the two newest packs and indexes their records
    fn refresh(&mut self) {
        let nums = self.numbers();
        let keep: Vec<u64> = nums.iter().skip(nums.len().saturating_sub(2)).copied().collect();
        // a pack another window rolled away leaves the index
        self.packs.retain(|p| keep.contains(&p.n));
        let open: Vec<u64> = self.packs.iter().map(|p| p.n).collect();
        self.index.retain(|_, w| open.contains(&w.pack));
        for n in keep {
            if !self.packs.iter().any(|p| p.n == n) {
                if let Ok(file) = OpenOptions::new().read(true).open(self.pack_path(n)) {
                    self.packs.push(Pack { n, file, scanned: 0 });
                }
            }
        }
        self.packs.sort_by_key(|p| p.n);
        for i in 0..self.packs.len() {
            self.scan(i);
        }
        self.ready = true;
    }

    /// indexes a pack's records from where it last stopped, up to the first that is not whole
    fn scan(&mut self, i: usize) {
        let pack = &mut self.packs[i];
        let len = pack.file.metadata().map(|m| m.len()).unwrap_or(0);
        let mut at = pack.scanned;
        let mut h = [0u8; HEADER];
        while at + HEADER as u64 <= len {
            if pack.file.seek_read(&mut h, at).ok() != Some(HEADER) {
                break;
            }
            let Some((key, _, _, plen, _)) = parse(&h) else { break };
            let end = at + HEADER as u64 + plen as u64 + 4;
            if plen > 16 << 20 || end > len {
                break;
            }
            self.index.insert(key, Where { pack: pack.n, offset: at, len: plen });
            at = end;
        }
        pack.scanned = at;
    }

    /// the thumbnail stored for `key`, if there is one and it is whole
    pub fn get(&mut self, key: &Key) -> Option<Stored> {
        if !self.ready {
            self.refresh();
        }
        if !self.index.contains_key(key) {
            // stored since, by this window or another: read on in the newest pack; once that is past full, another
            // window may have started the next one (the folder is listed only then, not for every new picture)
            match self.packs.len().checked_sub(1) {
                Some(last) => {
                    self.scan(last);
                    let p = &self.packs[last];
                    if p.scanned > PACK_LIMIT && self.numbers().last() != Some(&p.n) {
                        self.refresh();
                    }
                }
                None => self.refresh(),
            }
        }
        let w = self.index.get(key)?;
        let pack = self.packs.iter().find(|p| p.n == w.pack)?;
        let mut buf = vec![0u8; HEADER + w.len as usize + 4];
        if pack.file.seek_read(&mut buf, w.offset).ok()? != buf.len() {
            return None;
        }
        let (head, rest) = buf.split_at(HEADER);
        let (data, sum) = rest.split_at(w.len as usize);
        if crc32(&[head, data]).to_le_bytes() != sum[..4] {
            return None;
        }
        let (k, w, h, _, format) = parse(head.try_into().ok()?)?;
        (k == *key).then(|| Stored { w, h, format, data: data.to_vec() })
    }

    /// keeps a thumbnail; a failure only means it is made again next time
    pub fn put(&mut self, key: &Key, w: u32, h: u32, format: u8, data: &[u8]) {
        let _ = self.try_put(key, w, h, format, data);
    }

    fn try_put(&mut self, key: &Key, w: u32, h: u32, format: u8, data: &[u8]) -> std::io::Result<()> {
        if w == 0 || h == 0 || w > u16::MAX as u32 || h > u16::MAX as u32 || data.len() > 16 << 20 {
            return Ok(());
        }
        let head = header(key, w, h, data.len() as u32, format);
        let mut rec = Vec::with_capacity(HEADER + data.len() + 4);
        rec.extend_from_slice(&head);
        rec.extend_from_slice(data);
        rec.extend_from_slice(&crc32(&[&head, data]).to_le_bytes());
        let (mut n, mut file) = match self.writer.take() {
            Some(w) => w,
            None => {
                std::fs::create_dir_all(&self.dir)?;
                let n = self.numbers().last().copied().unwrap_or(1);
                (n, self.open_pack(n)?)
            }
        };
        let mut locked = Lock::new(&file)?;
        let mut rolled = false;
        // full, by this window or another: the next pack, or the newest if another window has started it since
        while file.seek(SeekFrom::End(0))? > PACK_LIMIT {
            drop(locked);
            n = self.numbers().last().copied().unwrap_or(n).max(n + 1);
            file = self.open_pack(n)?;
            locked = Lock::new(&file)?;
            rolled = true;
        }
        if rolled {
            // only the two newest are kept
            for old in self.numbers().into_iter().filter(|&m| m + 1 < n) {
                let _ = std::fs::remove_file(self.pack_path(old));
            }
            self.ready = false;
        }
        let r = file.write_all(&rec);
        drop(locked);
        self.writer = Some((n, file));
        r
    }
}

/// an exclusive lock on a pack's first byte while a record is appended (nothing is written without it)
struct Lock(windows::Win32::Foundation::HANDLE);

impl Lock {
    fn new(f: &File) -> std::io::Result<Lock> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Storage::FileSystem::{LockFileEx, LOCKFILE_EXCLUSIVE_LOCK};
        use windows::Win32::System::IO::OVERLAPPED;
        let h = windows::Win32::Foundation::HANDLE(f.as_raw_handle());
        let mut ov = OVERLAPPED::default();
        unsafe { LockFileEx(h, LOCKFILE_EXCLUSIVE_LOCK, None, 1, 0, &mut ov) }
            .map_err(|e| std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF))?;
        Ok(Lock(h))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        use windows::Win32::Storage::FileSystem::UnlockFileEx;
        use windows::Win32::System::IO::OVERLAPPED;
        let mut ov = OVERLAPPED::default();
        unsafe {
            let _ = UnlockFileEx(self.0, None, 1, 0, &mut ov);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dvfiles_thumbcache_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn stores_and_finds() {
        let d = dir("basic");
        let mut c = Cache::new(d.clone());
        let k = Key::new(r"C:\Photos\a.jpg", 1000, 42, 120, 120);
        assert!(c.get(&k).is_none());
        c.put(&k, 3, 2, RAW, &[1, 2, 3, 4, 5, 6, 7, 8]);
        // another window (a new cache) finds it, and so does this one
        let mut other = Cache::new(d.clone());
        assert_eq!(other.get(&k), Some(Stored { w: 3, h: 2, format: RAW, data: vec![1, 2, 3, 4, 5, 6, 7, 8] }));
        assert!(c.get(&k).is_some());
        // a changed file, another box, another path: not found
        assert!(c.get(&Key::new(r"C:\Photos\a.jpg", 1001, 42, 120, 120)).is_none());
        assert!(c.get(&Key::new(r"C:\Photos\a.jpg", 1000, 43, 120, 120)).is_none());
        assert!(c.get(&Key::new(r"C:\Photos\a.jpg", 1000, 42, 256, 256)).is_none());
        assert!(c.get(&Key::new(r"C:\Photos\b.jpg", 1000, 42, 120, 120)).is_none());
        // the path's case does not matter, as on NTFS
        assert!(c.get(&Key::new(r"c:\photos\A.JPG", 1000, 42, 120, 120)).is_some());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_cut_off_record_ends_the_pack() {
        let d = dir("cut");
        let mut c = Cache::new(d.clone());
        let (a, b) = (Key::new("a", 1, 1, 9, 9), Key::new("b", 1, 1, 9, 9));
        c.put(&a, 1, 1, RAW, &[9; 4]);
        c.put(&b, 1, 1, RAW, &[7; 4]);
        // the last record loses its end, as a crash in the middle of writing would leave it
        let pack = std::fs::read_dir(&d).unwrap().flatten().next().unwrap().path();
        let len = std::fs::metadata(&pack).unwrap().len();
        OpenOptions::new().write(true).open(&pack).unwrap().set_len(len - 3).unwrap();
        let mut fresh = Cache::new(d.clone());
        assert!(fresh.get(&a).is_some());
        assert!(fresh.get(&b).is_none());
        // a flipped byte inside a record is caught by its checksum
        let mut bytes = std::fs::read(&pack).unwrap();
        bytes[HEADER + 1] ^= 0xFF;
        std::fs::write(&pack, &bytes).unwrap();
        assert!(Cache::new(d.clone()).get(&a).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn full_packs_roll_over() {
        let d = dir("roll");
        let mut c = Cache::new(d.clone());
        // records of 1 MB: past the limit a new pack starts, and only two are ever kept
        let big = vec![5u8; 1 << 20];
        let mut keys = Vec::new();
        for i in 0..(3 * (PACK_LIMIT >> 20) + 5) {
            let k = Key::new(&format!("p{i}"), i, i, 256, 256);
            c.put(&k, 512, 512, RAW, &big);
            keys.push(k);
        }
        let packs = std::fs::read_dir(&d).unwrap().count();
        assert_eq!(packs, 2, "packs kept");
        let mut fresh = Cache::new(d.clone());
        assert!(fresh.get(keys.last().unwrap()).is_some(), "the newest is kept");
        assert!(fresh.get(&keys[0]).is_none(), "the oldest went with its pack");
        let total: u64 = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.metadata().unwrap().len()).sum();
        assert!(total <= 2 * PACK_LIMIT + (4 << 20), "{total}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_window_follows_the_pack_another_started() {
        let d = dir("follow");
        let mut a = Cache::new(d.clone());
        let mut b = Cache::new(d.clone());
        a.put(&Key::new("a0", 0, 0, 96, 96), 1, 1, RAW, &[1; 4]);
        // the other window fills the first pack and starts the next while this one keeps the first open
        let big = vec![5u8; 1 << 20];
        for i in 0..(PACK_LIMIT >> 20) + 2 {
            b.put(&Key::new(&format!("b{i}"), i, i, 256, 256), 512, 512, RAW, &big);
        }
        let k = Key::new("a1", 1, 1, 96, 96);
        a.put(&k, 1, 1, RAW, &[2; 4]);
        let mut names: Vec<String> =
            std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["thumbs-00000001.pack", "thumbs-00000002.pack"]);
        assert!(b.get(&k).is_some(), "written into the pack the other window started");
        assert!(Cache::new(d.clone()).get(&k).is_some());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_lock_is_taken() {
        let d = dir("lock");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("thumbs-00000001.pack");
        // opened as a record is written, the lock holds, and a second one waits for it (tried without waiting)
        let a = OpenOptions::new().create(true).read(true).append(true).open(&p).unwrap();
        let held = Lock::new(&a).expect("the lock is taken");
        let b = OpenOptions::new().read(true).append(true).open(&p).unwrap();
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Storage::FileSystem::{LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY};
        use windows::Win32::System::IO::OVERLAPPED;
        let mut ov = OVERLAPPED::default();
        let second = unsafe {
            LockFileEx(
                windows::Win32::Foundation::HANDLE(b.as_raw_handle()),
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                None,
                1,
                0,
                &mut ov,
            )
        };
        assert!(second.is_err(), "a second writer must not get in while the first holds the lock");
        drop(held);
        assert!(Lock::new(&b).is_ok());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn two_writers_at_once() {
        let d = dir("two");
        let threads: Vec<_> = (0..4)
            .map(|t| {
                let d = d.clone();
                std::thread::spawn(move || {
                    // each thread its own cache, as each window is its own process
                    let mut c = Cache::new(d);
                    for i in 0..200u64 {
                        c.put(&Key::new(&format!("t{t}-{i}"), i, t, 96, 96), 4, 4, RAW, &vec![(t * 7 + i) as u8; 64]);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let mut c = Cache::new(d.clone());
        for t in 0..4u64 {
            for i in 0..200u64 {
                let s = c.get(&Key::new(&format!("t{t}-{i}"), i, t, 96, 96)).expect("every record whole");
                assert_eq!(s.data, vec![(t * 7 + i) as u8; 64]);
            }
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
