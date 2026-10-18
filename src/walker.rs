// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Parallel folder walk, for search, folder sizes and copying. Several threads take folders from a shared stack, so
//! the time each folder takes to open overlaps with the others. Junctions, symlinks, mount points and cloud folder
//! placeholders are reported but never entered, so nothing is counted twice and loops cannot happen. Only metadata
//! is read. The idea follows WinMem's walker (MIT, same author set), on dvfiles' own directory reader.
use crate::fsx::{DirReader, RawEnt};
use crate::wide::long;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Default, Debug)]
pub struct Stats {
    pub dirs: AtomicU64,
    pub entries: AtomicU64,
    pub errors: AtomicU64,
    /// the search was answered from the index (see `index`)
    pub indexed: AtomicBool,
}

pub fn default_threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 16)
}

struct Queue {
    stack: Mutex<Vec<Arc<str>>>,
    cv: Condvar,
    // folders queued or being read; the walk ends when it reaches zero
    pending: AtomicUsize,
}

/// Walks the given folders. `visit(folder, entry)` sees every entry; for a plain folder its return value says
/// whether to walk into it. `on_error(folder, err)` hears about folders that could not be read. Returns early once
/// `stop` is set.
pub fn walk<V, E>(roots: &[String], threads: usize, stop: &AtomicBool, stats: &Stats, visit: V, on_error: E)
where
    V: Fn(&Arc<str>, &RawEnt) -> bool + Sync,
    E: Fn(&str, &std::io::Error) + Sync,
{
    let q = Queue {
        stack: Mutex::new(roots.iter().map(|r| Arc::from(r.as_str())).collect()),
        cv: Condvar::new(),
        pending: AtomicUsize::new(roots.len()),
    };
    if roots.is_empty() {
        return;
    }
    let threads = threads.max(1);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| worker(&q, stop, stats, &visit, &on_error));
        }
    });
}

fn worker<V, E>(q: &Queue, stop: &AtomicBool, stats: &Stats, visit: &V, on_error: &E)
where
    V: Fn(&Arc<str>, &RawEnt) -> bool + Sync,
    E: Fn(&str, &std::io::Error) + Sync,
{
    let mut reader = DirReader::default();
    let mut found: Vec<Arc<str>> = Vec::new();
    loop {
        let dir = {
            let mut stack = q.stack.lock().unwrap();
            loop {
                if stop.load(Ordering::Relaxed) {
                    q.cv.notify_all();
                    return;
                }
                if let Some(d) = stack.pop() {
                    break d;
                }
                if q.pending.load(Ordering::Acquire) == 0 {
                    q.cv.notify_all();
                    return;
                }
                stack = q.cv.wait(stack).unwrap();
            }
        };
        stats.dirs.fetch_add(1, Ordering::Relaxed);
        let mut n = 0u64;
        let r = reader.read(&long(&dir), |e| {
            n += 1;
            if visit(&dir, e) && e.is_plain_dir() && !stop.load(Ordering::Relaxed) {
                let name = String::from_utf16_lossy(e.name);
                found.push(Arc::from(crate::pathx::join(&dir, &name)));
            }
        });
        stats.entries.fetch_add(n, Ordering::Relaxed);
        if let Err(e) = r {
            stats.errors.fetch_add(1, Ordering::Relaxed);
            on_error(&dir, &e);
        }
        if !found.is_empty() {
            q.pending.fetch_add(found.len(), Ordering::AcqRel);
            let mut stack = q.stack.lock().unwrap();
            stack.append(&mut found);
            drop(stack);
            q.cv.notify_all();
        }
        if q.pending.fetch_sub(1, Ordering::AcqRel) == 1 {
            // last folder done: wake everyone so they can leave
            let _guard = q.stack.lock().unwrap();
            q.cv.notify_all();
        }
    }
}

/// Totals for Properties and the copy planner.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Totals {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    /// estimate: each file rounded up to the cluster size, cloud-only files 0
    pub on_disk: u64,
}

#[derive(Default)]
pub struct LiveTotals {
    pub files: AtomicU64,
    pub dirs: AtomicU64,
    pub bytes: AtomicU64,
    pub on_disk: AtomicU64,
}

