// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Copy, move and delete.
//!
//! Where the time goes in Explorer and what is done instead:
//! - **No "Calculating..." phase.** Copying starts with the first file found; totals keep growing while the folder
//!   walk (parallel, metadata only) runs alongside.
//! - **Several files at once.** Small files are dominated by per-file costs (open, create, attributes, the virus
//!   scanner looking at each new file), so a pool of workers copies files in parallel: up to 8 on SSDs, one on a
//!   spinning disk, where parallel streams would make the head seek.
//! - **Each file is copied by `CopyFile2`**, which keeps attributes, times and alternate streams (the "downloaded from
//!   the internet" mark included); files of 4 GB and more skip the cache (`COPY_FILE_NO_BUFFERING`).
//! - **A move on one volume is a rename** of each top-level item, however big it is.
//! - **Deleting** removes files from several threads while the walk is still finding more.
use crate::fsx::{self, FA_DIRECTORY, FA_READONLY};
use crate::naming;
use crate::pathx;
use crate::walker::{self, Stats};
use crate::wide::{long, pcwstr};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use windows::core::BOOL;
use windows::Win32::Storage::FileSystem::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Copy,
    Move,
    Delete,
    Recycle,
}

impl Kind {
    pub fn verb(self) -> &'static str {
        match self {
            Kind::Copy => "Copying",
            Kind::Move => "Moving",
            Kind::Delete => "Deleting",
            Kind::Recycle => "Recycling",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Resolution {
    Replace,
    Skip,
    KeepBoth,
}

/// what the conflict dialog is shown
pub struct ConflictInfo {
    pub src: String,
    pub dst: String,
    pub src_size: u64,
    pub dst_size: u64,
    pub src_modified: u64,
    pub dst_modified: u64,
}

/// The UI's answer to a name conflict; `true` applies the answer to the rest of the job.
pub trait Asker: Send + Sync {
    fn conflict(&self, info: &ConflictInfo) -> (Resolution, bool);
}

/// answers every conflict the same way, for scripts
pub struct Always(pub Resolution);

impl Asker for Always {
    fn conflict(&self, _: &ConflictInfo) -> (Resolution, bool) {
        (self.0, true)
    }
}

#[derive(Default)]
pub struct Progress {
    pub total_bytes: AtomicU64,
    pub total_items: AtomicU64,
    pub done_bytes: AtomicU64,
    pub done_items: AtomicU64,
    /// the walk is still finding items, so totals may grow
    pub scanning: AtomicBool,
    pub paused: AtomicBool,
    pub cancel: AtomicBool,
    pub finished: AtomicBool,
    /// waiting for the user to answer a conflict
    pub asking: AtomicBool,
    pub current: Mutex<String>,
    pub errors: Mutex<Vec<(String, String)>>,
    /// top-level results, so the window can select what was pasted
    pub results: Mutex<Vec<String>>,
    /// what undo needs to put this job back
    pub undo: Mutex<UndoLog>,
    gate: (Mutex<()>, Condvar),
}

impl Progress {
    pub fn set_paused(&self, on: bool) {
        self.paused.store(on, Ordering::SeqCst);
        let _g = self.gate.0.lock().unwrap();
        self.gate.1.notify_all();
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.set_paused(false);
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// blocks while paused; false once cancelled
    fn wait_if_paused(&self) -> bool {
        if self.paused.load(Ordering::Relaxed) {
            let mut g = self.gate.0.lock().unwrap();
            while self.paused.load(Ordering::SeqCst) && !self.cancelled() {
                g = self.gate.1.wait(g).unwrap();
            }
        }
        !self.cancelled()
    }

    fn error(&self, path: &str, e: impl std::fmt::Display) {
        self.errors.lock().unwrap().push((path.to_string(), e.to_string()));
    }

    fn set_current(&self, path: &str) {
        if let Ok(mut c) = self.current.try_lock() {
            c.clear();
            c.push_str(pathx::file_name(path));
        }
    }
}

/// What a job did, for undo. `inexact` is set when something was replaced, merged, moved between drives or handed
/// to the shell: such a job cannot be put back exactly, so it is not offered.
#[derive(Default, Clone, Debug)]
pub struct UndoLog {
    /// top-level (from, to) moves done by renaming
    pub moved: Vec<(String, String)>,
    /// top-level items this job created
    pub created: Vec<String>,
    /// (original, `$R` entry, `$I` record) for items recycled directly
    pub recycled: Vec<(String, String, String)>,
    pub inexact: bool,
}

impl Progress {
    fn not_exact(&self) {
        self.undo.lock().unwrap().inexact = true;
    }
}

pub struct Job {
    pub kind: Kind,
    pub sources: Vec<String>,
    pub dest: Option<String>,
    pub progress: Arc<Progress>,
}

pub fn run(job: &Job, asker: &dyn Asker) {
    let p = &job.progress;
    p.scanning.store(true, Ordering::SeqCst);
    match (job.kind, job.dest.as_deref()) {
        (Kind::Copy | Kind::Move, Some(dest)) => transfer(job.kind, &job.sources, dest, p, asker),
        (Kind::Delete, _) => delete(&job.sources, p),
        (Kind::Recycle, _) => crate::recycle::recycle(&job.sources, p),
        _ => {}
    }
    p.scanning.store(false, Ordering::SeqCst);
    p.finished.store(true, Ordering::SeqCst);
}

// ---------------------------------------------------------------------------------------------------------------
// conflicts

struct Conflicts<'a> {
    asker: &'a dyn Asker,
    // one question at a time, and the "do this for all" answer
    sticky: Mutex<Option<Resolution>>,
}

impl Conflicts<'_> {
    fn resolve(&self, p: &Progress, src: &str, dst: &str) -> Resolution {
        let mut sticky = self.sticky.lock().unwrap();
        if let Some(r) = *sticky {
            p.not_exact();
            return r;
        }
        let (s, d) = (fsx::stat(src).ok(), fsx::stat(dst).ok());
        let info = ConflictInfo {
            src: src.into(),
            dst: dst.into(),
            src_size: s.as_ref().map_or(0, |e| e.size),
            dst_size: d.as_ref().map_or(0, |e| e.size),
            src_modified: s.as_ref().map_or(0, |e| e.modified),
            dst_modified: d.as_ref().map_or(0, |e| e.modified),
        };
        p.not_exact();
        p.asking.store(true, Ordering::SeqCst);
        let (r, all) = self.asker.conflict(&info);
        p.asking.store(false, Ordering::SeqCst);
        if all {
            *sticky = Some(r);
        }
        r
    }
}

fn keep_both_target(dst: &str, is_dir: bool) -> String {
    let dir = pathx::parent(dst).unwrap_or_default();
    let name = naming::numbered_name(pathx::file_name(dst), is_dir, |n| fsx::exists(&pathx::join(&dir, n)));
    pathx::join(&dir, &name)
}

// ---------------------------------------------------------------------------------------------------------------
// copy and move

struct Task {
    src: String,
    dst: String,
    size: u64,
    // index of the top-level source this file belongs to, to know which sources a move may delete afterwards
    top: usize,
}

/// work handed from a walk to a pool of workers; `close` once the walk is done
struct TaskQueue<T = Task> {
    q: Mutex<VecDeque<T>>,
    cv: Condvar,
    closed: AtomicBool,
}

impl<T> TaskQueue<T> {
    fn new() -> Self {
        TaskQueue { q: Mutex::new(VecDeque::new()), cv: Condvar::new(), closed: AtomicBool::new(false) }
    }

