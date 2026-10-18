// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Undo for what dvfiles did: renames, new items, moves on one drive, copies, and items it put in the Recycle Bin
//! itself. Each undo first checks that nothing has changed in the way (the item is still where it was put, and its
//! old place is free); anything that does not check out is left alone and reported. Permanent deletes, moves between
//! drives and jobs that replaced or merged items cannot be undone exactly, so they are not offered.
use crate::fsx;
use crate::ops::Progress;
use crate::pathx;
use crate::wide::{long, pcwstr};
use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVE_FILE_FLAGS};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// `dir\from` was renamed to `dir\to`
    Rename { dir: String, from: String, to: String },
    /// new folders or files
    Created(Vec<String>),
    /// (from, to) pairs moved by renaming
    Moved(Vec<(String, String)>),
    /// the copies made
    Copied(Vec<String>),
    /// (original path, `$R` entry, `$I` record) for items recycled directly
    Recycled(Vec<(String, String, String)>),
}

impl Action {
    /// "Undo rename", "Undo delete", ... for menus
    pub fn label(&self) -> &'static str {
        match self {
            Action::Rename { .. } => "Undo rename",
            Action::Created(_) => "Undo new",
            Action::Moved(_) => "Undo move",
            Action::Copied(_) => "Undo copy",
            Action::Recycled(_) => "Undo delete",
        }
    }
}

impl Action {
    /// what was done, in a few words, for the undo bubble ("Moved 3 items to Pictures"); None for new items, whose
    /// name is being typed already
    pub fn summary(&self) -> Option<String> {
        let name = |p: &str| pathx::file_name(p).to_string();
        let folder = |p: &str| pathx::parent(p).map(|d| pathx::file_name(&d).to_string()).filter(|n| !n.is_empty());
        let count = |n: usize| if n == 1 { "1 item".to_string() } else { format!("{n} items") };
        match self {
            Action::Rename { to, .. } => Some(format!("Renamed to \"{to}\"")),
            Action::Created(_) => None,
            Action::Moved(v) if v.len() > 1 && v.iter().all(|(f, t)| pathx::parent(f) == pathx::parent(t)) => {
                Some(format!("Renamed {}", count(v.len())))
            }
            Action::Moved(v) => {
                let what = if v.len() == 1 { format!("\"{}\"", name(&v[0].0)) } else { count(v.len()) };
                Some(match v.first().and_then(|(_, to)| folder(to)) {
                    Some(f) => format!("Moved {what} to {f}"),
                    None => format!("Moved {what}"),
                })
            }
            Action::Copied(v) => {
                let what = if v.len() == 1 { format!("\"{}\"", name(&v[0])) } else { count(v.len()) };
                Some(match v.first().and_then(|p| folder(p)) {
                    Some(f) => format!("Copied {what} to {f}"),
                    None => format!("Copied {what}"),
                })
            }
            Action::Recycled(v) => Some(if v.len() == 1 {
                format!("\"{}\" went to the Recycle Bin", name(&v[0].0))
            } else {
                format!("{} went to the Recycle Bin", count(v.len()))
            }),
        }
    }
}

fn rename(from: &str, to: &str) -> Result<(), String> {
    unsafe { MoveFileExW(pcwstr(&long(from)), pcwstr(&long(to)), MOVE_FILE_FLAGS(0)) }
        .map_err(|e| e.message().to_string())
}