impl LiveTotals {
    pub fn get(&self) -> Totals {
        Totals {
            files: self.files.load(Ordering::Relaxed),
            dirs: self.dirs.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
            on_disk: self.on_disk.load(Ordering::Relaxed),
        }
    }
}

/// Adds up everything below the folders in `roots` (not counting the roots themselves) into `live`, which another
/// thread may read while this runs.
pub fn measure(roots: &[String], cluster: u64, stop: &AtomicBool, live: &LiveTotals) {
    let stats = Stats::default();
    let cluster = cluster.max(512);
    walk(
        roots,
        default_threads(),
        stop,
        &stats,
        |_, e| {
            if e.is_dir() {
                live.dirs.fetch_add(1, Ordering::Relaxed);
            } else {
                live.files.fetch_add(1, Ordering::Relaxed);
                live.bytes.fetch_add(e.size, Ordering::Relaxed);
                let disk = if crate::fsx::is_cloud_only(e.attrs) {
                    0
                } else if e.attrs & (crate::fsx::FA_COMPRESSED | crate::fsx::FA_SPARSE) != 0 {
                    // the file system already reports what these occupy
                    e.alloc
                } else {
                    e.size.div_ceil(cluster) * cluster
                };
                live.on_disk.fetch_add(disk, Ordering::Relaxed);
            }
            true
        },
        |_, _| {},
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn tree(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("dvfiles_walk_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in 0..8 {
            let dir = root.join(format!("d{d}")).join("inner").join("deeper");
            std::fs::create_dir_all(&dir).unwrap();
            for f in 0..10 {
                std::fs::write(dir.join(format!("f{f}.dat")), vec![7u8; d * 100 + f]).unwrap();
            }
        }
        root
    }

    #[test]
    fn walks_everything_once() {
        let root = tree("once");
        let r = root.to_str().unwrap().to_string();
        let seen = Mutex::new(HashSet::new());
        let stats = Stats::default();
        let stop = AtomicBool::new(false);
        walk(
            &[r.clone()],
            4,
            &stop,
            &stats,
            |dir, e| {
                let p = format!("{dir}\\{}", String::from_utf16_lossy(e.name));
                assert!(seen.lock().unwrap().insert(p), "visited twice");
                true
            },
            |_, _| panic!("no errors expected"),
        );
        // 8 x (d, inner, deeper) folders + 80 files
        assert_eq!(seen.lock().unwrap().len(), 8 * 3 + 80);
        assert_eq!(stats.dirs.load(Ordering::Relaxed), 1 + 24);
        let live = LiveTotals::default();
        measure(&[r], 4096, &stop, &live);
        let t = live.get();
        let bytes: u64 = (0..8u64).map(|d| (0..10u64).map(|f| d * 100 + f).sum::<u64>()).sum();
        assert_eq!((t.files, t.dirs, t.bytes), (80, 24, bytes));
        assert!(t.on_disk >= t.bytes);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stop_and_errors() {
        let root = tree("stop");
        let stop = AtomicBool::new(true);
        let stats = Stats::default();
        walk(&[root.to_str().unwrap().into()], 4, &stop, &stats, |_, _| true, |_, _| {});
        assert!(stats.dirs.load(Ordering::Relaxed) <= 1);
        let errors = AtomicU64::new(0);
        let stop = AtomicBool::new(false);
        walk(
            &[r"C:\definitely\not\here".into()],
            2,
            &stop,
            &stats,
            |_, _| true,
            |_, _| {
                errors.fetch_add(1, Ordering::Relaxed);
            },
        );
        assert_eq!(errors.load(Ordering::Relaxed), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn junctions_are_not_entered() {
        let root = tree("junction");
        let link = root.join("link");
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(root.join("d1"))
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            let live = LiveTotals::default();
            measure(&[root.to_str().unwrap().into()], 4096, &AtomicBool::new(false), &live);
            assert_eq!(live.get().files, 80, "the junction's target is counted once");
            let _ = std::process::Command::new("cmd").args(["/C", "rmdir"]).arg(&link).output();
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
