// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! A search index without administrator rights.
//!
//! The first search on a drive walks it once and keeps what it saw: every folder with its file id, and every item's
//! name. The index is saved in the user's own profile, so it only ever holds what that user could list anyway.
//!
//! Before each later search the index catches up with NTFS's change journal. Any user may read the journal (Windows
//! leaves the names out for non-administrators, but keeps each change's file id and its folder's id), so the index
//! learns which folders changed since it last looked and lists just those again. A search is then a scan over names
//! in memory instead of opening hundreds of thousands of folders. When the journal has moved past the last look (or
//! was recreated) the index is built again.
//!
//! Names are kept compact: UTF-8, and each name only as far as it differs from the one before it (a folder lists in
//! sorted order, so neighbours share long beginnings). Sizes and dates are not kept: the folders holding matches are
//! read again for those, which also leaves out anything deleted since. The saved index is mapped rather than read,
//! so its names stay file pages instead of the process's private memory.
use crate::fsx::{DirReader, Entry, FA_DIRECTORY};
use crate::query::Query;
use crate::usn::{Volume, ID_MASK, ROOT_ID};
use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

const NONE: u32 = u32::MAX;
// item flags
const F_DIR: u8 = 1;
// a folder that is walked into (not a link); its 6-byte file id follows the flags
const F_SUB: u8 = 2;

/// One folder. Its record (its own name, then its items: see `Enc`) is in the mapped index file, or in `fresh` once
/// it was read again or found new.
#[derive(Clone, Copy)]
struct Dir {
    // file id, sequence bits dropped
    frn: u64,
    parent: u32,
    off: u32,
    len: u32,
    flags: u32,
}

// the record is in `fresh`, not the file
const D_FRESH: u32 = 1;
// the folder is gone
const D_DEAD: u32 = 2;

fn put_len(out: &mut Vec<u8>, mut n: usize) {
    while n >= 0x80 {
        out.push((n as u8 & 0x7F) | 0x80);
        n >>= 7;
    }
    out.push(n as u8);
}

fn get_len(b: &[u8], at: &mut usize) -> usize {
    let (mut n, mut shift) = (0usize, 0);
    while let Some(&x) = b.get(*at) {
        *at += 1;
        n |= ((x & 0x7F) as usize) << shift;
        if x & 0x80 == 0 || shift > 28 {
            break;
        }
        shift += 7;
    }
    n
}

/// a folder's record: its name (length, UTF-8), then per item: flags, [file id], bytes shared with the previous
/// name, the rest's length, the rest
struct Enc {
    data: Vec<u8>,
    prev: Vec<u8>,
    name: Vec<u8>,
}

impl Enc {
    fn new(own: &[u8]) -> Enc {
        let mut e = Enc { data: Vec::with_capacity(256), prev: Vec::new(), name: Vec::new() };
        put_len(&mut e.data, own.len());
        e.data.extend_from_slice(own);
        e
    }

    fn push(&mut self, name: &[u16], flags: u8, frn: u64) {
        self.name.clear();
        for c in char::decode_utf16(name.iter().copied()) {
            let c = c.unwrap_or('\u{FFFD}');
            let mut b = [0u8; 4];
            self.name.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
        }
        let shared = self.prev.iter().zip(&self.name).take_while(|(a, b)| a == b).count().min(255);
        self.data.push(flags);
        if flags & F_SUB != 0 {
            self.data.extend_from_slice(&frn.to_le_bytes()[..6]);
        }
        self.data.push(shared as u8);
        put_len(&mut self.data, self.name.len() - shared);
        self.data.extend_from_slice(&self.name[shared..]);
        std::mem::swap(&mut self.prev, &mut self.name);
    }
}

/// a record holding only a folder's own name (until the folder is read)
fn name_only(own: &[u8]) -> Vec<u8> {
    Enc::new(own).data
}

/// steps through a folder's items, rebuilding each name
struct Items<'a> {
    b: &'a [u8],
    at: usize,
    name: Vec<u8>,
}

