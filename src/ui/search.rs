// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Searching: starting a search below the folder shown and taking its results in batches.
use super::*;

pub fn start_search(text: String) {
    let text = text.trim().to_string();
    if text.is_empty() {
        let loc = with_app(|a| {
            a.stop_search();
            a.model.search.take().map(|_| a.model.loc.clone())
        })
        .flatten();
        if let Some(loc) = loc {
            navigate(loc, false);
        }
        return;
    }
    // typing more of a word only narrows a finished search: pick the results out of it instead of walking again
    let narrowed = with_app(|a| {
        let s = a.model.search.as_mut()?;
        if s.running || s.capped {
            return None;
        }
        let (new_q, old_q) = (dvfiles::query::Query::parse(&text), dvfiles::query::Query::parse(&s.text));
        if !new_q.narrows(&old_q) {
            return None;
        }
        s.text = text.clone();
        let entries: Vec<dvfiles::fsx::Entry> = std::mem::take(&mut a.model.entries)
            .into_iter()
            .filter(|e| new_q.matches_str(&e.name, e.is_dir(), e.size))
            .collect();
        a.model.sel = dvfiles::selection::Selection::new(entries.len());
        a.model.entries = entries;
        a.lv.scroll_y = 0;
        a.lv.glide = None;
        a.resort();
        a.bar.dirty = true;
        Some(())
    })
    .flatten();
    if narrowed.is_some() {
        update_title();
        return;
    }
    with_app(|a| {
        a.stop_search();
        a.stop_sizer();
        a.model.sizes_pending = 0;
        a.generation += 1;
        let roots = match &a.model.loc {
            Loc::Dir(d) => vec![d.clone()],
            Loc::Pc => a
                .drives
                .iter()
                .filter(|d| d.kind == dvfiles::fsx::DriveKind::Fixed && d.ready)
                .map(|d| d.root.clone())
                .collect(),
        };
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stats = std::sync::Arc::new(dvfiles::walker::Stats::default());
        a.model.search = Some(model::Search {
            text: text.clone(),
            stop: stop.clone(),
            running: true,
            started: std::time::Instant::now(),
            elapsed: Default::default(),
            stats: stats.clone(),
            capped: false,
        });
        a.model.entries.clear();
        a.model.order.clear();
        a.model.sel = dvfiles::selection::Selection::new(0);
        a.model.error = None;
        a.model.loading = false;
        a.lv.scroll_y = 0;
        a.lv.glide = None;
        a.watcher = None;
        model::spawn_search(a.main, a.generation, roots, text, stop, stats, a.settings.search_index);
        set_busy(a, true);
        a.bar.dirty = true;
        a.after_change();
    });
    update_title();
}

pub(super) fn on_search(b: Box<model::SearchBatch>) {
    with_app(|a| {
        if b.generation != a.generation {
            return;
        }
        let Some(s) = &mut a.model.search else { return };
        if b.done {
            s.running = false;
            s.elapsed = s.started.elapsed();
            s.capped = b.capped;
            set_busy(a, false);
        }
        a.model.append(b.entries);
        if b.done {
            a.resort();
        }
        a.after_change();
    });
}
