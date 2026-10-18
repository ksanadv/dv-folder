// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! What the list shows: a folder, This PC, or search results, with sorting, hidden-item filtering and selection.
//! Folders are read and searched on threads that post their results back to the window.
use crate::settings::Col;
use crate::ui::post_box;
use dvfiles::fsx::{self, Drive, Entry, FA_DIRECTORY};
use dvfiles::natsort;
use dvfiles::query::Query;
use dvfiles::selection::Selection;
use dvfiles::walker::{self, Stats};
use std::cmp::Ordering;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AO};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::WM_APP;

pub const WM_LISTED: u32 = WM_APP + 12;
pub const WM_SEARCH: u32 = WM_APP + 13;
pub const WM_DRIVES: u32 = WM_APP + 14;
pub const WM_SIZES: u32 = WM_APP + 16;

/// one folder's measured size, from the background sizer
pub struct SizeResult {
    pub generation: usize,
    pub name: String,
    pub path: String,
    pub bytes: u64,
    pub last: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Loc {
    Pc,
    Dir(String),
}

impl Loc {
    pub fn path(&self) -> Option<&str> {
        match self {
            Loc::Dir(p) => Some(p),
            Loc::Pc => None,
        }
    }

    pub fn same(&self, o: &Loc) -> bool {
        match (self, o) {
            (Loc::Pc, Loc::Pc) => true,
            (Loc::Dir(a), Loc::Dir(b)) => dvfiles::pathx::same(a, b),
            _ => false,
        }
    }
}

pub struct Listed {
    pub generation: usize,
    pub loc: Loc,
    pub result: Result<Vec<Entry>, String>,
    pub refresh: bool,
    pub ms: f64,
}

pub struct SearchBatch {
    pub generation: usize,
    pub entries: Vec<Entry>,
    pub done: bool,
    // the walk stopped at MAX_RESULTS
    pub capped: bool,
}

/// a search stops collecting here: a one-letter query over a whole drive would otherwise gather and sort hundreds of
/// thousands of results
pub const MAX_RESULTS: u64 = 100_000;

pub struct Search {
    pub text: String,
    pub stop: Arc<AtomicBool>,
    pub running: bool,
    pub started: Instant,
    pub elapsed: Duration,
    pub stats: Arc<Stats>,
    pub capped: bool,
}

impl Drop for Search {
    fn drop(&mut self) {
        self.stop.store(true, AO::Relaxed);
    }
}

pub struct Model {
    pub loc: Loc,
    pub entries: Vec<Entry>,
    pub order: Vec<u32>,
    pub sel: Selection,
    pub search: Option<Search>,
    pub error: Option<String>,
    pub loading: bool,
    pub show_hidden: bool,
    pub show_super_hidden: bool,
    pub list_ms: f64,
    // what to select (and maybe rename) once the next listing arrives
    pub pending_select: Vec<String>,
    pub pending_rename: bool,
    // measured folder sizes, by lower-case name, and how many are still being measured
    pub sizes: std::collections::HashMap<String, u64>,
    pub sizes_pending: usize,
}

fn super_hidden_setting() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    let v: u32 = winreg::RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced")
        .and_then(|k| k.get_value("ShowSuperHidden"))
        .unwrap_or(0);
    v == 1
}

impl Model {
    pub fn new(loc: Loc, show_hidden: bool) -> Model {
        Model {
            loc,
            entries: Vec::new(),
            order: Vec::new(),
            sel: Selection::new(0),
            search: None,
            error: None,
            loading: true,
            show_hidden,
            show_super_hidden: super_hidden_setting(),
            list_ms: 0.0,
            pending_select: Vec::new(),
            pending_rename: false,
            sizes: std::collections::HashMap::new(),
            sizes_pending: 0,
        }
    }

    pub fn visible(&self, e: &Entry) -> bool {
        if self.loc == Loc::Pc {
            return true;
        }
        if e.is_super_hidden() {
            return self.show_hidden && self.show_super_hidden;
        }
        !e.is_hidden() || self.show_hidden
    }

    pub fn folder(&self) -> &str {
        self.loc.path().unwrap_or("")
    }

    pub fn path_of(&self, i: u32) -> String {
        let e = &self.entries[i as usize];
        match &self.loc {
            Loc::Pc => e.name.to_string(),
            Loc::Dir(d) => e.path(d),
        }
    }

    pub fn selected(&self) -> Vec<u32> {
        self.sel.picked(&self.order)
    }

