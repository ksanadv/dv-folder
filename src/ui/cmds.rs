// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Every action, whether it comes from the command bar, a key or a context menu, and the menus themselves.
use crate::settings::{Col, ThemeMode, ViewMode};
use crate::ui::model::Loc;
use crate::ui::{clip, invalidate, navigate, refresh, shell, with_app, App};
use dvfiles::ops::{Job, Kind, Progress};
use std::sync::Arc;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Controls::EM_SETSEL;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_CONTROL, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Debug)]
pub enum Cmd {
    Back,
    Forward,
    Up,
    Refresh,
    Go(Loc),
    Open,
    OpenNewWindow,
    NewWindowAt(String),
    OpenWith,
    OpenLocation,
    RunAsAdmin,
    Cut,
    Copy,
    Paste,
    PasteInto,
    CopyPath,
    Rename,
    Delete,
    DeletePermanently,
    NewFolder,
    NewTextFile,
    Properties,
    PropertiesOf(String),
    SelectAll,
    SelectNone,
    InvertSelection,
    ToggleHidden,
    ToggleExt,
    ToggleFolderSizes,
    ToggleSearchIndex,
    TogglePreview,
    Undo,
    View(ViewMode),
    SortBy(Col),
    SortDesc(bool),
    Terminal,
    ShellMenu(i32, i32, bool),
    FocusAddress,
    FocusSearch,
    NewWindow,
    CloseWindow,
    NewTab,
    CloseTab,
    NextTab(i32),
    TabAt(usize),
    OpenInNewTab(String),
    Theme(ThemeMode),
    Pin(String),
    Unpin(String),
    DefaultHandler(bool),
    Settings,
    ChangedRecently,
    ShelfAdd,
    PutAway,
    ShelfHere(bool),
    ShelfClear,
    About,
}

// ---------------------------------------------------------------------------------------------------------------
// popup menus

pub(crate) const F_CHECK: u32 = 1;
pub(crate) const F_OFF: u32 = 2;
pub(crate) const F_DEFAULT: u32 = 4;

pub enum M {
    Item(u32, String, u32),
    Sep,
    Sub(String, Vec<M>),
}

#[derive(Default)]
pub struct Mb {
    cmds: Vec<Cmd>,
}

impl Mb {
    pub fn item(&mut self, label: &str, cmd: Cmd) -> M {
        self.with(label, cmd, 0)
    }

    pub fn with(&mut self, label: &str, cmd: Cmd, flags: u32) -> M {
        self.cmds.push(cmd);
        M::Item(self.cmds.len() as u32, label.to_string(), flags)
    }

    pub fn check(&mut self, label: &str, on: bool, cmd: Cmd) -> M {
        self.with(label, cmd, if on { F_CHECK } else { 0 })
    }

    pub fn when(&mut self, label: &str, enabled: bool, cmd: Cmd) -> M {
        self.with(label, cmd, if enabled { 0 } else { F_OFF })
    }
}

/// shows a menu at a screen point and returns what was picked: dvfiles' own menu (menu.rs)
pub fn popup(mb: Mb, items: Vec<M>, x: i32, y: i32) -> Option<Cmd> {
    let (owner, style) = with_app(|a| (a.main, crate::ui::menu::Style::of(a)))?;
    if !crate::ui::quiet() {
        unsafe {
            let _ = SetForegroundWindow(owner);
        }
    }
    let id = crate::ui::menu::track(owner, style, &items, x, y)?;
    mb.cmds.get((id as usize).checked_sub(1)?).cloned()
}

fn show(mb: Mb, items: Vec<M>, x: i32, y: i32) {
    if let Some(c) = popup(mb, items, x, y) {
        run(c);
    }
}

fn sort_items(mb: &mut Mb, a: &App) -> Vec<M> {
    let (col, desc) = a.sort_key();
    let mut v = vec![
        mb.check("Name", col == Col::Name, Cmd::SortBy(Col::Name)),
        mb.check("Date modified", col == Col::Modified, Cmd::SortBy(Col::Modified)),
        mb.check("Type", col == Col::Type, Cmd::SortBy(Col::Type)),
        mb.check("Size", col == Col::Size, Cmd::SortBy(Col::Size)),
    ];
    if a.model.search.is_some() {
        v.push(mb.check("Folder", col == Col::Folder, Cmd::SortBy(Col::Folder)));
    }
    v.push(M::Sep);
    v.push(mb.check("Ascending", !desc, Cmd::SortDesc(false)));
    v.push(mb.check("Descending", desc, Cmd::SortDesc(true)));
    v
}

fn view_items(mb: &mut Mb, a: &App) -> Vec<M> {
    let v = a.view();
    vec![
        mb.check("Large icons", v == ViewMode::Icons, Cmd::View(ViewMode::Icons)),
        mb.check("Details", v == ViewMode::Details, Cmd::View(ViewMode::Details)),
        M::Sep,
        mb.check("Hidden items", a.model.show_hidden, Cmd::ToggleHidden),
        mb.check("File name extensions", a.settings.show_ext, Cmd::ToggleExt),
        mb.check("Folder sizes", a.settings.folder_sizes, Cmd::ToggleFolderSizes),
        M::Sep,
        mb.check("Preview pane\tAlt+P", a.settings.preview, Cmd::TogglePreview),
    ]
}

pub fn sort_menu(x: i32, y: i32) {
    let mut mb = Mb::default();
    let Some(items) = with_app(|a| sort_items(&mut mb, a)) else { return };
    show(mb, items, x, y);
}