/// Puts back what `action` did. Returns the problems: each item that could not be put back and why; the rest
/// was undone.
pub fn undo(action: &Action) -> Vec<(String, String)> {
    let mut problems = Vec::new();
    match action {
        Action::Rename { dir, from, to } => {
            let (now, back) = (pathx::join(dir, to), pathx::join(dir, from));
            let case_only = from.eq_ignore_ascii_case(to);
            if !fsx::exists(&now) {
                problems.push((now, "It is no longer there.".into()));
            } else if !case_only && fsx::exists(&back) {
                problems.push((back, "Something else now has the old name.".into()));
            } else if let Err(e) = rename(&now, &back) {
                problems.push((now, e));
            }
        }
        Action::Created(paths) | Action::Copied(paths) => {
            // what was made goes to the Recycle Bin, so undoing an undo stays possible from there
            let there: Vec<String> = paths.iter().filter(|p| fsx::exists(p)).cloned().collect();
            if !there.is_empty() {
                let p = Progress::default();
                crate::recycle::recycle(&there, &p);
                problems.extend(p.errors.into_inner().unwrap());
            }
        }
        Action::Moved(pairs) => {
            for (from, to) in pairs.iter().rev() {
                if !fsx::exists(to) {
                    problems.push((to.clone(), "It is no longer there.".into()));
                } else if fsx::exists(from) {
                    problems.push((from.clone(), "Something else is now in its old place.".into()));
                } else if !pathx::parent(from).is_some_and(|d| fsx::is_dir(&d)) {
                    problems.push((from.clone(), "Its old folder no longer exists.".into()));
                } else if let Err(e) = rename(to, from) {
                    problems.push((to.clone(), e));
                }
            }
        }
        Action::Recycled(items) => {
            for (original, r, i) in items {
                if !fsx::exists(r) {
                    problems.push((original.clone(), "It is no longer in the Recycle Bin.".into()));
                    continue;
                }
                if fsx::exists(original) {
                    problems.push((original.clone(), "Something else is now in its old place.".into()));
                    continue;
                }
                // as the Recycle Bin's own Restore does, the folders it was in are made again if they are gone
                if let Some(d) = pathx::parent(original) {
                    let _ = std::fs::create_dir_all(&d);
                }
                match rename(r, original) {
                    Ok(()) => {
                        let _ = std::fs::remove_file(i);
                    }
                    Err(e) => problems.push((original.clone(), e)),
                }
            }
            crate::recycle::refresh_bin();
        }
    }
    problems
}

#[cfg(test)]
mod summary_tests {
    use super::*;