    fn push(&self, t: T) {
        self.q.lock().unwrap().push_back(t);
        self.cv.notify_one();
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let _g = self.q.lock().unwrap();
        self.cv.notify_all();
    }

    fn pop(&self, p: &Progress) -> Option<T> {
        let mut q = self.q.lock().unwrap();
        loop {
            if p.cancelled() {
                return None;
            }
            if let Some(t) = q.pop_front() {
                return Some(t);
            }
            if self.closed.load(Ordering::SeqCst) {
                return None;
            }
            q = self.cv.wait(q).unwrap();
        }
    }
}

/// how many files to copy at once between these two places
pub fn copy_threads(src: &str, dst: &str) -> usize {
    let roots = [fsx::volume_root(src), fsx::volume_root(dst)];
    // measured on NVMe: 4, 8 and 16 copies at once take the same time for 2,000 small files
    let mut threads = walker::default_threads().min(8);
    for r in roots.iter().flatten() {
        match fsx::drive_kind(r) {
            fsx::DriveKind::Network => threads = threads.min(4),
            fsx::DriveKind::Optical => threads = 1,
            _ => {
                if fsx::seek_penalty(r) == Some(true) {
                    threads = 1;
                }
            }
        }
    }
    threads.max(1)
}

fn transfer(kind: Kind, sources: &[String], dest: &str, p: &Progress, asker: &dyn Asker) {
    let conflicts = Conflicts { asker, sticky: Mutex::new(None) };
    let queue: TaskQueue = TaskQueue::new();
    // per top-level source: whether anything inside failed or was skipped (a move then keeps the source)
    let failed: Vec<AtomicBool> = sources.iter().map(|_| AtomicBool::new(false)).collect();
    let threads = sources.first().map_or(1, |s| copy_threads(s, dest));
    // a move to another volume deletes each source once all of it has arrived
    let mut copied_moves: Vec<usize> = Vec::new();

    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| copy_worker(&queue, p, &conflicts, &failed));
        }
        for (i, src) in sources.iter().enumerate() {
            if p.cancelled() {
                break;
            }
            let Ok(meta) = fsx::stat(src) else {
                p.error(src, "The item could not be found.");
                failed[i].store(true, Ordering::Relaxed);
                continue;
            };
            let is_dir = meta.is_dir();
            let name = pathx::file_name(src).to_string();
            let mut target = pathx::join(dest, &name);
            if is_dir && pathx::is_within(dest, src) {
                p.error(src, "The destination folder is a subfolder of the source folder.");
                failed[i].store(true, Ordering::Relaxed);
                continue;
            }
            let same_folder = pathx::parent(src).is_some_and(|d| pathx::same(&d, dest));
            if same_folder {
                if kind == Kind::Move {
                    continue;
                }
                let n = naming::copy_name(&name, is_dir, |n| fsx::exists(&pathx::join(dest, n)));
                target = pathx::join(dest, &n);
            }
            p.set_current(src);
            if kind == Kind::Move && fsx::same_volume(src, dest) {
                p.total_items.fetch_add(1, Ordering::Relaxed);
                let existed = fsx::exists(&target);
                if existed {
                    // a merge or a conflict: not put back exactly
                    p.not_exact();
                }
                if let Some(t) = move_by_rename(src, &target, is_dir, p, &conflicts) {
                    if !existed {
                        p.undo.lock().unwrap().moved.push((src.clone(), t.clone()));
                    }
                    p.results.lock().unwrap().push(t);
                }
                p.done_items.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            let existed = fsx::exists(&target);
            if kind == Kind::Move {
                // the copy can be undone, the delete of the source on the other drive cannot
                p.not_exact();
            } else if existed {
                p.not_exact();
            } else {
                p.undo.lock().unwrap().created.push(target.clone());
            }
            if is_dir {
                if let Err(e) = make_dir(src, &target) {
                    p.error(&target, e);
                    failed[i].store(true, Ordering::Relaxed);
                    continue;
                }
                p.results.lock().unwrap().push(target.clone());
                plan_folder(src, &target, i, p, &queue);
            } else {
                p.total_items.fetch_add(1, Ordering::Relaxed);
                p.total_bytes.fetch_add(meta.size, Ordering::Relaxed);
                p.results.lock().unwrap().push(target.clone());
                queue.push(Task { src: src.clone(), dst: target, size: meta.size, top: i });
            }
            if kind == Kind::Move {
                copied_moves.push(i);
            }
        }
        p.scanning.store(false, Ordering::SeqCst);
        queue.close();
    });

    if kind == Kind::Move && !p.cancelled() {
        let done: Vec<String> = copied_moves
            .into_iter()
            .filter(|&i| !failed[i].load(Ordering::Relaxed))
            .map(|i| sources[i].clone())
            .collect();
        if !done.is_empty() {
            // the copies are complete, so the originals go; errors here are reported but nothing is lost
            let quiet = Progress::default();
            delete(&done, &quiet);
            for (path, e) in quiet.errors.into_inner().unwrap() {
                p.error(&path, e);
            }
        }
    }
}