pub fn view_menu(x: i32, y: i32) {
    let mut mb = Mb::default();
    let Some(items) = with_app(|a| view_items(&mut mb, a)) else { return };
    show(mb, items, x, y);
}

pub fn new_menu(x: i32, y: i32) {
    let mut mb = Mb::default();
    let items = vec![mb.item("Folder\tCtrl+Shift+N", Cmd::NewFolder), mb.item("Text Document", Cmd::NewTextFile)];
    show(mb, items, x, y);
}

pub fn more_menu(x: i32, y: i32) {
    let mut mb = Mb::default();
    let Some(items) = with_app(|a| {
        let theme = a.settings.theme;
        let in_dir = a.model.loc.path().is_some();
        let is_default = crate::install::is_default_handler();
        let undo = a.undo.last().map(|u| u.label());
        vec![
            mb.when(&format!("{}\tCtrl+Z", undo.unwrap_or("Undo")), undo.is_some(), Cmd::Undo),
            M::Sep,
            mb.item("Select all\tCtrl+A", Cmd::SelectAll),
            mb.item("Select none", Cmd::SelectNone),
            mb.item("Invert selection\tCtrl+I", Cmd::InvertSelection),
            M::Sep,
            mb.when("Copy path\tCtrl+Shift+C", in_dir, Cmd::CopyPath),
            mb.when("Open in Terminal", in_dir, Cmd::Terminal),
            mb.item("Changed recently\tCtrl+Shift+R", Cmd::ChangedRecently),
            shelf_menu(&mut mb, a).unwrap_or(M::Sep),
            mb.item("New tab\tCtrl+T", Cmd::NewTab),
            mb.item("New window\tCtrl+N", Cmd::NewWindow),
            M::Sep,
            mb.check("Fast search index", a.settings.search_index, Cmd::ToggleSearchIndex),
            M::Sub(
                "Theme".into(),
                vec![
                    mb.check("Use system setting", theme == ThemeMode::System, Cmd::Theme(ThemeMode::System)),
                    mb.check("Light", theme == ThemeMode::Light, Cmd::Theme(ThemeMode::Light)),
                    mb.check("Dark", theme == ThemeMode::Dark, Cmd::Theme(ThemeMode::Dark)),
                ],
            ),
            mb.check("Open folders with dvfiles", is_default, Cmd::DefaultHandler(!is_default)),
            M::Sep,
            mb.item("Settings\tCtrl+,", Cmd::Settings),
            mb.item("About dvfiles", Cmd::About),
        ]
    }) else {
        return;
    };
    show(mb, items, x, y);
}

pub fn location_menu(locs: &[(String, Loc)], x: i32, y: i32) {
    if locs.is_empty() {
        return;
    }
    let mut mb = Mb::default();
    let items = locs.iter().map(|(l, loc)| mb.item(l, Cmd::Go(loc.clone()))).collect();
    show(mb, items, x, y);
}

/// the folders inside a breadcrumb, from its chevron
pub fn subfolder_menu(loc: &Loc, x: i32, y: i32) {
    let Some((show_hidden, drives)) = with_app(|a| (a.model.show_hidden, a.drives.clone())) else { return };
    let locs: Vec<(String, Loc)> = match loc {
        Loc::Pc => drives.iter().map(|d| (d.title(), Loc::Dir(d.root.clone()))).collect(),
        Loc::Dir(d) => {
            let mut v: Vec<String> = dvfiles::fsx::list(d)
                .unwrap_or_default()
                .into_iter()
                .filter(|e| e.is_dir() && (show_hidden || !e.is_hidden()) && !e.is_super_hidden())
                .map(|e| e.name.to_string())
                .collect();
            v.sort_by(|a, b| dvfiles::natsort::cmp(a, b));
            v.truncate(400);
            v.into_iter().map(|n| (n.clone(), Loc::Dir(dvfiles::pathx::join(d, &n)))).collect()
        }
    };
    location_menu(&locs, x, y);
}

pub fn recent_menu(x: i32, y: i32) {
    let Some(locs) = with_app(|a| {
        let name = |l: &Loc| match l {
            Loc::Pc => "This PC".to_string(),
            Loc::Dir(d) => a.display_name(d),
        };
        let mut v: Vec<(String, Loc)> = a.fwd.iter().map(|l| (name(l), l.clone())).collect();
        v.push((format!("{} (current)", name(&a.model.loc)), a.model.loc.clone()));
        v.extend(a.back.iter().rev().take(15).map(|l| (name(l), l.clone())));
        v
    }) else {
        return;
    };
    location_menu(&locs, x, y);
}

const RUNNABLE: [&str; 6] = ["exe", "bat", "cmd", "msi", "ps1", "com"];

/// the shelf's menu, while something is on it: put it all here, or let it go
fn shelf_menu(mb: &mut Mb, a: &App) -> Option<M> {
    if a.shelf.is_empty() {
        return None;
    }
    let here = a.model.loc.path().is_some() && a.model.search.is_none();
    Some(M::Sub(
        format!("Shelf ({})", a.shelf.len()),
        vec![
            mb.when("Copy here", here, Cmd::ShelfHere(false)),
            mb.when("Move here", here, Cmd::ShelfHere(true)),
            M::Sep,
            mb.item("Clear the shelf", Cmd::ShelfClear),
        ],
    ))
}