    pub fn selected_paths(&self) -> Vec<String> {
        self.selected().into_iter().map(|i| self.path_of(i)).collect()
    }

    /// Replaces the entries, keeping the selection and focus by name (a refresh), and sorts.
    pub fn set_entries(
        &mut self,
        entries: Vec<Entry>,
        keep: bool,
        sort: (Col, bool),
        types: &mut dyn FnMut(&Entry) -> Rc<str>,
    ) {
        let (picked, focus): (Vec<String>, Option<String>) = if keep {
            (
                self.selected().into_iter().map(|i| self.entries[i as usize].name.to_lowercase()).collect(),
                self.sel.focus.map(|f| self.entries[f as usize].name.to_lowercase()),
            )
        } else {
            (Vec::new(), None)
        };
        self.entries = entries;
        self.sel = Selection::new(self.entries.len());
        self.sort(sort, types);
        let wanted: Vec<String> = if self.pending_select.is_empty() {
            picked
        } else {
            std::mem::take(&mut self.pending_select).into_iter().map(|n| n.to_lowercase()).collect()
        };
        let focus = if wanted.is_empty() { focus } else { wanted.first().cloned() };
        if !wanted.is_empty() || focus.is_some() {
            for (i, e) in self.entries.iter().enumerate() {
                let lower = e.name.to_lowercase();
                if wanted.contains(&lower) {
                    self.sel.set(i as u32, true);
                }
                if focus.as_deref() == Some(lower.as_str()) {
                    self.sel.focus = Some(i as u32);
                }
            }
        }
    }

    /// search results streaming in: appended in the order found
    pub fn append(&mut self, more: Vec<Entry>) {
        let start = self.entries.len();
        self.entries.extend(more);
        self.sel.grow(self.entries.len());
        for i in start..self.entries.len() {
            if self.visible(&self.entries[i]) {
                self.order.push(i as u32);
            }
        }
    }

    pub fn sort(&mut self, (col, desc): (Col, bool), types: &mut dyn FnMut(&Entry) -> Rc<str>) {
        let mut order: Vec<u32> =
            (0..self.entries.len() as u32).filter(|&i| self.visible(&self.entries[i as usize])).collect();
        let entries = &self.entries;
        if self.loc == Loc::Pc {
            // drives stay in letter order
            order.sort_by(|&a, &b| entries[a as usize].name.cmp(&entries[b as usize].name));
        } else {
            let sizes = &self.sizes;
            let size_of =
                |e: &Entry| if e.is_dir() { sizes.get(&e.name.to_lowercase()).copied().unwrap_or(0) } else { e.size };
            let names: Vec<Rc<str>> =
                if col == Col::Type { entries.iter().map(|e| types(e)).collect() } else { Vec::new() };
            order.sort_by(|&a, &b| {
                let (x, y) = (&entries[a as usize], &entries[b as usize]);
                let o = y.is_dir().cmp(&x.is_dir()).then_with(|| match col {
                    Col::Modified => x.modified.cmp(&y.modified),
                    // folders by their measured size when there is one
                    Col::Size => size_of(x).cmp(&size_of(y)),
                    Col::Type => natsort::cmp(&names[a as usize], &names[b as usize]),
                    Col::Folder => natsort::cmp(x.dir.as_deref().unwrap_or(""), y.dir.as_deref().unwrap_or("")),
                    _ => Ordering::Equal,
                });
                let o = o.then_with(|| natsort::cmp(&x.name, &y.name));
                if desc {
                    o.reverse()
                } else {
                    o
                }
            });
        }
        self.order = order;
    }
}

/// reads a location on a thread; the result arrives as WM_LISTED
pub fn spawn_list(hwnd: HWND, generation: usize, loc: Loc, refresh: bool) {
    let h = hwnd.0 as isize;
    std::thread::spawn(move || {
        let t = Instant::now();
        let result = match &loc {
            Loc::Pc => Ok(fsx::drive_roots()
                .into_iter()
                .map(|r| Entry { name: r.into(), dir: None, attrs: FA_DIRECTORY, size: 0, modified: 0, created: 0 })
                .collect()),
            Loc::Dir(d) => fsx::list(d).map_err(|e| e.to_string()),
        };
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        post_box(HWND(h as *mut _), WM_LISTED, Listed { generation, loc, result, refresh, ms });
    });
}