fn make_dir(template: &str, dst: &str) -> std::io::Result<()> {
    let (t, d) = (long(template), long(dst));
    match unsafe { CreateDirectoryExW(pcwstr(&t), pcwstr(&d), None) } {
        Ok(()) => Ok(()),
        // merging into an existing folder
        Err(_) if fsx::is_dir(dst) => Ok(()),
        Err(e) => Err(fsx::win_err(e)),
    }
}

/// walks a source folder, creating its folders at the destination and queueing its files
fn plan_folder(src: &str, target: &str, top: usize, p: &Progress, queue: &TaskQueue) {
    let stats = Stats::default();
    let src_len = src.trim_end_matches('\\').len();
    let target = target.trim_end_matches('\\');
    walker::walk(
        &[src.to_string()],
        walker::default_threads(),
        &p.cancel,
        &stats,
        |dir, e| {
            let name = String::from_utf16_lossy(e.name);
            let from = pathx::join(dir, &name);
            let to = format!("{target}{}", &from[src_len..]);
            if e.is_dir() {
                if e.is_plain_dir() {
                    if let Err(err) = make_dir(&from, &to) {
                        p.error(&to, err);
                        return false;
                    }
                    return true;
                }
                // a junction or symlink: copied as a link would be surprising, so it is listed as skipped
                p.error(&from, "Links to other folders are not copied.");
                return false;
            }
            p.total_items.fetch_add(1, Ordering::Relaxed);
            p.total_bytes.fetch_add(e.size, Ordering::Relaxed);
            queue.push(Task { src: from, dst: to, size: e.size, top });
            false
        },
        |dir, err| p.error(dir, err),
    );
}