/// the shelf row's menu in the navigation pane
pub fn shelf_popup(x: i32, y: i32) {
    let mut mb = Mb::default();
    let Some(items) = with_app(|a| match shelf_menu(&mut mb, a) {
        Some(M::Sub(_, v)) => v,
        _ => vec![mb.when("Drop files here to gather them", false, Cmd::ShelfClear)],
    }) else {
        return;
    };
    show(mb, items, x, y);
}

pub fn item_menu(x: i32, y: i32) {
    let mut mb = Mb::default();
    let Some(items) = with_app(|a| {
        let sel = a.model.selected();
        let first = &a.model.entries[sel[0] as usize];
        let single = sel.len() == 1;
        let all_dirs = sel.iter().all(|&i| a.model.entries[i as usize].is_dir());
        let pc = a.model.loc == Loc::Pc;
        let ext = dvfiles::naming::ext_lower(&first.name);
        let mut v = vec![mb.with("Open", Cmd::Open, F_DEFAULT)];
        if single && all_dirs {
            v.push(mb.item("Open in new tab", Cmd::OpenInNewTab(a.model.path_of(sel[0]))));
            v.push(mb.item("Open in new window", Cmd::OpenNewWindow));
        }
        if a.model.search.is_some() {
            v.push(mb.item("Open file location", Cmd::OpenLocation));
        }
        if single && !all_dirs {
            v.push(mb.item("Open with...", Cmd::OpenWith));
            if RUNNABLE.contains(&ext.as_str()) {
                v.push(mb.item("Run as administrator", Cmd::RunAsAdmin));
            }
        }
        if !pc {
            v.push(M::Sep);
            v.push(mb.item("Cut\tCtrl+X", Cmd::Cut));
            v.push(mb.item("Copy\tCtrl+C", Cmd::Copy));
            if single && all_dirs && clip::has_files() {
                v.push(mb.item("Paste into folder", Cmd::PasteInto));
            }
            v.push(M::Sep);
            v.push(mb.item("Copy as path\tCtrl+Shift+C", Cmd::CopyPath));
            v.push(mb.item("Add to shelf", Cmd::ShelfAdd));
            if let Some(label) = put_away_label(a) {
                v.push(mb.item(&label, Cmd::PutAway));
            }
        }
        if single && all_dirs {
            let path = a.model.path_of(sel[0]);
            let pinned = a.settings.pins.iter().any(|p| dvfiles::pathx::same(p, &path));
            v.push(if pinned {
                mb.item("Unpin from Quick access", Cmd::Unpin(path.clone()))
            } else {
                mb.item("Pin to Quick access", Cmd::Pin(path.clone()))
            });
            v.push(mb.item("Open in Terminal", Cmd::Terminal));
        }
        if !pc {
            v.push(M::Sep);
            v.push(mb.item("Delete\tDel", Cmd::Delete));
            v.push(mb.item("Delete permanently\tShift+Del", Cmd::DeletePermanently));
            v.push(mb.item("Rename\tF2", Cmd::Rename));
        }
        v.push(M::Sep);
        v.push(mb.item("Properties\tAlt+Enter", Cmd::Properties));
        v.push(mb.item("Show more options", Cmd::ShellMenu(x, y, false)));
        v
    }) else {
        return;
    };
    show(mb, items, x, y);
}

pub fn background_menu(x: i32, y: i32) {
    let mut mb = Mb::default();
    let Some(items) = with_app(|a| {
        let in_dir = a.model.loc.path().is_some() && a.model.search.is_none();
        let mut v = vec![
            M::Sub("View".into(), view_items(&mut mb, a)),
            M::Sub("Sort by".into(), sort_items(&mut mb, a)),
            mb.item("Refresh\tF5", Cmd::Refresh),
        ];
        if let Some(u) = a.undo.last() {
            v.push(M::Sep);
            v.push(mb.item(&format!("{}\tCtrl+Z", u.label()), Cmd::Undo));
        }
        if in_dir {
            v.push(M::Sep);
            v.push(mb.when("Paste\tCtrl+V", clip::has_files(), Cmd::Paste));
            if let Some(s) = shelf_menu(&mut mb, a) {
                v.push(s);
            }
            v.push(M::Sep);
            v.push(M::Sub(
                "New".into(),
                vec![mb.item("Folder", Cmd::NewFolder), mb.item("Text Document", Cmd::NewTextFile)],
            ));
            v.push(M::Sep);
            v.push(mb.item("Open in Terminal", Cmd::Terminal));
        }
        v.push(mb.item("Select all", Cmd::SelectAll));
        if in_dir {
            v.push(M::Sep);
            v.push(mb.item("Properties", Cmd::Properties));
            v.push(mb.item("Show more options", Cmd::ShellMenu(x, y, true)));
        }
        v
    }) else {
        return;
    };
    show(mb, items, x, y);
}

pub fn nav_menu(path: &str, pinned: bool, x: i32, y: i32) {
    let mut mb = Mb::default();
    let items = vec![
        mb.with("Open", Cmd::Go(Loc::Dir(path.to_string())), F_DEFAULT),
        mb.item("Open in new tab", Cmd::OpenInNewTab(path.to_string())),
        mb.item("Open in new window", Cmd::NewWindowAt(path.to_string())),
        M::Sep,
        if pinned {
            mb.item("Unpin from Quick access", Cmd::Unpin(path.into()))
        } else {
            mb.item("Pin to Quick access", Cmd::Pin(path.into()))
        },
        M::Sep,
        mb.item("Properties", Cmd::PropertiesOf(path.to_string())),
    ];
    show(mb, items, x, y);
}