impl<'a> Items<'a> {
    fn new(data: &'a [u8]) -> Items<'a> {
        let mut at = 0;
        let n = get_len(data, &mut at);
        Items { b: data, at: (at + n).min(data.len()), name: Vec::with_capacity(64) }
    }

    /// flags, file id (folders walked into, else 0), name (UTF-8)
    fn next(&mut self) -> Option<(u8, u64, &[u8])> {
        let b = self.b;
        let flags = *b.get(self.at)?;
        self.at += 1;
        let mut frn = 0u64;
        if flags & F_SUB != 0 {
            let mut id = [0u8; 8];
            id[..6].copy_from_slice(b.get(self.at..self.at + 6)?);
            frn = u64::from_le_bytes(id);
            self.at += 6;
        }
        let shared = *b.get(self.at)? as usize;
        self.at += 1;
        let n = get_len(b, &mut self.at);
        let rest = b.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        self.name.truncate(shared.min(self.name.len()));
        self.name.extend_from_slice(rest);
        Some((flags, frn, &self.name))
    }
}

fn own_name(data: &[u8]) -> &[u8] {
    let mut at = 0;
    let n = get_len(data, &mut at);
    data.get(at..at + n).unwrap_or(&[])
}

pub struct Index {
    /// the folder it covers: a drive (`C:\`) or any folder on it
    pub root: String,
    journal: u64,
    usn: i64,
    dirs: Vec<Dir>,
    // the saved file, and where its file-id table (12 bytes per folder, by id) and its records start
    file: Option<crate::fsx::Mapped>,
    table_at: usize,
    table_n: usize,
    blob_at: usize,
    // records written since loading
    fresh: Vec<u8>,
    // file ids of folders found since loading (the saved table knows the rest)
    extra: HashMap<u64, u32>,
    // where the saved copy stands in the journal, and how many folders were read since it was written
    saved_usn: i64,
    since_save: usize,
    /// worth writing back: new, or much has changed since it was saved
    pub dirty: bool,
}

/// one folder as read: its record, and the folders inside to walk into (file id, name)
fn read_folder(reader: &mut DirReader, path: &str, own: &[u8]) -> io::Result<(Vec<u8>, Vec<(u64, Box<[u16]>)>)> {
    let mut enc = Enc::new(own);
    let mut subs = Vec::new();
    reader.read(&crate::wide::long(path), |e| {
        let mut flags = if e.attrs & FA_DIRECTORY != 0 { F_DIR } else { 0 };
        if e.is_plain_dir() {
            flags |= F_SUB;
            subs.push((e.id & ID_MASK, e.name.into()));
        }
        enc.push(e.name, flags, e.id & ID_MASK);
    })?;
    Ok((enc.data, subs))
}

/// what a search found: the folder and the name
pub struct Hit {
    pub dir: Arc<str>,
    pub name: String,
}

fn has(hay: &[u8], needle: &[u8]) -> bool {
    let Some((&first, rest)) = needle.split_first() else { return true };
    if needle.len() > hay.len() {
        return false;
    }
    (0..=hay.len() - needle.len()).any(|i| hay[i] == first && &hay[i + 1..i + needle.len()] == rest)
}

impl Index {
    fn empty(root: String, journal: u64, usn: i64, top: u64) -> Index {
        let mut ix = Index {
            root,
            journal,
            usn,
            dirs: Vec::new(),
            file: None,
            table_at: 0,
            table_n: 0,
            blob_at: 0,
            fresh: Vec::new(),
            extra: HashMap::new(),
            saved_usn: usn,
            since_save: 0,
            dirty: true,
        };
        ix.fresh = name_only(b"");
        ix.dirs.push(Dir { frn: top, parent: NONE, off: 0, len: ix.fresh.len() as u32, flags: D_FRESH });
        ix.extra.insert(top, 0);
        ix
    }

    /// Walks a drive (or one folder and everything in it). Fails when the drive has no change journal (FAT, exFAT,
    /// network drives), since such an index could not be kept current.
    pub fn build(root: &str, stop: &AtomicBool) -> io::Result<Index> {
        let root = if root.trim_end_matches('\\').len() <= 2 {
            format!("{}\\", root.trim_end_matches('\\'))
        } else {
            root.trim_end_matches('\\').to_string()
        };
        let j =
            Volume::open(&root[..2])?.journal().ok_or_else(|| io::Error::other("the drive keeps no change journal"))?;
        let top = if root.len() == 3 { ROOT_ID } else { crate::fsx::file_id(&root)? & ID_MASK };
        // the journal position is taken first: whatever changes during the walk is looked at again next time
        let mut ix = Index::empty(root, j.id, j.next, top);
        ix.walk_from(vec![0], stop);
        if stop.load(Ordering::Relaxed) {
            return Err(io::Error::other("stopped"));
        }
        Ok(ix)
    }

    fn data(&self, d: &Dir) -> &[u8] {
        let (off, len) = (d.off as usize, d.len as usize);
        if d.flags & D_FRESH != 0 {
            self.fresh.get(off..off + len).unwrap_or(&[])
        } else {
            self.file.as_ref().and_then(|m| m.bytes().get(self.blob_at + off..self.blob_at + off + len)).unwrap_or(&[])
        }
    }

    fn data_of(&self, d: u32) -> &[u8] {
        self.data(&self.dirs[d as usize])
    }

    fn set_data(&mut self, d: u32, bytes: &[u8]) {
        let off = self.fresh.len();
        self.fresh.extend_from_slice(bytes);
        let dir = &mut self.dirs[d as usize];
        dir.off = off as u32;
        dir.len = bytes.len() as u32;
        dir.flags |= D_FRESH;
    }

    fn alive(&self, d: u32) -> bool {
        self.dirs.get(d as usize).is_some_and(|x| x.flags & D_DEAD == 0)
    }