    #[test]
    fn what_the_bubble_says() {
        let r = Action::Rename { dir: r"C:\a".into(), from: "x.txt".into(), to: "y.txt".into() };
        assert_eq!(r.summary().as_deref(), Some("Renamed to \"y.txt\""));
        assert_eq!(Action::Created(vec![r"C:\a\New folder".into()]).summary(), None);
        let m = Action::Moved(vec![
            (r"C:\a\1.jpg".into(), r"C:\Pictures\1.jpg".into()),
            (r"C:\a\2.jpg".into(), r"C:\Pictures\2.jpg".into()),
        ]);
        assert_eq!(m.summary().as_deref(), Some("Moved 2 items to Pictures"));
        let c = Action::Copied(vec![r"D:\Backup\notes.md".into()]);
        assert_eq!(c.summary().as_deref(), Some("Copied \"notes.md\" to Backup"));
        let d = Action::Recycled(vec![(r"C:\a\old.zip".into(), String::new(), String::new())]);
        assert_eq!(d.summary().as_deref(), Some("\"old.zip\" went to the Recycle Bin"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{self, Always, Job, Kind, Resolution};
    use std::sync::Arc;

    fn scratch(name: &str) -> String {
        let root = std::env::temp_dir().join(format!("dvfiles_undo_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root.to_str().unwrap().to_string()
    }

    #[test]
    fn rename_back_unless_the_name_is_taken() {
        let d = scratch("rename");
        std::fs::write(format!(r"{d}\new.txt"), b"x").unwrap();
        let a = Action::Rename { dir: d.clone(), from: "old.txt".into(), to: "new.txt".into() };
        assert!(undo(&a).is_empty());
        assert!(fsx::exists(&format!(r"{d}\old.txt")) && !fsx::exists(&format!(r"{d}\new.txt")));
        // again: the renamed item is gone now, so nothing happens and it says why
        assert_eq!(undo(&a).len(), 1);
        // the old name taken by something else: left alone
        std::fs::write(format!(r"{d}\new.txt"), b"y").unwrap();
        assert_eq!(undo(&a).len(), 1);
        assert_eq!(std::fs::read(format!(r"{d}\new.txt")).unwrap(), b"y");
        assert_eq!(std::fs::read(format!(r"{d}\old.txt")).unwrap(), b"x");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn move_back_from_the_engines_record() {
        let d = scratch("move");
        std::fs::create_dir_all(format!(r"{d}\a\inner")).unwrap();
        std::fs::write(format!(r"{d}\a\inner\f.txt"), b"f").unwrap();
        std::fs::create_dir_all(format!(r"{d}\into")).unwrap();
        let job = Job {
            kind: Kind::Move,
            sources: vec![format!(r"{d}\a")],
            dest: Some(format!(r"{d}\into")),
            progress: Arc::new(ops::Progress::default()),
        };
        ops::run(&job, &Always(Resolution::Skip));
        let log = job.progress.undo.lock().unwrap().clone();
        assert!(!log.inexact);
        assert_eq!(log.moved, vec![(format!(r"{d}\a"), format!(r"{d}\into\a"))]);
        assert!(undo(&Action::Moved(log.moved.clone())).is_empty());
        assert_eq!(std::fs::read(format!(r"{d}\a\inner\f.txt")).unwrap(), b"f");
        assert!(!fsx::exists(&format!(r"{d}\into\a")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_move_that_merged_is_not_exact() {
        let d = scratch("merge");
        std::fs::create_dir_all(format!(r"{d}\a")).unwrap();
        std::fs::create_dir_all(format!(r"{d}\into\a")).unwrap();
        let job = Job {
            kind: Kind::Move,
            sources: vec![format!(r"{d}\a")],
            dest: Some(format!(r"{d}\into")),
            progress: Arc::new(ops::Progress::default()),
        };
        ops::run(&job, &Always(Resolution::Skip));
        assert!(job.progress.undo.lock().unwrap().inexact);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn copies_are_recorded_only_when_new() {
        let d = scratch("copy");
        std::fs::create_dir_all(format!(r"{d}\src")).unwrap();
        std::fs::write(format!(r"{d}\src\f.txt"), b"f").unwrap();
        std::fs::create_dir_all(format!(r"{d}\out")).unwrap();
        let job = Job {
            kind: Kind::Copy,
            sources: vec![format!(r"{d}\src")],
            dest: Some(format!(r"{d}\out")),
            progress: Arc::new(ops::Progress::default()),
        };
        ops::run(&job, &Always(Resolution::Skip));
        let log = job.progress.undo.lock().unwrap().clone();
        assert_eq!((log.created.clone(), log.inexact), (vec![format!(r"{d}\out\src")], false));
        // copying again merges into the existing copy: not exact
        let job = Job {
            kind: Kind::Copy,
            sources: vec![format!(r"{d}\src")],
            dest: Some(format!(r"{d}\out")),
            progress: Arc::new(ops::Progress::default()),
        };
        ops::run(&job, &Always(Resolution::Skip));
        assert!(job.progress.undo.lock().unwrap().inexact);
        let _ = std::fs::remove_dir_all(&d);
    }

    // Recycles a scratch file into the real bin, then undoes it: the file is back and its bin entries are gone.
    #[test]
    fn restore_from_the_recycle_bin() {
        let d = scratch("bin");
        if ["OneDrive", "OneDriveConsumer"]
            .iter()
            .filter_map(|v| std::env::var(v).ok())
            .any(|r| pathx::is_within(&d, &r))
        {
            return;
        }
        let f = format!(r"{d}\gone.txt");
        std::fs::write(&f, b"keep").unwrap();
        let job = Job {
            kind: Kind::Recycle,
            sources: vec![f.clone()],
            dest: None,
            progress: Arc::new(ops::Progress::default()),
        };
        ops::run(&job, &Always(Resolution::Skip));
        let log = job.progress.undo.lock().unwrap().clone();
        if log.inexact {
            // the shell took it (no bin of ours on this volume); nothing of dvfiles' to undo
            return;
        }
        let (orig, r, i) = log.recycled[0].clone();
        assert_eq!(orig, f);
        assert!(fsx::exists(&r) && fsx::exists(&i) && !fsx::exists(&f));
        assert!(undo(&Action::Recycled(log.recycled.clone())).is_empty());
        assert_eq!(std::fs::read(&f).unwrap(), b"keep");
        assert!(!fsx::exists(&r) && !fsx::exists(&i));
        let _ = std::fs::remove_dir_all(&d);
    }
}