// ---------------------------------------------------------------------------------------------------------------
// actions

pub fn focus_list() {
    if let Some(l) = with_app(|a| a.list) {
        crate::ui::focus(l);
    }
}

fn main_hwnd() -> HWND {
    with_app(|a| a.main).unwrap_or_default()
}

pub struct JobDone {
    pub kind: Kind,
    pub dest: Option<String>,
    pub progress: Arc<Progress>,
    pub dialog: isize,
    pub clear_clipboard: Option<u32>,
}

/// runs a copy, move, delete or recycle on its own thread with a progress window
/// where a file in Downloads belongs: its kind's own user folder (Pictures for a photo), none for the rest
fn home_of(a: &App, name: &str, is_dir: bool) -> Option<String> {
    use crate::ui::art::Kind as K;
    if is_dir || !a.settings.inbox {
        return None;
    }
    let label = match crate::ui::art::kind_of(&dvfiles::naming::ext_lower(name)) {
        K::Image => "Pictures",
        K::Video => "Videos",
        K::Audio => "Music",
        K::Text | K::Doc | K::Sheet | K::Slides | K::Pdf => "Documents",
        _ => return None,
    };
    a.known.iter().find(|(k, _)| *k == label).map(|(_, p)| p.clone())
}

/// "Put away in Pictures" in Downloads' menu, when a selected file has a folder of its own kind
fn put_away_label(a: &App) -> Option<String> {
    let here = a.model.loc.path()?;
    if !a.known.iter().any(|(k, p)| *k == "Downloads" && dvfiles::pathx::same(p, here)) {
        return None;
    }
    let mut homes: Vec<&str> = Vec::new();
    for &i in &a.model.selected() {
        let e = &a.model.entries[i as usize];
        if let Some(h) = home_of(a, &e.name, e.is_dir()) {
            let label = a.known.iter().find(|(_, p)| *p == h).map_or("", |(k, _)| *k);
            if !homes.contains(&label) {
                homes.push(label);
            }
        }
    }
    (!homes.is_empty()).then(|| format!("Put away in {}", homes.join(", ")))
}

pub fn start_job(kind: Kind, sources: Vec<String>, dest: Option<String>, clear_clipboard: Option<u32>) {
    if sources.is_empty() {
        return;
    }
    let main = main_hwnd();
    let progress = Arc::new(Progress::default());
    let dialog = crate::ui::opsui::open(main, kind, &sources, dest.as_deref(), progress.clone());
    let job = Job { kind, sources, dest: dest.clone(), progress: progress.clone() };
    let main_raw = main.0 as isize;
    // copies that move data wait their turn: one at a time is quicker than several fighting over the disk
    let queued = with_app(|a| a.settings.copy_queue).unwrap_or(false)
        && match (kind, &job.dest) {
            (Kind::Copy, _) => true,
            (Kind::Move, Some(d)) => !dvfiles::fsx::same_volume(&job.sources[0], d),
            _ => false,
        };
    std::thread::spawn(move || {
        static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _turn = if queued {
            let mut waited = false;
            loop {
                if let Ok(g) = TURN.try_lock() {
                    break Some(g);
                }
                if job.progress.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    break None;
                }
                if !waited {
                    *job.progress.current.lock().unwrap() = "Waiting for the copy before it to finish".into();
                    waited = true;
                }
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
        } else {
            None
        };
        let asker = crate::ui::opsui::UiAsker { dialog };
        dvfiles::ops::run(&job, &asker);
        crate::ui::post_box(
            HWND(main_raw as *mut _),
            crate::ui::WM_JOB_DONE,
            JobDone { kind, dest, progress, dialog, clear_clipboard },
        );
    });
}

pub fn job_done(d: Box<JobDone>) {
    crate::ui::opsui::finished(d.dialog);
    // what the job did, so Ctrl+Z can put it back (only jobs that can be put back exactly)
    let log = d.progress.undo.lock().unwrap().clone();
    crate::trace::log(|| format!("job done {:?}: undo log {:?}", d.kind, log));
    if !log.inexact {
        let action = match d.kind {
            Kind::Move if !log.moved.is_empty() => Some(dvfiles::undo::Action::Moved(log.moved)),
            Kind::Copy if !log.created.is_empty() => Some(dvfiles::undo::Action::Copied(log.created)),
            Kind::Recycle if !log.recycled.is_empty() => Some(dvfiles::undo::Action::Recycled(log.recycled)),
            _ => None,
        };
        if let Some(a) = action {
            with_app(|app| app.push_undo(a));
        }
    }
    let names: Vec<String> =
        d.progress.results.lock().unwrap().iter().map(|p| dvfiles::pathx::file_name(p).to_string()).collect();
    let here = with_app(|a| {
        let into_here = match (&d.dest, &a.model.loc) {
            (Some(dest), Loc::Dir(cur)) => dvfiles::pathx::same(dest, cur),
            _ => false,
        };
        if into_here && matches!(d.kind, Kind::Copy | Kind::Move) {
            a.model.pending_select = names.clone();
        }
        a.main
    });
    if let (Some(seq), Some(main)) = (d.clear_clipboard, here) {
        if clip::sequence() == seq {
            clip::clear(main);
        }
        with_app(|a| a.cut = None);
    }
    refresh();
}