// unbuffered I/O only for files too big to sit in the cache usefully; below that the cache makes copies faster
const NO_BUFFERING_FROM: u64 = 4 << 30;
const PROGRESS_FROM: u64 = 4 << 20;

struct FileProgress<'a> {
    p: &'a Progress,
    reported: u64,
}

unsafe extern "system" fn chunk_done(msg: *const COPYFILE2_MESSAGE, ctx: *const c_void) -> COPYFILE2_MESSAGE_ACTION {
    let fp = &mut *(ctx as *mut FileProgress);
    let m = &*msg;
    if m.Type == COPYFILE2_CALLBACK_CHUNK_FINISHED {
        let done = m.Info.ChunkFinished.uliTotalBytesTransferred;
        if done > fp.reported {
            fp.p.done_bytes.fetch_add(done - fp.reported, Ordering::Relaxed);
            fp.reported = done;
        }
    }
    if !fp.p.wait_if_paused() {
        return COPYFILE2_PROGRESS_CANCEL;
    }
    COPYFILE2_PROGRESS_CONTINUE
}

const E_FILE_EXISTS: i32 = 0x8007_0050u32 as i32;
const E_ALREADY_EXISTS: i32 = 0x8007_00B7u32 as i32;
const E_ACCESS_DENIED: i32 = 0x8007_0005u32 as i32;
const E_ABORTED: i32 = 0x8007_04D3u32 as i32;

