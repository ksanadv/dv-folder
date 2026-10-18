// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Tabs: several folders in one window. The strip above the toolbar appears only once there are two, so a window
//! with one folder looks as it always did. A tab in the background keeps only where it is (its history, selection
//! and scroll position), not its listing: switching back reads the folder again, which takes milliseconds, and an
//! idle window with ten tabs holds as little as one with one.
use crate::ui::model::Loc;
use crate::ui::{invalidate, navigate, with_app, App};

#[derive(Clone)]
pub struct Tab {
    pub loc: Loc,
    pub back: Vec<Loc>,
    pub fwd: Vec<Loc>,
    pub scroll: i32,
    // selected items by name, the focused one first
    pub picked: Vec<String>,
    pub title: String,
}

#[derive(Default)]
pub struct Tabs {
    pub list: Vec<Tab>,
    pub active: usize,
    // tabs opened behind the shown one since it was shown: the next goes after them, in the order they were opened
    run: usize,
}

impl Tab {
    pub fn at(loc: Loc, title: String) -> Tab {
        Tab { loc, back: Vec::new(), fwd: Vec::new(), scroll: 0, picked: Vec::new(), title }
    }
}

/// the strip's height: none for a single tab
pub fn strip_h(a: &App) -> i32 {
    if a.tabs.list.len() > 1 {
        a.s(36.0)
    } else {
        0
    }
}

/// the shown tab as it is now
fn snapshot(a: &App) -> Tab {
    let m = &a.model;
    let mut picked: Vec<String> = Vec::new();
    if let Some(f) = m.sel.focus.filter(|&f| m.sel.is(f)) {
        picked.push(m.entries[f as usize].name.to_string());
    }
    for i in m.selected() {
        let n = m.entries[i as usize].name.to_string();
        if !picked.contains(&n) {
            picked.push(n);
        }
    }
    Tab {
        loc: m.loc.clone(),
        back: a.back.clone(),
        fwd: a.fwd.clone(),
        scroll: a.lv.scroll_y,
        picked,
        title: a.title_text(),
    }
}

/// shows tab `t` in the window (the tab list already says it is the active one)
fn show(t: Tab) {
    with_app(|a| {
        a.back = t.back;
        a.fwd = t.fwd;
        a.model.pending_select = t.picked;
        a.lv.restore_scroll = Some(t.scroll);
    });
    navigate(t.loc, false);
    crate::ui::layout();
}

/// a new tab after the shown one; `switch` makes it the shown one (Ctrl+T), otherwise it opens behind (a middle
/// click on a folder)
pub fn open(loc: Loc, switch: bool) {
    let shown = with_app(|a| {
        let title = match &loc {
            Loc::Pc => "This PC".to_string(),
            Loc::Dir(d) => a.display_name(d),
        };
        let now = snapshot(a);
        if a.tabs.list.is_empty() {
            a.tabs.list.push(now);
        } else {
            a.tabs.list[a.tabs.active] = now;
        }
        let at = if switch { a.tabs.active + 1 } else { (a.tabs.active + 1 + a.tabs.run).min(a.tabs.list.len()) };
        a.tabs.list.insert(at, Tab::at(loc.clone(), title));
        if switch {
            a.tabs.active = at;
            a.tabs.run = 0;
            Some(a.tabs.list[at].clone())
        } else {
            a.tabs.run += 1;
            a.bar.dirty = true;
            invalidate(a.main);
            None
        }
    });
    match shown {
        Some(Some(t)) => show(t),
        Some(None) => crate::ui::layout(),
        None => {}
    }
}

pub fn switch(i: usize) {
    let target = with_app(|a| {
        if i == a.tabs.active || i >= a.tabs.list.len() {
            return None;
        }
        a.tabs.list[a.tabs.active] = snapshot(a);
        a.tabs.active = i;
        a.tabs.run = 0;
        Some(a.tabs.list[i].clone())
    })
    .flatten();
    if let Some(t) = target {
        show(t);
    }
}

/// the next (or previous) tab, wrapping around
pub fn step(delta: i32) {
    let i = with_app(|a| {
        let n = a.tabs.list.len() as i32;
        (n > 1).then(|| ((a.tabs.active as i32 + delta).rem_euclid(n)) as usize)
    })
    .flatten();
    if let Some(i) = i {
        switch(i);
    }
}

/// Closes a tab; closing the last one closes the window.
pub fn close(i: usize) {
    enum Next {
        Window,
        Show(Tab),
        Relayout,
    }
    let next = with_app(|a| {
        if a.tabs.list.len() <= 1 {
            return Next::Window;
        }
        if i >= a.tabs.list.len() {
            return Next::Relayout;
        }
        a.tabs.list.remove(i);
        a.tabs.run = 0;
        if i == a.tabs.active {
            a.tabs.active = i.min(a.tabs.list.len() - 1);
            return Next::Show(a.tabs.list[a.tabs.active].clone());
        }
        if i < a.tabs.active {
            a.tabs.active -= 1;
        }
        Next::Relayout
    });
    match next {
        Some(Next::Window) => crate::ui::cmds::run(crate::ui::cmds::Cmd::CloseWindow),
        Some(Next::Show(t)) => show(t),
        Some(Next::Relayout) => crate::ui::layout(),
        None => {}
    }
}

/// the shown tab's label follows the folder shown
pub fn retitle(a: &mut App) {
    let t = a.title_text();
    let active = a.tabs.active;
    if let Some(tab) = a.tabs.list.get_mut(active) {
        if tab.title != t {
            tab.title = t;
            a.bar.dirty = true;
        }
    }
}