pub fn drop_into(paths: Vec<String>, target: String) {
    let (ctrl, shift) = (crate::ui::key_down(VK_CONTROL), crate::ui::key_down(VK_SHIFT));
    let kind = if ctrl {
        Kind::Copy
    } else if shift || dvfiles::fsx::same_volume(&paths[0], &target) {
        Kind::Move
    } else {
        Kind::Copy
    };
    let all_here = paths.iter().all(|p| dvfiles::pathx::parent(p).is_some_and(|d| dvfiles::pathx::same(&d, &target)));
    if kind == Kind::Move && all_here {
        return;
    }
    start_job(kind, paths, Some(target), None);
}

/// after a delete the item below the deleted ones gets the selection, as in Explorer
fn select_after_delete(a: &mut App) {
    let order = &a.model.order;
    let last = order.iter().rposition(|&e| a.model.sel.is(e));
    let next = last.and_then(|p| {
        order[p + 1..]
            .iter()
            .find(|&&e| !a.model.sel.is(e))
            .or_else(|| order[..p].iter().rev().find(|&&e| !a.model.sel.is(e)))
    });
    if let Some(&e) = next {
        a.model.pending_select = vec![a.model.entries[e as usize].name.to_string()];
    }
}

fn open_selection() {
    let Some((main, items, folder)) = with_app(|a| {
        let items: Vec<(String, bool)> =
            a.model.selected().iter().map(|&i| (a.model.path_of(i), a.model.entries[i as usize].is_dir())).collect();
        (a.main, items, a.model.folder().to_string())
    }) else {
        return;
    };
    if items.is_empty() {
        return;
    }
    if items.len() == 1 && items[0].1 {
        navigate(Loc::Dir(items[0].0.clone()), true);
        return;
    }
    // one picture: dvfiles' viewer, with the folder's other pictures in the list's order a key away
    if items.len() == 1 && !items[0].1 && dvfiles::img::readable(&dvfiles::naming::ext_lower(&items[0].0)) {
        let pics = with_app(|a| {
            a.settings.viewer.then(|| {
                a.model
                    .order
                    .iter()
                    .map(|&i| a.model.path_of(i))
                    .filter(|p| dvfiles::img::readable(&dvfiles::naming::ext_lower(p)))
                    .collect::<Vec<_>>()
            })
        })
        .flatten();
        if let Some(pics) = pics {
            let at = pics.iter().position(|p| *p == items[0].0).unwrap_or(0);
            crate::ui::viewer::open(pics, at);
            return;
        }
    }
    if items.len() > 15 && !shell::ask(main, &format!("Open {} items?", items.len())) {
        return;
    }
    for (path, is_dir) in items {
        if is_dir {
            shell::new_window(Some(&path));
            continue;
        }
        // a shortcut to a folder opens here rather than in Explorer
        if dvfiles::naming::ext_lower(&path) == "lnk" {
            if let Some(t) = shell::resolve_link(&path).filter(|t| dvfiles::fsx::is_dir(t)) {
                navigate(Loc::Dir(t), true);
                return;
            }
        }
        let dir = dvfiles::pathx::parent(&path).unwrap_or_else(|| folder.clone());
        shell::open(main, &path, &dir);
    }
}

fn create_new(folder_kind: bool) {
    let Some((main, folder)) = with_app(|a| (a.main, a.model.loc.path().map(String::from))) else { return };
    let Some(folder) = folder else { return };
    let (base, ext) = if folder_kind { ("New folder", "") } else { ("New Text Document", ".txt") };
    let name = dvfiles::naming::new_name(base, ext, |n| dvfiles::fsx::exists(&dvfiles::pathx::join(&folder, n)));
    let path = dvfiles::pathx::join(&folder, &name);
    let r = if folder_kind { std::fs::create_dir(&path) } else { std::fs::File::create_new(&path).map(|_| ()) };
    match r {
        Ok(()) => {
            with_app(|a| {
                a.model.pending_select = vec![name.clone()];
                a.model.pending_rename = true;
                a.push_undo(dvfiles::undo::Action::Created(vec![path.clone()]));
            });
            refresh();
        }
        Err(e) => shell::message(main, &format!("Could not create \"{name}\": {e}"), true),
    }
}

fn paste(into: Option<String>) {
    let Some((main, folder, as_file)) =
        with_app(|a| (a.main, a.model.loc.path().map(String::from), a.settings.paste_files))
    else {
        return;
    };
    let here = into.is_none();
    let Some(dest) = into.or(folder) else { return };
    match clip::get_files(main) {
        Some((files, cut)) => {
            let seq = cut.then(clip::sequence);
            start_job(if cut { Kind::Move } else { Kind::Copy }, files, Some(dest), seq);
        }
        None if as_file => paste_as_file(main, &dest, here),
        None => {}
    }
}

/// "2026-10-16 101530", the local time, for a pasted file's name
fn stamp() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:04}-{:02}-{:02} {:02}{:02}{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

/// A picture, web address or text on the clipboard (and no files) becomes a new file in `dest`: a PNG, an Internet
/// shortcut or a .txt, chosen and named by `dvfiles::pastefile`. Pasted into the folder shown, it is selected with
/// its name ready to change, as a new file is; Ctrl+Z takes it away again.
fn paste_as_file(main: HWND, dest: &str, here: bool) {
    use std::io::Write;
    let Some(clip) = clip::get_pasteable(main) else { return };
    let Some(made) = dvfiles::pastefile::make(&clip, &stamp()) else { return };
    let name =
        dvfiles::naming::new_name(&made.base, made.ext, |n| dvfiles::fsx::exists(&dvfiles::pathx::join(dest, n)));
    let path = dvfiles::pathx::join(dest, &name);
    match std::fs::File::create_new(&path).and_then(|mut f| f.write_all(&made.bytes)) {
        Ok(()) => {
            with_app(|a| {
                if here {
                    a.model.pending_select = vec![name.clone()];
                    a.model.pending_rename = true;
                }
                a.push_undo(dvfiles::undo::Action::Created(vec![path.clone()]));
            });
            refresh();
        }
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            shell::message(main, &format!("Could not paste \"{name}\": {e}"), true);
        }
    }
}