    /// the folder with a file id: found since loading, or in the saved table
    fn by_frn(&self, frn: u64) -> Option<u32> {
        let ok = |k: u32| self.alive(k) && self.dirs[k as usize].frn == frn;
        if let Some(&k) = self.extra.get(&frn) {
            if ok(k) {
                return Some(k);
            }
        }
        let b = self.file.as_ref()?.bytes();
        let entry = |i: usize| {
            let at = self.table_at + i * 12;
            (
                u64::from_le_bytes(b[at..at + 8].try_into().unwrap()),
                u32::from_le_bytes(b[at + 8..at + 12].try_into().unwrap()),
            )
        };
        let (mut lo, mut hi) = (0usize, self.table_n);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let (f, k) = entry(mid);
            match f.cmp(&frn) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return ok(k).then_some(k),
            }
        }
        None
    }

    /// reads the given folders and everything below them, in parallel
    fn walk_from(&mut self, start: Vec<u32>, stop: &AtomicBool) {
        if start.is_empty() {
            return;
        }
        let first: Vec<(u32, Arc<str>, Vec<u8>)> = start
            .iter()
            .filter_map(|&d| self.path(d).map(|p| (d, Arc::from(p.as_str()), own_name(self.data_of(d)).to_vec())))
            .collect();
        let before = self.dirs.len();
        let pending = AtomicUsize::new(first.len());
        let queue = Mutex::new(first);
        let cv = Condvar::new();
        let state = Mutex::new((std::mem::take(&mut self.dirs), std::mem::take(&mut self.fresh)));
        std::thread::scope(|s| {
            for _ in 0..crate::walker::default_threads() {
                s.spawn(|| {
                    let mut reader = DirReader::with_ids();
                    loop {
                        let (d, path, own) = {
                            let mut q = queue.lock().unwrap();
                            loop {
                                if stop.load(Ordering::Relaxed) {
                                    cv.notify_all();
                                    return;
                                }
                                if let Some(x) = q.pop() {
                                    break x;
                                }
                                if pending.load(Ordering::Acquire) == 0 {
                                    cv.notify_all();
                                    return;
                                }
                                q = cv.wait(q).unwrap();
                            }
                        };
                        let mut found = Vec::new();
                        // a folder that cannot be read (no access) keeps only its name, as in the user's own walk
                        if let Ok((data, subs)) = read_folder(&mut reader, &path, &own) {
                            let names: Vec<(String, Arc<str>)> = subs
                                .iter()
                                .map(|(_, n)| {
                                    let n = String::from_utf16_lossy(n);
                                    let p = Arc::from(crate::pathx::join(&path, &n).as_str());
                                    (n, p)
                                })
                                .collect();
                            let mut st = state.lock().unwrap();
                            let (dirs, fresh) = &mut *st;
                            let off = fresh.len() as u32;
                            fresh.extend_from_slice(&data);
                            let dir = &mut dirs[d as usize];
                            dir.off = off;
                            dir.len = data.len() as u32;
                            dir.flags |= D_FRESH;
                            for ((frn, _), (n, p)) in subs.into_iter().zip(names) {
                                let k = dirs.len() as u32;
                                let rec = name_only(n.as_bytes());
                                let off = fresh.len() as u32;
                                fresh.extend_from_slice(&rec);
                                dirs.push(Dir { frn, parent: d, off, len: rec.len() as u32, flags: D_FRESH });
                                found.push((k, p, n.into_bytes()));
                            }
                        }
                        if !found.is_empty() {
                            pending.fetch_add(found.len(), Ordering::AcqRel);
                            queue.lock().unwrap().append(&mut found);
                            cv.notify_all();
                        }
                        if pending.fetch_sub(1, Ordering::AcqRel) == 1 {
                            let _g = queue.lock().unwrap();
                            cv.notify_all();
                        }
                    }
                });
            }
        });
        let (dirs, fresh) = state.into_inner().unwrap();
        self.dirs = dirs;
        self.fresh = fresh;
        // the folders found by this walk; one moved in from elsewhere replaces its old record
        for k in before..self.dirs.len() {
            self.extra.insert(self.dirs[k].frn, k as u32);
        }
        self.since_save += self.dirs.len() - before;
    }

    /// a folder's full path; None when it or a folder above it is gone
    pub fn path(&self, mut d: u32) -> Option<String> {
        let mut parts: Vec<&[u8]> = Vec::new();
        while d != 0 {
            if !self.alive(d) || parts.len() > 1000 {
                return None;
            }
            parts.push(own_name(self.data_of(d)));
            d = self.dirs[d as usize].parent;
        }
        if parts.is_empty() {
            return Some(self.root.clone());
        }
        let mut s = self.root.trim_end_matches('\\').to_string();
        for p in parts.iter().rev() {
            s.push('\\');
            s.push_str(&String::from_utf8_lossy(p));
        }
        Some(s)
    }

    fn depth(&self, mut d: u32) -> usize {
        let mut n = 0;
        while d != 0 && d != NONE && n < 1000 {
            d = self.dirs[d as usize].parent;
            n += 1;
        }
        n
    }

    /// the folders a record lists, by index
    fn subs_of(&self, d: u32) -> Vec<u32> {
        let mut out = Vec::new();
        let mut it = Items::new(self.data_of(d));
        while let Some((flags, frn, _)) = it.next() {
            if flags & F_SUB != 0 {
                if let Some(k) = self.by_frn(frn) {
                    if self.dirs[k as usize].parent == d && k != d {
                        out.push(k);
                    }
                }
            }
        }
        out
    }

    fn kill(&mut self, d: u32) {
        let mut stack = vec![d];
        while let Some(k) = stack.pop() {
            if !self.alive(k) {
                continue;
            }
            stack.extend(self.subs_of(k));
            let frn = self.dirs[k as usize].frn;
            if self.extra.get(&frn) == Some(&k) {
                self.extra.remove(&frn);
            }
            let dir = &mut self.dirs[k as usize];
            dir.flags = D_DEAD | D_FRESH;
            dir.len = 0;
        }
    }

    /// Catches up with the change journal: the folders that changed since the last look are read again (and new
    /// folders walked). Returns how many folders were read. Err means the journal can no longer tell what changed:
    /// build again.
    pub fn refresh(&mut self, stop: &AtomicBool) -> io::Result<usize> {
        let vol = Volume::open(&self.root[..2])?;
        let j = vol.journal().ok_or_else(|| io::Error::other("the change journal is gone"))?;
        if j.id != self.journal {
            return Err(io::Error::other("the change journal was recreated"));
        }
        let mut touched: HashSet<u64> = HashSet::new();
        let end = vol.changes(&j, self.usn, |r| {
            touched.insert(r.parent & ID_MASK);
        })?;
        let mut todo: Vec<u32> = touched.iter().filter_map(|&f| self.by_frn(f)).collect();
        // folders above first, so a renamed parent has its new name before its children are read
        todo.sort_by_key(|&d| self.depth(d));
        let mut reader = DirReader::with_ids();
        let mut fresh_dirs: Vec<u32> = Vec::new();
        let mut dropped: Vec<(u32, u32)> = Vec::new();
        // A folder may not be where the index last saw it: moved, with its parent's listing (which says where to)
        // read later in the same pass. Such a folder is tried again once its place has changed.
        let mut pending: Vec<u32> = todo.clone();
        for _round in 0..8 {
            let mut missing: Vec<(u32, String)> = Vec::new();
            for &d in &pending {
                if stop.load(Ordering::Relaxed) {
                    return Err(io::Error::other("stopped"));
                }
                if !self.alive(d) {
                    continue;
                }
                let Some(path) = self.path(d) else { continue };
                match self.reread(d, &path, &mut reader, &mut fresh_dirs, &mut dropped) {
                    Ok(()) => {}
                    Err(e) if matches!(e.raw_os_error(), Some(2 | 3 | 267)) => missing.push((d, path)),
                    Err(_) => {}
                }
            }
            let (moved, gone): (Vec<_>, Vec<_>) =
                missing.into_iter().partition(|(d, was)| self.path(*d).is_some_and(|p| p != *was));
            for (d, _) in gone {
                // gone (or no longer a folder), unless a listing still to come says where it went
                dropped.push((d, self.dirs[d as usize].parent));
            }
            pending = moved.into_iter().map(|m| m.0).collect();
            if pending.is_empty() {
                break;
            }
        }
        // folders that left a listing and did not turn up in another one are gone, with everything in them
        for (k, from) in dropped {
            if self.dirs[k as usize].parent == from {
                self.kill(k);
            }
        }
        self.walk_from(fresh_dirs, stop);
        if stop.load(Ordering::Relaxed) {
            return Err(io::Error::other("stopped"));
        }
        self.since_save += todo.len();
        self.usn = end;
        // Writing the whole index back costs more than reading a few hundred folders again at the next load, so it
        // is written only once much has changed, or before the journal could move past where the saved copy stands.
        let span = (j.next - j.first).max(1);
        if self.since_save > 5000 || end - self.saved_usn > span / 4 {
            self.dirty = true;
        }
        Ok(todo.len())
    }

    /// Reads one folder again: its record is replaced, folders inside it that the index knows are moved or renamed
    /// to match, new ones are queued for a walk, and ones no longer there are noted in `dropped`.
    fn reread(
        &mut self,
        d: u32,
        path: &str,
        reader: &mut DirReader,
        fresh_dirs: &mut Vec<u32>,
        dropped: &mut Vec<(u32, u32)>,
    ) -> io::Result<()> {
        let own = own_name(self.data_of(d)).to_vec();
        let old_subs = self.subs_of(d);
        let (data, subs) = read_folder(reader, path, &own)?;
        let mut now = Vec::with_capacity(subs.len());
        for (frn, name) in subs {
            let n = String::from_utf16_lossy(&name);
            match self.by_frn(frn) {
                Some(k) if k != d => {
                    // known: the same folder, maybe renamed or moved here; its own name is refreshed
                    let mut rec = name_only(n.as_bytes());
                    let old = self.data_of(k);
                    let items_at = {
                        let mut at = 0;
                        let l = get_len(old, &mut at);
                        (at + l).min(old.len())
                    };
                    rec.extend_from_slice(&old[items_at..]);
                    self.set_data(k, &rec);
                    self.dirs[k as usize].parent = d;
                    now.push(k);
                }
                _ => {
                    let k = self.dirs.len() as u32;
                    self.dirs.push(Dir { frn, parent: d, off: 0, len: 0, flags: D_FRESH });
                    self.set_data(k, &name_only(n.as_bytes()));
                    self.extra.insert(frn, k);
                    fresh_dirs.push(k);
                    now.push(k);
                }
            }
        }
        for old in old_subs {
            if !now.contains(&old) {
                dropped.push((old, d));
            }
        }
        self.set_data(d, &data);
        Ok(())
    }

    /// the folder index for a path inside the indexed folder
    pub fn find(&self, path: &str) -> Option<u32> {
        if !crate::pathx::is_within(path, &self.root) && !crate::pathx::same(path, &self.root) {
            return None;
        }
        let rest = path.get(self.root.trim_end_matches('\\').len()..)?;
        let mut d = 0u32;
        for part in rest.split('\\').filter(|p| !p.is_empty()) {
            let want = part.to_lowercase();
            let mut it = Items::new(self.data_of(d));
            let mut next = None;
            while let Some((flags, frn, name)) = it.next() {
                if flags & F_SUB != 0 && String::from_utf8_lossy(name).to_lowercase() == want {
                    next = self.by_frn(frn);
                    break;
                }
            }
            d = next?;
        }
        Some(d)
    }

    /// Matches names below `scope` (a folder in the index) or everywhere. Returns the matches (at most `limit`),
    /// whether there were more, and how many items and folders were looked at. Size filters are not applied here
    /// (the index keeps no sizes): `entries` applies them.
    ///
    /// Most names are plain ASCII: for those the query's words are first looked for in the bytes as they are, and
    /// only a name that has them all is decoded and matched in full.
    pub fn search(&self, q: &Query, scope: Option<&str>, limit: usize) -> (Vec<Hit>, bool, u64, u64) {
        let scope = match scope {
            Some(p) => match self.find(p) {
                Some(d) => d,
                None => return (Vec::new(), false, 0, 0),
            },
            None => 0,
        };
        let names_only = q.without_size();
        let needles = q.ascii_needles();
        // the folders to look in: all of them, or the scope's part of the tree
        let picked: Vec<u32> = if scope == 0 {
            (0..self.dirs.len() as u32).collect()
        } else {
            let mut v = vec![scope];
            let mut k = 0;
            while k < v.len() {
                let subs = self.subs_of(v[k]);
                v.extend(subs);
                k += 1;
            }
            v
        };
        let threads = crate::walker::default_threads().min(picked.len().div_ceil(2000)).max(1);
        let chunk = picked.len().div_ceil(threads).max(1);
        let found = AtomicUsize::new(0);
        let parts: Vec<(Vec<(u32, String)>, u64, u64)> = std::thread::scope(|s| {
            let handles: Vec<_> = picked
                .chunks(chunk)
                .map(|ds| {
                    let (found, q, needles) = (&found, &names_only, &needles);
                    s.spawn(move || {
                        let mut hits = Vec::new();
                        let (mut items, mut folders) = (0u64, 0u64);
                        let mut wide: Vec<u16> = Vec::with_capacity(128);
                        let mut low: Vec<u8> = Vec::with_capacity(128);
                        for &d in ds {
                            let dir = &self.dirs[d as usize];
                            if dir.flags & D_DEAD != 0 {
                                continue;
                            }
                            folders += 1;
                            let mut it = Items::new(self.data(dir));
                            while let Some((flags, _, name)) = it.next() {
                                items += 1;
                                if !needles.is_empty() && name.is_ascii() {
                                    low.clear();
                                    low.extend(name.iter().map(|b| b.to_ascii_lowercase()));
                                    if !needles.iter().all(|n| has(&low, n)) {
                                        continue;
                                    }
                                }
                                wide.clear();
                                if name.is_ascii() {
                                    wide.extend(name.iter().map(|&b| b as u16));
                                } else {
                                    wide.extend(String::from_utf8_lossy(name).encode_utf16());
                                }
                                if q.matches(&wide, flags & F_DIR != 0, 0) {
                                    if found.fetch_add(1, Ordering::Relaxed) >= limit {
                                        return (hits, items, folders);
                                    }
                                    hits.push((d, String::from_utf8_lossy(name).into_owned()));
                                }
                            }
                        }
                        (hits, items, folders)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap_or_default()).collect()
        });
        let capped = found.load(Ordering::Relaxed) > limit;
        let (mut items, mut folders) = (0, 0);
        let mut paths: HashMap<u32, Option<Arc<str>>> = HashMap::new();
        let mut out = Vec::new();
        for (hits, n, f) in parts {
            items += n;
            folders += f;
            for (d, name) in hits {
                if let Some(dir) = paths.entry(d).or_insert_with(|| self.path(d).map(|p| Arc::from(p.as_str()))).clone()
                {
                    out.push(Hit { dir, name });
                }
            }
        }
        (out, capped, items, folders)
    }

    /// folders and items held, the bytes of their records, and the bytes on the private heap
    pub fn stats(&self) -> (usize, usize, usize, usize) {
        let (mut dirs, mut items, mut bytes) = (0usize, 0usize, 0usize);
        for d in self.dirs.iter().filter(|d| d.flags & D_DEAD == 0) {
            dirs += 1;
            let data = self.data(d);
            let mut it = Items::new(data);
            while it.next().is_some() {
                items += 1;
            }
            bytes += data.len();
        }
        let heap =
            self.dirs.capacity() * std::mem::size_of::<Dir>() + self.fresh.capacity() + self.extra.capacity() * 16;
        (dirs, items, bytes, heap)
    }

    // -----------------------------------------------------------------------------------------------------------
    // saved form: header, folder table (20 bytes each), file-id table (12 bytes each, by id), records

    const MAGIC: &'static [u8; 8] = b"DVIX0003";

    /// Writes the index (live folders only) to a temporary file that then replaces the old one, and maps the new
    /// file in place of what was in memory.
    pub fn save(&mut self, file: &std::path::Path) -> io::Result<()> {
        if let Some(d) = file.parent() {
            std::fs::create_dir_all(d)?;
        }
        // live folders get new, dense numbers, parents before children
        let mut remap = vec![NONE; self.dirs.len()];
        let mut order = Vec::new();
        let mut stack = vec![0u32];
        while let Some(d) = stack.pop() {
            if remap[d as usize] != NONE {
                continue;
            }
            remap[d as usize] = order.len() as u32;
            order.push(d);
            stack.extend(self.subs_of(d));
        }
        let tmp = file.with_extension(format!("{}.tmp", std::process::id()));
        {
            let mut w = io::BufWriter::with_capacity(1 << 20, std::fs::File::create(&tmp)?);
            w.write_all(Self::MAGIC)?;
            let root = self.root.as_bytes();
            w.write_all(&(root.len() as u32).to_le_bytes())?;
            w.write_all(root)?;
            w.write_all(&self.journal.to_le_bytes())?;
            w.write_all(&self.usn.to_le_bytes())?;
            w.write_all(&(order.len() as u32).to_le_bytes())?;
            let mut off = 0u64;
            for &d in &order {
                let dir = &self.dirs[d as usize];
                let parent = if d == 0 { NONE } else { remap[dir.parent as usize] };
                let len = self.data(dir).len() as u32;
                w.write_all(&dir.frn.to_le_bytes())?;
                w.write_all(&parent.to_le_bytes())?;
                w.write_all(&(off as u32).to_le_bytes())?;
                w.write_all(&len.to_le_bytes())?;
                off += len as u64;
            }
            if off > u32::MAX as u64 {
                return Err(io::Error::other("the index is too large"));
            }
            let mut ids: Vec<(u64, u32)> =
                order.iter().enumerate().map(|(k, &d)| (self.dirs[d as usize].frn, k as u32)).collect();
            ids.sort_unstable();
            for (frn, k) in ids {
                w.write_all(&frn.to_le_bytes())?;
                w.write_all(&k.to_le_bytes())?;
            }
            for &d in &order {
                w.write_all(self.data_of(d))?;
            }
            w.flush()?;
        }
        // the old file is let go before it is replaced; if it cannot be replaced (another dvfiles window has it
        // open) the old one stays, and the change journal brings it up to date again next time
        self.file = None;
        let replaced = std::fs::rename(&tmp, file);
        if replaced.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        *self = Index::load(file)?;
        replaced
    }

    pub fn load(file: &std::path::Path) -> io::Result<Index> {
        let m = crate::fsx::Mapped::open(file)?;
        let bad = || io::Error::other("the saved index is damaged");
        let b = m.bytes();
        let mut r = Reader { b, at: 0 };
        if r.take(8).ok_or_else(bad)? != Self::MAGIC {
            return Err(bad());
        }
        let n = r.u32().ok_or_else(bad)? as usize;
        let root = String::from_utf8(r.take(n).ok_or_else(bad)?.to_vec()).map_err(|_| bad())?;
        let journal = r.u64().ok_or_else(bad)?;
        let usn = r.u64().ok_or_else(bad)? as i64;
        let count = r.u32().ok_or_else(bad)? as usize;
        if count == 0 || count > b.len() / 32 {
            return Err(bad());
        }
        let mut dirs = Vec::with_capacity(count);
        for k in 0..count {
            let frn = r.u64().ok_or_else(bad)?;
            let parent = r.u32().ok_or_else(bad)?;
            let off = r.u32().ok_or_else(bad)?;
            let len = r.u32().ok_or_else(bad)?;
            if k > 0 && parent as usize >= k {
                return Err(bad());
            }
            dirs.push(Dir { frn, parent, off, len, flags: 0 });
        }
        let table_at = r.at;
        r.take(count * 12).ok_or_else(bad)?;
        let blob_at = r.at;
        let blob_len = b.len() - blob_at;
        if dirs.iter().any(|d| d.off as usize + d.len as usize > blob_len) {
            return Err(bad());
        }
        Ok(Index {
            root,
            journal,
            usn,
            dirs,
            file: Some(m),
            table_at,
            table_n: count,
            blob_at,
            fresh: Vec::new(),
            extra: HashMap::new(),
            saved_usn: usn,
            since_save: 0,
            dirty: false,
        })
    }
}

/// Turns hits into list entries by reading each folder that holds some (on several threads), so sizes and dates
/// are current, size filters apply, and anything deleted since is left out. `each` receives batches as folders are
/// read.
pub fn entries(hits: Vec<Hit>, q: &Query, stop: &AtomicBool, each: &(dyn Fn(Vec<Entry>) + Sync)) {
    let mut by_dir: HashMap<Arc<str>, HashSet<String>> = HashMap::new();
    for h in hits {
        by_dir.entry(h.dir).or_default().insert(h.name.to_lowercase());
    }
    let work: Mutex<Vec<(Arc<str>, HashSet<String>)>> = Mutex::new(by_dir.into_iter().collect());
    std::thread::scope(|s| {
        for _ in 0..crate::walker::default_threads() {
            s.spawn(|| {
                let mut reader = DirReader::default();
                let mut batch = Vec::new();
                loop {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let Some((dir, names)) = work.lock().unwrap().pop() else { break };
                    let _ = reader.read(&crate::wide::long(&dir), |e| {
                        let name = String::from_utf16_lossy(e.name);
                        if names.contains(&name.to_lowercase())
                            && q.matches(e.name, e.is_dir(), e.size)
                            && q.fresh(e.modified)
                        {
                            batch.push(Entry::from_raw(e, Some(dir.clone())));
                        }
                    });
                    if batch.len() >= 2000 {
                        each(std::mem::take(&mut batch));
                    }
                }
                if !batch.is_empty() {
                    each(batch);
                }
            });
        }
    });
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
}

/// the exit code of a build for a drive that keeps no change journal
pub const NO_JOURNAL: i32 = 3;

/// Builds and saves a drive's index, for `dvfiles --build-index C:\` (run by `shelf::build_later`): the process runs
/// in background mode (lowest disk and memory priority) and ends when the file is written. Returns the exit code.
pub fn build_file(root: &str) -> i32 {
    use windows::Win32::System::Threading::{GetCurrentProcess, SetPriorityClass, PROCESS_MODE_BACKGROUND_BEGIN};
    unsafe {
        let _ = SetPriorityClass(GetCurrentProcess(), PROCESS_MODE_BACKGROUND_BEGIN);
    }
    let Some(file) = file_for(root) else { return 1 };
    let has_journal = Volume::open(&root[..2.min(root.len())]).ok().and_then(|v| v.journal()).is_some();
    if !has_journal {
        return NO_JOURNAL;
    }
    match Index::build(root, &AtomicBool::new(false)).and_then(|mut ix| ix.save(&file)) {
        Ok(()) => 0,
        Err(e) => {
            crate::trace_log(&format!("index {root}: {e}"));
            1
        }
    }
}

/// where a volume's index is kept: `%LOCALAPPDATA%\dvfiles\index\C.idx`
pub fn file_for(root: &str) -> Option<std::path::PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")?;
    let letter = root.chars().next()?.to_ascii_uppercase();
    Some(std::path::PathBuf::from(base).join("dvfiles").join("index").join(format!("{letter}.idx")))
}

/// The drives' indexes as this process uses them: loaded by the first search on a drive (mapping the saved file
/// takes milliseconds), kept while searches keep coming, written back and let go ten seconds after the last one,
/// so an idle window holds none of it.
pub mod shelf {
    use super::{file_for, Hit, Index};
    use crate::query::Query;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct Slot {
        ix: Option<Index>,
        // the process building the index, or the drive cannot have one (no change journal)
        builder: Option<std::process::Child>,
        unsupported: bool,
    }

    /// whether a build is still running (and what it said when it ended)
    fn building(s: &mut Slot) -> bool {
        let Some(child) = s.builder.as_mut() else { return false };
        match child.try_wait() {
            Ok(None) => true,
            Ok(Some(status)) => {
                if status.code() == Some(super::NO_JOURNAL) {
                    s.unsupported = true;
                }
                s.builder = None;
                false
            }
            Err(_) => {
                s.builder = None;
                false
            }
        }
    }

    type Slots = HashMap<char, Arc<Mutex<Slot>>>;
    static SLOTS: Mutex<Option<Slots>> = Mutex::new(None);
    static LAST_USE: Mutex<Option<Instant>> = Mutex::new(None);
    static SWEEPER: AtomicBool = AtomicBool::new(false);
    const KEEP: Duration = Duration::from_secs(10);

    fn letter(path: &str) -> Option<char> {
        let b = path.as_bytes();
        (b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic()).then(|| b[0].to_ascii_uppercase() as char)
    }

    fn slot(l: char) -> Arc<Mutex<Slot>> {
        SLOTS.lock().unwrap().get_or_insert_with(HashMap::new).entry(l).or_default().clone()
    }

    /// Searches `folder` (a drive or any folder on it) through the drive's index. None when there is no usable
    /// index: the caller then walks, and may call `build_later`.
    pub fn search(folder: &str, q: &Query, limit: usize, stop: &AtomicBool) -> Option<(Vec<Hit>, bool, u64, u64)> {
        // The index knows no sizes: a size filter is applied to the name matches afterwards, so a capped name match
        // could hide matches of the right size. Such searches walk.
        if q.has_size() {
            return None;
        }
        let l = letter(folder)?;
        let file = file_for(folder)?;
        let slot = slot(l);
        let mut s = slot.lock().unwrap();
        if building(&mut s) || s.unsupported {
            return None;
        }
        if s.ix.is_none() {
            s.ix = Index::load(&file).ok().filter(|ix| ix.root.len() == 3);
        }
        let ix = s.ix.as_mut()?;
        if let Err(e) = ix.refresh(stop) {
            if !stop.load(Ordering::Relaxed) {
                // the journal can no longer say what changed: this index is of no more use
                crate::trace_log(&format!("index {l}: {e}"));
                s.ix = None;
                let _ = std::fs::remove_file(&file);
            }
            return None;
        }
        let scope = (folder.trim_end_matches('\\').len() > 2).then_some(folder);
        // a folder the index's walk never entered (behind a junction, a drive mounted in a folder) is walked
        if scope.is_some_and(|f| ix.find(f).is_none()) {
            return None;
        }
        let r = ix.search(q, scope, limit);
        drop(s);
        touch();
        Some(r)
    }

    /// Builds a drive's index, unless one exists or is being built: in a separate, short-lived process (this
    /// program run with `--build-index`, see `build_file`), at low CPU and disk priority, so the walk's memory is
    /// never this window's and is all given back when it ends. The next search maps the finished file.
    pub fn build_later(folder: &str) {
        use std::os::windows::process::CommandExt;
        let Some(l) = letter(folder) else { return };
        let Some(file) = file_for(folder) else { return };
        let slot = slot(l);
        let mut s = slot.lock().unwrap();
        if building(&mut s) || s.unsupported || s.ix.is_some() || file.exists() {
            return;
        }
        let Ok(exe) = std::env::current_exe() else { return };
        // CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS
        let child = std::process::Command::new(exe)
            .arg("--build-index")
            .arg(format!("{l}:\\"))
            .creation_flags(0x0800_0000 | 0x4000)
            .spawn();
        match child {
            Ok(c) => s.builder = Some(c),
            Err(e) => crate::trace_log(&format!("index {l}: could not start the build: {e}")),
        }
    }

    /// marks the shelf as in use; ten seconds after the last use it is written back and emptied
    fn touch() {
        *LAST_USE.lock().unwrap() = Some(Instant::now());
        if SWEEPER.swap(true, Ordering::AcqRel) {
            return;
        }
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_secs(5));
            let idle = LAST_USE.lock().unwrap().is_none_or(|t| t.elapsed() >= KEEP);
            if idle {
                put_away();
                SWEEPER.store(false, Ordering::Release);
                // a search may have come in just now
                if LAST_USE.lock().unwrap().is_some_and(|t| t.elapsed() < KEEP) && !SWEEPER.swap(true, Ordering::AcqRel)
                {
                    continue;
                }
                return;
            }
        });
    }

    /// writes back what changed and lets every index go
    pub fn put_away() {
        let slots: Vec<(char, Arc<Mutex<Slot>>)> = SLOTS
            .lock()
            .unwrap()
            .as_ref()
            .map(|m| m.iter().map(|(k, v)| (*k, v.clone())).collect())
            .unwrap_or_default();
        for (l, slot) in slots {
            let mut s = slot.lock().unwrap();
            if let Some(mut ix) = s.ix.take() {
                if ix.dirty {
                    if let Some(f) = file_for(&format!("{l}:\\")) {
                        let _ = ix.save(&f);
                    }
                }
            }
        }
        crate::trim_memory();
    }

    /// turns the index off: lets go of every index and deletes the saved ones
    pub fn forget() {
        if let Some(m) = SLOTS.lock().unwrap().as_mut() {
            m.clear();
        }
        if let Some(dir) = file_for("C:\\").and_then(|f| f.parent().map(|p| p.to_path_buf())) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip() {
        let mut e = Enc::new(b"Folder");
        let names = ["alpha.txt", "alphabet.txt", "b", "Ünïcode ✓.md", "zz"];
        for (k, n) in names.iter().enumerate() {
            let w: Vec<u16> = n.encode_utf16().collect();
            e.push(&w, if k == 2 { F_DIR | F_SUB } else { 0 }, 0x1234_5678_9ABC);
        }
        assert_eq!(own_name(&e.data), b"Folder");
        let mut it = Items::new(&e.data);
        for (k, n) in names.iter().enumerate() {
            let (flags, frn, name) = it.next().unwrap();
            assert_eq!(std::str::from_utf8(name).unwrap(), *n);
            assert_eq!(flags & F_SUB != 0, k == 2);
            assert_eq!(frn, if k == 2 { 0x1234_5678_9ABC } else { 0 });
        }
        assert!(it.next().is_none());
    }

    fn names(ix: &Index, text: &str) -> Vec<String> {
        let (hits, _, _, _) = ix.search(&Query::parse(text), None, 1000);
        let mut v: Vec<String> =
            hits.iter().map(|h| crate::pathx::join(&h.dir, &h.name)[ix.root.len()..].to_string()).collect();
        v.sort();
        v
    }

    #[test]
    fn follows_changes() {
        let root = std::env::temp_dir().join(format!("dvfiles_index_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["a/inner", "b", "gone/deep"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("a/inner/note.txt"), "x").unwrap();
        std::fs::write(root.join("gone/deep/old.txt"), "x").unwrap();
        let r = root.to_str().unwrap().to_string();
        let stop = AtomicBool::new(false);
        let Ok(mut ix) = Index::build(&r, &stop) else {
            // a drive without a change journal (a RAM disk TEMP): nothing to follow
            let _ = std::fs::remove_dir_all(&root);
            return;
        };
        assert_eq!(names(&ix, "note"), vec![r"\a\inner\note.txt"]);
        assert_eq!(names(&ix, "old"), vec![r"\gone\deep\old.txt"]);

        // a new file, a new folder with things in it, a renamed folder, a folder moved, a folder deleted
        std::fs::write(root.join("b/fresh.txt"), "x").unwrap();
        std::fs::create_dir_all(root.join("b/newdir/sub")).unwrap();
        std::fs::write(root.join("b/newdir/sub/innew.txt"), "x").unwrap();
        std::fs::rename(root.join("a/inner"), root.join("a/renamed")).unwrap();
        std::fs::rename(root.join("a"), root.join("b/a")).unwrap();
        std::fs::remove_dir_all(root.join("gone")).unwrap();
        ix.refresh(&stop).unwrap();

        assert_eq!(names(&ix, "fresh"), vec![r"\b\fresh.txt"]);
        assert_eq!(names(&ix, "innew"), vec![r"\b\newdir\sub\innew.txt"]);
        assert_eq!(names(&ix, "note"), vec![r"\b\a\renamed\note.txt"]);
        assert!(names(&ix, "old").is_empty());
        assert!(names(&ix, "deep").is_empty());
        // scoped to a folder
        let (hits, _, _, _) = ix.search(&Query::parse("txt"), Some(&format!(r"{r}\b\a")), 100);
        assert_eq!(hits.len(), 1);

        // and the saved form gives the same answers
        let file = root.with_extension("idx");
        ix.save(&file).unwrap();
        let back = Index::load(&file).unwrap();
        assert_eq!(names(&back, "txt"), names(&ix, "txt"));
        // mapped files are let go before they can be deleted
        drop(back);
        drop(ix);
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn lengths() {
        for n in [0usize, 1, 127, 128, 300, 70_000] {
            let mut v = Vec::new();
            put_len(&mut v, n);
            let mut at = 0;
            assert_eq!(get_len(&v, &mut at), n);
            assert_eq!(at, v.len());
        }
    }
}