fn copy_file(src: &str, dst: &str, size: u64, replace: bool, p: &Progress) -> windows::core::Result<()> {
    let (s, d) = (long(src), long(dst));
    let mut flags = COPY_FILE_FAIL_IF_EXISTS.0;
    if replace {
        flags &= !COPY_FILE_FAIL_IF_EXISTS.0;
    }
    if size >= NO_BUFFERING_FROM {
        flags |= COPY_FILE_NO_BUFFERING.0;
    }
    let mut fp = FileProgress { p, reported: 0 };
    let params = COPYFILE2_EXTENDED_PARAMETERS {
        dwSize: std::mem::size_of::<COPYFILE2_EXTENDED_PARAMETERS>() as u32,
        dwCopyFlags: COPYFILE_FLAGS(flags),
        pfCancel: std::ptr::null_mut::<BOOL>(),
        pProgressRoutine: if size >= PROGRESS_FROM { Some(chunk_done) } else { None },
        pvCallbackContext: &mut fp as *mut FileProgress as *mut c_void,
    };
    let r = unsafe { CopyFile2(pcwstr(&s), pcwstr(&d), Some(&params)) };
    // whatever the callback did not report yet (all of it for small files, or the rest after a failure)
    let rest = if r.is_ok() { size.saturating_sub(fp.reported) } else { 0 };
    p.done_bytes.fetch_add(rest, Ordering::Relaxed);
    if r.is_err() {
        // a failed copy is taken out of the totals so the bar still reaches 100%
        p.total_bytes.fetch_sub(size.min(p.total_bytes.load(Ordering::Relaxed)), Ordering::Relaxed);
        p.done_bytes.fetch_sub(fp.reported.min(p.done_bytes.load(Ordering::Relaxed)), Ordering::Relaxed);
    }
    r
}

fn copy_worker(queue: &TaskQueue, p: &Progress, conflicts: &Conflicts, failed: &[AtomicBool]) {
    while let Some(t) = queue.pop(p) {
        if !p.wait_if_paused() {
            break;
        }
        p.set_current(&t.src);
        let mut dst = t.dst.clone();
        let mut replace = false;
        let result = loop {
            match copy_file(&t.src, &dst, t.size, replace, p) {
                Err(e) if !replace && (e.code().0 == E_FILE_EXISTS || e.code().0 == E_ALREADY_EXISTS) => {
                    match conflicts.resolve(p, &t.src, &dst) {
                        Resolution::Replace => {
                            replace = true;
                            // CopyFile2 cannot overwrite a read-only file
                            if let Some(a) = fsx::attributes(&dst) {
                                if a & FA_READONLY != 0 {
                                    let _ = fsx::set_attributes(&dst, a & !FA_READONLY);
                                }
                            }
                            // the failed attempt was taken out of the totals; put it back for the retry
                            p.total_bytes.fetch_add(t.size, Ordering::Relaxed);
                        }
                        Resolution::KeepBoth => {
                            dst = keep_both_target(&dst, false);
                            p.total_bytes.fetch_add(t.size, Ordering::Relaxed);
                        }
                        Resolution::Skip => break Ok(false),
                    }
                }
                Err(e) if e.code().0 == E_ABORTED => break Ok(false),
                Err(e) => break Err(e),
                Ok(()) => break Ok(true),
            }
        };
        match result {
            Ok(true) => {}
            Ok(false) => failed[t.top].store(true, Ordering::Relaxed),
            Err(e) => {
                failed[t.top].store(true, Ordering::Relaxed);
                let msg = if e.code().0 == E_ACCESS_DENIED {
                    "Access is denied.".to_string()
                } else {
                    e.message().to_string()
                };
                p.error(&t.src, msg);
            }
        }
        p.done_items.fetch_add(1, Ordering::Relaxed);
    }
}