pub fn run(cmd: Cmd) {
    crate::trace::log(|| format!("run {cmd:?}"));
    match cmd {
        Cmd::Back => {
            let to = with_app(|a| {
                let to = a.back.pop()?;
                a.fwd.push(a.model.loc.clone());
                Some(to)
            })
            .flatten();
            if let Some(l) = to {
                navigate(l, false);
            }
        }
        Cmd::Forward => {
            let to = with_app(|a| {
                let to = a.fwd.pop()?;
                a.back.push(a.model.loc.clone());
                Some(to)
            })
            .flatten();
            if let Some(l) = to {
                navigate(l, false);
            }
        }
        Cmd::Up => {
            let up = with_app(|a| match &a.model.loc {
                Loc::Dir(d) => Some((
                    dvfiles::pathx::parent(d).map(Loc::Dir).unwrap_or(Loc::Pc),
                    dvfiles::pathx::file_name(d).to_string(),
                )),
                Loc::Pc => None,
            })
            .flatten();
            if let Some((l, from)) = up {
                with_app(|a| a.model.pending_select = vec![if l == Loc::Pc { format!("{from}\\") } else { from }]);
                navigate(l, true);
            }
        }
        Cmd::Refresh => {
            with_app(|a| {
                if let Loc::Dir(d) = &a.model.loc {
                    a.icons.forget_paths_under(d);
                    a.thumbs.forget_under(d);
                    // F5 measures this folder's subfolders again
                    let d = d.to_lowercase();
                    a.size_cache.retain(|p, _| !dvfiles::pathx::is_within(p, &d));
                }
            });
            refresh();
        }
        Cmd::Go(l) => navigate(l, true),
        Cmd::Open => open_selection(),
        Cmd::OpenNewWindow => {
            if let Some(p) = with_app(|a| a.model.selected_paths().into_iter().next()).flatten() {
                shell::new_window(Some(&p));
            }
        }
        Cmd::NewWindowAt(p) => shell::new_window(Some(&p)),
        Cmd::OpenWith => {
            if let Some((m, Some(p))) = with_app(|a| (a.main, a.model.selected_paths().into_iter().next())) {
                shell::open_with(m, &p);
            }
        }
        Cmd::RunAsAdmin => {
            if let Some((m, Some(p))) = with_app(|a| (a.main, a.model.selected_paths().into_iter().next())) {
                let dir = dvfiles::pathx::parent(&p).unwrap_or_default();
                shell::run_as_admin(m, &p, &dir);
            }
        }
        Cmd::OpenLocation => {
            if let Some(Some(p)) = with_app(|a| a.model.selected_paths().into_iter().next()) {
                if let Some(dir) = dvfiles::pathx::parent(&p) {
                    with_app(|a| a.model.pending_select = vec![dvfiles::pathx::file_name(&p).to_string()]);
                    crate::ui::set_search_text("");
                    navigate(Loc::Dir(dir), true);
                }
            }
        }
        Cmd::Cut | Cmd::Copy => {
            let cut = matches!(cmd, Cmd::Cut);
            with_app(|a| {
                let paths = a.model.selected_paths();
                if paths.is_empty() || a.model.loc == Loc::Pc {
                    return;
                }
                clip::set_files(a.main, &paths, cut);
                a.cut = cut.then(|| (clip::sequence(), paths.iter().map(|p| p.to_lowercase()).collect()));
                invalidate(a.list);
                invalidate(a.main);
            });
        }
        Cmd::Paste => paste(None),
        Cmd::PasteInto => {
            let into = with_app(|a| a.model.selected_paths().into_iter().next()).flatten();
            if into.is_some() {
                paste(into);
            }
        }
        Cmd::CopyPath => {
            with_app(|a| {
                let mut paths = a.model.selected_paths();
                if paths.is_empty() {
                    paths.extend(a.model.loc.path().map(String::from));
                }
                let text = paths.iter().map(|p| format!("\"{p}\"")).collect::<Vec<_>>().join("\r\n");
                clip::set_text(a.main, &text);
            });
        }
        Cmd::Rename => crate::ui::list::begin_rename(),
        Cmd::Delete => {
            if let Some((paths, false)) = with_app(|a| {
                select_after_delete(a);
                (a.model.selected_paths(), a.model.loc == Loc::Pc)
            }) {
                start_job(Kind::Recycle, paths, None, None);
            }
        }
        Cmd::DeletePermanently => {
            if let Some((main, paths, false)) = with_app(|a| (a.main, a.model.selected_paths(), a.model.loc == Loc::Pc))
            {
                if paths.is_empty() {
                    return;
                }
                let what = if paths.len() == 1 {
                    format!("\"{}\"", dvfiles::pathx::file_name(&paths[0]))
                } else {
                    format!("these {} items", paths.len())
                };
                let they = if paths.len() == 1 { "It" } else { "They" };
                if shell::ask(
                    main,
                    &format!(
                        "Are you sure you want to permanently delete {what}?\n\n{they} will not go to the Recycle Bin."
                    ),
                ) {
                    with_app(select_after_delete);
                    start_job(Kind::Delete, paths, None, None);
                }
            }
        }
        Cmd::NewFolder => create_new(true),
        Cmd::NewTextFile => create_new(false),
        Cmd::Properties => {
            let target = with_app(|a| {
                let sel = a.model.selected_paths();
                if !sel.is_empty() {
                    return Some((sel, a.model.loc == Loc::Pc));
                }
                a.model.loc.path().map(|p| (vec![p.to_string()], false))
            })
            .flatten();
            if let Some((paths, drives)) = target {
                crate::ui::props::open(paths, drives);
            }
        }
        Cmd::PropertiesOf(p) => {
            let drive = p.len() <= 3;
            crate::ui::props::open(vec![p], drive);
        }
        Cmd::SelectAll | Cmd::SelectNone | Cmd::InvertSelection => {
            with_app(|a| {
                let order = a.model.order.clone();
                match cmd {
                    Cmd::SelectAll => a.model.sel.all(&order),
                    Cmd::SelectNone => a.model.sel.clear(),
                    _ => a.model.sel.invert(&order),
                }
                invalidate(a.list);
                invalidate(a.main);
            });
        }
        Cmd::ToggleHidden => {
            with_app(|a| {
                a.model.show_hidden = !a.model.show_hidden;
                a.settings.show_hidden = Some(a.model.show_hidden);
                a.resort();
            });
        }
        Cmd::Undo => {
            let Some((main, action)) = with_app(|a| (a.main, a.undo.pop())) else { return };
            let Some(action) = action else { return };
            let problems = dvfiles::undo::undo(&action);
            crate::trace::log(|| format!("undo {action:?}: problems {problems:?}"));
            if !problems.is_empty() {
                let lines: Vec<String> = problems
                    .iter()
                    .take(8)
                    .map(|(p, why)| format!("{}: {why}", dvfiles::pathx::file_name(p)))
                    .collect();
                let more =
                    if problems.len() > 8 { format!("\nand {} more", problems.len() - 8) } else { String::new() };
                shell::message(
                    main,
                    &format!("{} could not put everything back:\n\n{}{more}", action.label(), lines.join("\n")),
                    true,
                );
            }
            refresh();
        }
        Cmd::ToggleFolderSizes => {
            with_app(|a| {
                a.settings.folder_sizes = !a.settings.folder_sizes;
                if a.settings.folder_sizes {
                    a.start_sizer();
                } else {
                    a.stop_sizer();
                    a.model.sizes.clear();
                    a.model.sizes_pending = 0;
                }
                a.resort();
            });
        }
        Cmd::TogglePreview => {
            with_app(|a| {
                a.settings.preview = !a.settings.preview;
                a.pv.subject = String::from("\u{0}");
            });
            crate::ui::layout();
        }
        Cmd::ToggleSearchIndex => {
            with_app(|a| {
                a.settings.search_index = !a.settings.search_index;
                if !a.settings.search_index {
                    // nothing of it stays behind
                    dvfiles::index::shelf::forget();
                }
                a.settings.save();
            });
        }
        Cmd::ToggleExt => {
            with_app(|a| {
                a.settings.show_ext = !a.settings.show_ext;
                invalidate(a.list);
            });
        }
        Cmd::View(v) => {
            with_app(|a| {
                if a.model.loc == Loc::Pc {
                    return;
                }
                a.settings.view = v;
                a.lv.scroll_y = 0;
                a.lv.glide = None;
                a.ensure_visible_focus();
                a.after_change();
            });
        }
        Cmd::SortBy(c) => {
            with_app(|a| {
                if a.settings.sort == c {
                    a.settings.sort_desc = !a.settings.sort_desc;
                } else {
                    a.settings.sort = c;
                    a.settings.sort_desc = false;
                }
                a.resort();
            });
        }
        Cmd::SortDesc(d) => {
            with_app(|a| {
                a.settings.sort_desc = d;
                a.resort();
            });
        }
        Cmd::Terminal => {
            let dir = with_app(|a| {
                let sel = a.model.selected();
                if sel.len() == 1 && a.model.entries[sel[0] as usize].is_dir() {
                    Some(a.model.path_of(sel[0]))
                } else {
                    a.model.loc.path().map(String::from)
                }
            })
            .flatten();
            if let Some(d) = dir {
                shell::terminal(&d);
            }
        }
        Cmd::ShellMenu(x, y, background) => {
            let Some((main, folder, items)) = with_app(|a| {
                let items = if background { Vec::new() } else { a.model.selected_paths() };
                let folder = items
                    .first()
                    .and_then(|p| dvfiles::pathx::parent(p))
                    .or_else(|| a.model.loc.path().map(String::from))
                    .unwrap_or_default();
                // the shell menu needs items from one folder
                let items: Vec<String> = items
                    .into_iter()
                    .filter(|p| dvfiles::pathx::parent(p).is_some_and(|d| dvfiles::pathx::same(&d, &folder)))
                    .collect();
                (a.main, folder, items)
            }) else {
                return;
            };
            if !folder.is_empty() || !items.is_empty() {
                shell::context_menu(main, &folder, &items, x, y, crate::ui::key_down(VK_SHIFT));
            }
        }
        Cmd::FocusAddress => crate::ui::bar::begin_address_edit(),
        Cmd::FocusSearch => {
            let (_, s) = crate::ui::ensure_edits();
            if !s.0.is_null() {
                unsafe {
                    crate::ui::focus(s);
                    SendMessageW(
                        s,
                        EM_SETSEL,
                        Some(windows::Win32::Foundation::WPARAM(0)),
                        Some(windows::Win32::Foundation::LPARAM(-1)),
                    );
                }
            }
        }
        Cmd::NewWindow => {
            let p = with_app(|a| a.model.loc.path().map(String::from)).flatten();
            shell::new_window(p.as_deref());
        }
        Cmd::NewTab => crate::ui::tabs::open(Loc::Pc, true),
        Cmd::CloseTab => {
            let active = with_app(|a| a.tabs.active).unwrap_or(0);
            crate::ui::tabs::close(active);
        }
        Cmd::NextTab(d) => crate::ui::tabs::step(d),
        Cmd::TabAt(i) => {
            // Ctrl+9 is the last tab, as in browsers
            let n = with_app(|a| a.tabs.list.len()).unwrap_or(0);
            if n > 1 {
                crate::ui::tabs::switch(if i == 8 { n - 1 } else { i });
            }
        }
        Cmd::OpenInNewTab(p) => crate::ui::tabs::open(Loc::Dir(p), false),
        Cmd::CloseWindow => unsafe {
            let _ = PostMessageW(Some(main_hwnd()), WM_CLOSE, Default::default(), Default::default());
        },
        Cmd::Theme(m) => crate::ui::set_theme(m),
        Cmd::Pin(p) => {
            with_app(|a| {
                if !a.settings.pins.iter().any(|x| dvfiles::pathx::same(x, &p)) {
                    a.settings.pins.push(p.clone());
                    a.navs.add_pin(&p);
                    a.settings.save();
                    invalidate(a.nav);
                }
            });
        }
        Cmd::Unpin(p) => {
            with_app(|a| {
                a.settings.pins.retain(|x| !dvfiles::pathx::same(x, &p));
                a.navs.remove_pin(&p);
                a.settings.save();
                invalidate(a.nav);
            });
        }
        Cmd::DefaultHandler(on) => {
            let main = main_hwnd();
            let r = std::env::current_exe()
                .map_err(|e| e.to_string())
                .and_then(|exe| crate::install::set_default_handler(&exe, on));
            match r {
                Ok(()) if on => shell::message(
                    main,
                    "Folders now open in dvfiles: double-clicking a folder on the desktop, in a shortcut or in another program.\n\nWin+E and the taskbar's File Explorer button still open Explorer. Turn this off again from the same menu.",
                    false,
                ),
                Ok(()) => shell::message(main, "Folders open in File Explorer again.", false),
                Err(e) => shell::message(main, &e, true),
            }
        }
        Cmd::Settings => crate::ui::prefs::open(),
        Cmd::PutAway => {
            let Some(groups) = with_app(|a| {
                let mut groups: Vec<(String, Vec<String>)> = Vec::new();
                for &i in &a.model.selected() {
                    let e = &a.model.entries[i as usize];
                    let Some(home) = home_of(a, &e.name, e.is_dir()) else { continue };
                    let path = a.model.path_of(i);
                    match groups.iter_mut().find(|(h, _)| *h == home) {
                        Some((_, v)) => v.push(path),
                        None => groups.push((home, vec![path])),
                    }
                }
                groups
            }) else {
                return;
            };
            // one job a folder; with "Copy one at a time" they follow each other
            for (home, items) in groups {
                start_job(Kind::Move, items, Some(home), None);
            }
        }
        Cmd::ShelfAdd => {
            with_app(|a| {
                for p in a.model.selected_paths() {
                    if !a.shelf.iter().any(|s| dvfiles::pathx::same(s, &p)) {
                        a.shelf.push(p);
                    }
                }
                invalidate(a.main);
                invalidate(a.nav);
            });
        }
        Cmd::ShelfHere(is_move) => {
            let Some((items, dest)) = with_app(|a| {
                let items: Vec<String> = a.shelf.iter().filter(|p| dvfiles::fsx::exists(p)).cloned().collect();
                if is_move {
                    a.shelf.clear();
                    invalidate(a.main);
                    invalidate(a.nav);
                }
                (items, a.model.loc.path().map(String::from))
            }) else {
                return;
            };
            start_job(if is_move { Kind::Move } else { Kind::Copy }, items, dest, None);
        }
        Cmd::ShelfClear => {
            with_app(|a| {
                a.shelf.clear();
                invalidate(a.main);
                invalidate(a.nav);
            });
        }
        Cmd::ChangedRecently => {
            // the files changed in the last week below this folder (your own folder, from This PC), newest first
            if with_app(|a| a.model.loc.path().is_none()) == Some(true) {
                if let Some(home) = std::env::var_os("USERPROFILE") {
                    crate::ui::navigate(Loc::Dir(home.to_string_lossy().into_owned()), true);
                }
            }
            with_app(|a| {
                a.settings.sort = Col::Modified;
                a.settings.sort_desc = true;
            });
            let q = "modified:7d type:file";
            crate::ui::set_search_text(q);
            crate::ui::start_search(q.to_string());
        }
        Cmd::About => {
            let main = main_hwnd();
            shell::message(
                main,
                &format!(
                    "dvfiles {}\n\nA fast Windows 10 style file manager in Rust.\nGPL-3.0-or-later.\n\nSettings: {}",
                    env!("CARGO_PKG_VERSION"),
                    crate::settings::dir().map(|d| d.join("settings.txt").display().to_string()).unwrap_or_default()
                ),
                false,
            );
        }
    }
}