/// labels, file systems and free space of every drive, each drive on its own thread so one slow drive does not hold
/// up the rest; the list arrives as WM_DRIVES
pub fn spawn_drives(hwnd: HWND) {
    let h = hwnd.0 as isize;
    std::thread::spawn(move || {
        let roots = fsx::drive_roots();
        let handles: Vec<_> = roots.into_iter().map(|r| std::thread::spawn(move || fsx::drive_info(&r))).collect();
        let drives: Vec<Drive> = handles.into_iter().filter_map(|t| t.join().ok()).collect();
        post_box(HWND(h as *mut _), WM_DRIVES, drives);
    });
}

/// Searches below `roots`. A drive (or a folder on it) with a saved index is answered from the index: names are
/// matched in memory and only the folders holding matches are read, for sizes and dates. The rest is walked on
/// several threads, and the drives walked get an index built afterwards (when `use_index`). Matches are posted in
/// batches (WM_SEARCH) about 12 times a second while the search runs; nothing runs once it has finished or `stop`
/// is set.
pub fn spawn_search(
    hwnd: HWND,
    generation: usize,
    roots: Vec<String>,
    text: String,
    stop: Arc<AtomicBool>,
    stats: Arc<Stats>,
    use_index: bool,
) {
    let h = hwnd.0 as isize;
    std::thread::spawn(move || {
        let q = Query::parse(&text);
        let batch: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
        let done = AtomicBool::new(false);
        let capped = AtomicBool::new(false);
        let found = AtomicU64::new(0);
        std::thread::scope(|s| {
            s.spawn(|| loop {
                let finished = done.load(AO::SeqCst);
                let entries = std::mem::take(&mut *batch.lock().unwrap());
                if !entries.is_empty() || finished {
                    let capped = capped.load(AO::SeqCst);
                    post_box(HWND(h as *mut _), WM_SEARCH, SearchBatch { generation, entries, done: finished, capped });
                }
                if finished {
                    break;
                }
                std::thread::sleep(Duration::from_millis(80));
            });
            let mut walk = Vec::new();
            for root in &roots {
                let indexed =
                    if use_index { dvfiles::index::shelf::search(root, &q, MAX_RESULTS as usize, &stop) } else { None };
                match indexed {
                    Some((hits, more, items, folders)) => {
                        stats.indexed.store(true, AO::Relaxed);
                        stats.entries.fetch_add(items, AO::Relaxed);
                        stats.dirs.fetch_add(folders, AO::Relaxed);
                        if more {
                            capped.store(true, AO::SeqCst);
                        }
                        found.fetch_add(hits.len() as u64, AO::Relaxed);
                        dvfiles::index::entries(hits, &q, &stop, &|b| batch.lock().unwrap().extend(b));
                    }
                    None => walk.push(root.clone()),
                }
            }
            walker::walk(
                &walk,
                walker::default_threads(),
                &stop,
                &stats,
                |dir, e| {
                    if q.matches(e.name, e.is_dir(), e.size) && q.fresh(e.modified) {
                        if found.fetch_add(1, AO::Relaxed) >= MAX_RESULTS {
                            capped.store(true, AO::SeqCst);
                            stop.store(true, AO::SeqCst);
                            return false;
                        }
                        batch.lock().unwrap().push(Entry::from_raw(e, Some(dir.clone())));
                    }
                    true
                },
                |_, _| {},
            );
            done.store(true, AO::SeqCst);
            // the next search on these drives can use an index
            if use_index && !stop.load(AO::Relaxed) {
                for r in &walk {
                    dvfiles::index::shelf::build_later(r);
                }
            }
        });
    });
}

/// Measures the given folders one after another (each with the parallel walker), posting each size as it is known.
/// Stops when `stop` is set; nothing runs after the last folder.
pub fn spawn_sizer(hwnd: HWND, generation: usize, folders: Vec<(String, String)>, stop: Arc<AtomicBool>) {
    let h = hwnd.0 as isize;
    std::thread::spawn(move || {
        let n = folders.len();
        for (k, (name, path)) in folders.into_iter().enumerate() {
            if stop.load(AO::Relaxed) {
                return;
            }
            let live = walker::LiveTotals::default();
            walker::measure(&[path.clone()], 4096, &stop, &live);
            if stop.load(AO::Relaxed) {
                return;
            }
            let bytes = live.get().bytes;
            post_box(HWND(h as *mut _), WM_SIZES, SizeResult { generation, name, path, bytes, last: k + 1 == n });
        }
    });
}