/// Same-volume move: one rename for the whole item. When the destination already has a folder of that name the two
/// are merged, still by renaming each file. Returns the final path.
fn move_by_rename(src: &str, target: &str, is_dir: bool, p: &Progress, conflicts: &Conflicts) -> Option<String> {
    let rename = |from: &str, to: &str, replace: bool| unsafe {
        let flags = if replace { MOVEFILE_REPLACE_EXISTING } else { MOVE_FILE_FLAGS(0) };
        MoveFileExW(pcwstr(&long(from)), pcwstr(&long(to)), flags)
    };
    if !fsx::exists(target) {
        return match rename(src, target, false) {
            Ok(()) => Some(target.to_string()),
            Err(e) => {
                p.error(src, e.message());
                None
            }
        };
    }
    if is_dir && fsx::is_dir(target) {
        merge_by_rename(src, target, p, conflicts);
        return Some(target.to_string());
    }
    let (to, replace) = match conflicts.resolve(p, src, target) {
        Resolution::Skip => return None,
        Resolution::Replace => (target.to_string(), true),
        Resolution::KeepBoth => (keep_both_target(target, is_dir), false),
    };
    if replace && fsx::is_dir(target) != is_dir {
        p.error(src, "A file and a folder cannot replace each other.");
        return None;
    }
    match rename(src, &to, replace) {
        Ok(()) => Some(to),
        Err(e) => {
            p.error(src, e.message());
            None
        }
    }
}

fn merge_by_rename(src: &str, target: &str, p: &Progress, conflicts: &Conflicts) {
    let entries = match fsx::list(src) {
        Ok(v) => v,
        Err(e) => return p.error(src, e),
    };
    for e in entries {
        if p.cancelled() {
            return;
        }
        let (from, to) = (pathx::join(src, &e.name), pathx::join(target, &e.name));
        move_by_rename(&from, &to, e.is_dir(), p, conflicts);
    }
    // the source folder is empty now unless something was skipped
    unsafe {
        let _ = RemoveDirectoryW(pcwstr(&long(src)));
    }
}

// ---------------------------------------------------------------------------------------------------------------
// delete

fn delete_file(path: &str, attrs: u32) -> std::io::Result<()> {
    let w = long(path);
    unsafe {
        if DeleteFileW(pcwstr(&w)).is_ok() {
            return Ok(());
        }
        if attrs & FA_READONLY != 0 {
            let _ = SetFileAttributesW(pcwstr(&w), FILE_FLAGS_AND_ATTRIBUTES(attrs & !FA_READONLY));
            return DeleteFileW(pcwstr(&w)).map_err(fsx::win_err);
        }
    }
    Err(fsx::last_error())
}

fn remove_dir(path: &str, attrs: u32) -> std::io::Result<()> {
    let w = long(path);
    unsafe {
        if RemoveDirectoryW(pcwstr(&w)).is_ok() {
            return Ok(());
        }
        if attrs & FA_READONLY != 0 {
            let _ = SetFileAttributesW(pcwstr(&w), FILE_FLAGS_AND_ATTRIBUTES(attrs & !FA_READONLY));
            return RemoveDirectoryW(pcwstr(&w)).map_err(fsx::win_err);
        }
    }
    Err(fsx::last_error())
}

/// Permanent delete. The walk hands files to a pool of workers, so even one folder with 100,000 files is deleted
/// on several threads while the walk is still finding more; folders are removed deepest first afterwards. Links
/// (junctions, symlinks) are removed themselves, never what they point to. Progress counts files.
pub fn delete(sources: &[String], p: &Progress) {
    for src in sources {
        if p.cancelled() {
            return;
        }
        let Some(attrs) = fsx::attributes(src) else {
            p.error(src, "The item could not be found.");
            continue;
        };
        p.set_current(src);
        let plain_dir = attrs & (FA_DIRECTORY | fsx::FA_REPARSE) == FA_DIRECTORY;
        if !plain_dir {
            p.total_items.fetch_add(1, Ordering::Relaxed);
            let r = if attrs & FA_DIRECTORY != 0 { remove_dir(src, attrs) } else { delete_file(src, attrs) };
            if let Err(e) = r {
                p.error(src, e);
            }
            p.done_items.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let dirs: Mutex<Vec<(usize, String, u32)>> = Mutex::new(vec![(0, src.clone(), attrs)]);
        let failures = AtomicUsize::new(0);
        let queue: TaskQueue<(String, u32)> = TaskQueue::new();
        std::thread::scope(|s| {
            // measured on NVMe: 4 workers delete as fast as 8 (one folder of 20,000 files: 2.3 s either way, 2.6 s
            // with 2, 5 s with 1), so the other four are not used
            for _ in 0..walker::default_threads().min(4) {
                s.spawn(|| {
                    while let Some((path, a)) = queue.pop(p) {
                        if !p.wait_if_paused() {
                            break;
                        }
                        if let Err(err) = delete_file(&path, a) {
                            failures.fetch_add(1, Ordering::Relaxed);
                            p.error(&path, err);
                        }
                        p.done_items.fetch_add(1, Ordering::Relaxed);
                    }
                });
            }
            let stats = Stats::default();
            walker::walk(
                &[src.clone()],
                walker::default_threads(),
                &p.cancel,
                &stats,
                |dir, e| {
                    let path = pathx::join(dir, &String::from_utf16_lossy(e.name));
                    if e.is_dir() {
                        let depth = path.matches('\\').count();
                        dirs.lock().unwrap().push((depth, path, e.attrs));
                        return e.is_plain_dir();
                    }
                    p.total_items.fetch_add(1, Ordering::Relaxed);
                    queue.push((path, e.attrs));
                    false
                },
                |dir, err| p.error(dir, err),
            );
            queue.close();
        });
        let mut dirs = dirs.into_inner().unwrap();
        dirs.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, d, a) in dirs {
            if p.cancelled() {
                break;
            }
            if let Err(e) = remove_dir(&d, a) {
                // a folder that still holds a file that failed is expected to stay; report only new problems
                if failures.load(Ordering::Relaxed) == 0 {
                    p.error(&d, e);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> String {
        let root = std::env::temp_dir().join(format!("dvfiles_ops_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root.to_str().unwrap().to_string()
    }

    fn make_tree(root: &str) -> (u64, u64) {
        let (mut files, mut bytes) = (0, 0);
        for d in 0..5 {
            let dir = format!(r"{root}\src\d{d}\inner");
            std::fs::create_dir_all(&dir).unwrap();
            for f in 0..20 {
                let n = d * 1000 + f * 13;
                std::fs::write(format!(r"{dir}\f{f}.bin"), vec![(f % 251) as u8; n]).unwrap();
                files += 1;
                bytes += n as u64;
            }
        }
        std::fs::write(format!(r"{root}\src\top.txt"), b"top").unwrap();
        (files + 1, bytes + 3)
    }

    fn job(kind: Kind, sources: Vec<String>, dest: Option<String>) -> Job {
        Job { kind, sources, dest, progress: Arc::new(Progress::default()) }
    }

    fn tree_bytes(path: &str) -> (u64, u64) {
        let live = walker::LiveTotals::default();
        walker::measure(&[path.into()], 4096, &AtomicBool::new(false), &live);
        let t = live.get();
        (t.files, t.bytes)
    }

    #[test]
    fn copy_a_tree_then_into_the_same_folder() {
        let root = scratch("copy");
        let (files, bytes) = make_tree(&root);
        let dest = format!(r"{root}\out");
        std::fs::create_dir_all(&dest).unwrap();
        let j = job(Kind::Copy, vec![format!(r"{root}\src")], Some(dest.clone()));
        run(&j, &Always(Resolution::Skip));
        let p = &j.progress;
        assert!(p.errors.lock().unwrap().is_empty(), "{:?}", p.errors.lock().unwrap());
        assert_eq!(tree_bytes(&format!(r"{dest}\src")), (files, bytes));
        assert_eq!(p.done_bytes.load(Ordering::Relaxed), bytes);
        assert_eq!(p.total_items.load(Ordering::Relaxed), files);
        assert_eq!(p.results.lock().unwrap().as_slice(), [format!(r"{dest}\src")]);
        // pasting where it already is makes "- Copy"
        let j = job(Kind::Copy, vec![format!(r"{dest}\src")], Some(dest.clone()));
        run(&j, &Always(Resolution::Skip));
        assert_eq!(tree_bytes(&format!(r"{dest}\src - Copy")), (files, bytes));
        // into itself is refused
        let j = job(Kind::Copy, vec![format!(r"{root}\src")], Some(format!(r"{root}\src\d0")));
        run(&j, &Always(Resolution::Skip));
        assert_eq!(j.progress.errors.lock().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn conflicts_replace_skip_keep_both() {
        let root = scratch("conflict");
        std::fs::create_dir_all(format!(r"{root}\a")).unwrap();
        std::fs::create_dir_all(format!(r"{root}\b")).unwrap();
        std::fs::write(format!(r"{root}\a\x.txt"), b"new").unwrap();
        std::fs::write(format!(r"{root}\b\x.txt"), b"old!").unwrap();
        let src = vec![format!(r"{root}\a\x.txt")];
        let dest = Some(format!(r"{root}\b"));
        run(&job(Kind::Copy, src.clone(), dest.clone()), &Always(Resolution::Skip));
        assert_eq!(std::fs::read(format!(r"{root}\b\x.txt")).unwrap(), b"old!");
        run(&job(Kind::Copy, src.clone(), dest.clone()), &Always(Resolution::KeepBoth));
        assert_eq!(std::fs::read(format!(r"{root}\b\x (2).txt")).unwrap(), b"new");
        let j = job(Kind::Copy, src.clone(), dest.clone());
        run(&j, &Always(Resolution::Replace));
        assert_eq!(std::fs::read(format!(r"{root}\b\x.txt")).unwrap(), b"new");
        let p = &j.progress;
        assert_eq!(p.done_bytes.load(Ordering::Relaxed), p.total_bytes.load(Ordering::Relaxed));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn move_on_one_volume_renames_and_merges() {
        let root = scratch("move");
        let (files, bytes) = make_tree(&root);
        let dest = format!(r"{root}\dest");
        std::fs::create_dir_all(format!(r"{dest}\src\d0")).unwrap();
        std::fs::write(format!(r"{dest}\src\d0\keep.txt"), b"k").unwrap();
        let j = job(Kind::Move, vec![format!(r"{root}\src")], Some(dest.clone()));
        run(&j, &Always(Resolution::Replace));
        assert!(j.progress.errors.lock().unwrap().is_empty(), "{:?}", j.progress.errors.lock().unwrap());
        assert!(!fsx::exists(&format!(r"{root}\src")));
        assert_eq!(tree_bytes(&format!(r"{dest}\src")), (files + 1, bytes + 1));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn delete_tree_with_read_only_files_and_a_junction() {
        let root = scratch("delete");
        make_tree(&root);
        let ro = format!(r"{root}\src\d1\inner\f1.bin");
        fsx::set_attributes(&ro, FA_READONLY).unwrap();
        // a junction inside pointing outside: removing must not empty the target
        let outside = format!(r"{root}\outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(format!(r"{outside}\precious.txt"), b"keep me").unwrap();
        let _ = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J", &format!(r"{root}\src\link"), &outside])
            .output();
        let j = job(Kind::Delete, vec![format!(r"{root}\src")], None);
        run(&j, &Always(Resolution::Skip));
        assert!(j.progress.errors.lock().unwrap().is_empty(), "{:?}", j.progress.errors.lock().unwrap());
        assert!(!fsx::exists(&format!(r"{root}\src")));
        assert_eq!(std::fs::read(format!(r"{outside}\precious.txt")).unwrap(), b"keep me");
        let _ = std::fs::remove_dir_all(&root);
    }
}
