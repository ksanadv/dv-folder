// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! dvfiles-cli: the engine without a window, for scripts and AI agents (and for timing the engine on its own).
use dvfiles::fmt::{self, NumFmt};
use dvfiles::fsx::{self, Entry};
use dvfiles::ops::{self, Always, Job, Kind, Progress, Resolution};
use dvfiles::query::Query;
use dvfiles::walker::{self, Stats};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const USAGE: &str = "dvfiles-cli: list, search, size, copy, move, recycle and delete from the command line

  dvfiles-cli ls <folder> [--json]
  dvfiles-cli search <folder> <query...> [--limit N] [--json]
        query: words (any order, any case), *wildcards*, \"quoted words\", -exclude,
               ext:pdf;docx  type:folder|file  size:>10MB  size:1MB..1GB
  dvfiles-cli size <path...> [--json]
  dvfiles-cli copy <source...> <destination folder> [--replace | --skip | --keep-both]
  dvfiles-cli move <source...> <destination folder> [--replace | --skip | --keep-both]
  dvfiles-cli recycle <path...>
  dvfiles-cli delete <path...> --yes       permanent, no Recycle Bin
  dvfiles-cli index <drive> [folder] [query...] [--rebuild]
        the search index: built on first use, caught up from the change journal, then searched

Conflicts are skipped unless --replace or --keep-both is given. Times go to stderr.";

fn json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn entry_json(e: &Entry, folder: &str) -> String {
    format!(
        "{{\"path\":{},\"name\":{},\"dir\":{},\"size\":{},\"modified\":{},\"attributes\":{}}}",
        json_str(&e.path(folder)),
        json_str(&e.name),
        e.is_dir(),
        e.size,
        json_str(&fmt::iso(e.modified)),
        e.attrs
    )
}

fn fail(msg: &str) -> ! {
    eprintln!("dvfiles-cli: {msg}");
    std::process::exit(1)
}

fn ls(args: &[String], json: bool) {
    let dir = fsx::full_path(args.first().unwrap_or_else(|| fail("ls needs a folder")));
    let t = Instant::now();
    let mut entries = fsx::list(&dir).unwrap_or_else(|e| fail(&format!("{dir}: {e}")));
    let read = t.elapsed();
    entries.sort_by(|a, b| b.is_dir().cmp(&a.is_dir()).then_with(|| dvfiles::natsort::cmp(&a.name, &b.name)));
    let sorted = t.elapsed();
    let nf = NumFmt::default();
    let out = std::io::stdout();
    let mut out = std::io::BufWriter::new(out.lock());
    for e in &entries {
        if json {
            let _ = writeln!(out, "{}", entry_json(e, &dir));
        } else {
            let size = if e.is_dir() { "<DIR>".to_string() } else { nf.group(e.size) };
            let _ = writeln!(out, "{}  {:>15}  {}", fmt::iso(e.modified), size, e.name);
        }
    }
    let _ = out.flush();
    eprintln!("{} entries, read in {:.2} ms, sorted by {:.2} ms", entries.len(), ms(read), ms(sorted));
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn search(args: &[String], json: bool) {
    let mut rest: Vec<&String> = Vec::new();
    let mut limit = u64::MAX;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--limit" {
            limit = it.next().and_then(|n| n.parse().ok()).unwrap_or_else(|| fail("--limit needs a number"));
        } else {
            rest.push(a);
        }
    }
    let Some((root, words)) = rest.split_first() else { fail("search needs a folder and a query") };
    let root = fsx::full_path(root);
    let text = words.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ");
    let q = Query::parse(&text);
    let t = Instant::now();
    let stop = AtomicBool::new(false);
    let stats = Stats::default();
    let found = AtomicU64::new(0);
    let out = Mutex::new(std::io::BufWriter::new(std::io::stdout()));
    walker::walk(
        &[root.clone()],
        walker::default_threads(),
        &stop,
        &stats,
        |dir, e| {
            if q.matches(e.name, e.is_dir(), e.size) {
                let n = found.fetch_add(1, Ordering::Relaxed);
                if n < limit {
                    let entry = Entry::from_raw(e, Some(dir.clone()));
                    let mut o = out.lock().unwrap();
                    if json {
                        let _ = writeln!(o, "{}", entry_json(&entry, ""));
                    } else {
                        let _ = writeln!(o, "{}", entry.path(""));
                    }
                }
                if n + 1 >= limit {
                    stop.store(true, Ordering::Relaxed);
                }
            }
            true
        },
        |_, _| {},
    );
    let _ = out.lock().unwrap().flush();
    eprintln!(
        "{} matches in {} entries / {} folders, {:.0} ms ({} unreadable)",
        found.load(Ordering::Relaxed).min(limit),
        stats.entries.load(Ordering::Relaxed),
        stats.dirs.load(Ordering::Relaxed),
        ms(t.elapsed()),
        stats.errors.load(Ordering::Relaxed)
    );
}

fn size(args: &[String], json: bool) {
    let nf = NumFmt::default();
    for a in args {
        let path = fsx::full_path(a);
        let t = Instant::now();
        let e = fsx::stat(&path).unwrap_or_else(|e| fail(&format!("{path}: {e}")));
        let totals = if e.is_dir() {
            let live = walker::LiveTotals::default();
            let root = fsx::volume_root(&path).unwrap_or_default();
            walker::measure(&[path.clone()], fsx::cluster_size(&root), &AtomicBool::new(false), &live);
            live.get()
        } else {
            walker::Totals { files: 1, dirs: 0, bytes: e.size, on_disk: e.size }
        };
        if json {
            println!(
                "{{\"path\":{},\"bytes\":{},\"on_disk\":{},\"files\":{},\"folders\":{}}}",
                json_str(&path),
                totals.bytes,
                totals.on_disk,
                totals.files,
                totals.dirs
            );
        } else {
            println!(
                "{path}\n  {}, {} files, {} folders",
                nf.bytes_exact(totals.bytes),
                nf.group(totals.files),
                nf.group(totals.dirs)
            );
        }
        eprintln!("  {:.0} ms", ms(t.elapsed()));
    }
}

fn transfer_or_remove(kind: Kind, args: &[String]) {
    let mut policy = Resolution::Skip;
    let mut yes = false;
    let mut paths: Vec<String> = Vec::new();
    for a in args {
        match a.as_str() {
            "--replace" => policy = Resolution::Replace,
            "--skip" => policy = Resolution::Skip,
            "--keep-both" => policy = Resolution::KeepBoth,
            "--yes" => yes = true,
            _ => paths.push(fsx::full_path(a)),
        }
    }
    let dest = match kind {
        Kind::Copy | Kind::Move => {
            if paths.len() < 2 {
                fail("give one or more sources and a destination folder");
            }
            let d = paths.pop();
            if !fsx::is_dir(d.as_deref().unwrap()) {
                fail("the destination is not a folder");
            }
            d
        }
        Kind::Delete if !yes => fail("delete is permanent; add --yes (or use recycle)"),
        _ => None,
    };
    if paths.is_empty() {
        fail("nothing to do");
    }
    let job = Job { kind, sources: paths, dest, progress: Arc::new(Progress::default()) };
    let p = job.progress.clone();
    let t = Instant::now();
    // the progress line every 250 ms, ending the moment the job does
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let ticker = {
        let p = p.clone();
        std::thread::spawn(move || {
            let nf = NumFmt::default();
            while let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = stop_rx.recv_timeout(Duration::from_millis(250))
            {
                let (db, tb) = (p.done_bytes.load(Ordering::Relaxed), p.total_bytes.load(Ordering::Relaxed));
                let (di, ti) = (p.done_items.load(Ordering::Relaxed), p.total_items.load(Ordering::Relaxed));
                eprint!("\r  {} / {} items, {} / {}      ", nf.group(di), nf.group(ti), nf.bytes(db), nf.bytes(tb));
            }
        })
    };
    ops::run(&job, &Always(policy));
    drop(stop_tx);
    let _ = ticker.join();
    let secs = t.elapsed().as_secs_f64();
    let nf = NumFmt::default();
    let bytes = p.done_bytes.load(Ordering::Relaxed);
    eprintln!(
        "\r{} {} items, {} in {:.2} s ({})            ",
        match kind {
            Kind::Copy => "copied",
            Kind::Move => "moved",
            Kind::Delete => "deleted",
            Kind::Recycle => "recycled",
        },
        nf.group(p.done_items.load(Ordering::Relaxed)),
        nf.bytes(bytes),
        secs,
        nf.speed(bytes as f64 / secs.max(1e-6))
    );
    let errors = p.errors.lock().unwrap();
    for (path, e) in errors.iter() {
        eprintln!("  {path}: {e}");
    }
    if !errors.is_empty() {
        std::process::exit(2);
    }
}

/// the search index: load it (or build it), catch up with the change journal, then search; times go to stderr
fn index(args: &[String], json: bool) {
    use dvfiles::index::{file_for, Index};
    let root = args.first().map(String::as_str).unwrap_or_else(|| fail("index needs a drive, like C:"));
    let rebuild = args.iter().any(|a| a == "--rebuild");
    let words: Vec<String> = args[1..].iter().filter(|a| *a != "--rebuild").cloned().collect();
    let file = file_for(root).unwrap_or_else(|| fail("no LOCALAPPDATA"));
    let stop = AtomicBool::new(false);
    let ms = |t: Instant| t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let loaded = if rebuild { None } else { Index::load(&file).ok() };
    let mut ix = match loaded {
        Some(ix) => {
            eprintln!("loaded {} in {:.0} ms", file.display(), ms(t));
            ix
        }
        None => {
            let ix = Index::build(root, &stop).unwrap_or_else(|e| fail(&format!("{root}: {e}")));
            eprintln!("built in {:.0} ms", ms(t));
            ix
        }
    };
    let t = Instant::now();
    match ix.refresh(&stop) {
        Ok(n) => eprintln!("caught up with the change journal in {:.1} ms ({n} folders read again)", ms(t)),
        Err(e) => {
            eprintln!("journal: {e}; building again");
            let t = Instant::now();
            ix = Index::build(root, &stop).unwrap_or_else(|e| fail(&format!("{root}: {e}")));
            eprintln!("built in {:.0} ms", ms(t));
        }
    }
    let (dirs, items, bytes, heap) = ix.stats();
    eprintln!(
        "{dirs} folders, {items} items, {:.1} MB of records, {:.1} MB on the heap",
        bytes as f64 / 1048576.0,
        heap as f64 / 1048576.0
    );
    if ix.dirty {
        let t = Instant::now();
        ix.save(&file).unwrap_or_else(|e| fail(&format!("{}: {e}", file.display())));
        eprintln!(
            "saved in {:.0} ms ({:.1} MB)",
            ms(t),
            std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0) as f64 / 1048576.0
        );
    }
    if words.is_empty() {
        return;
    }
    let (scope, words) = match words.first() {
        Some(w) if w.contains('\\') => (Some(fsx::full_path(w)), &words[1..]),
        _ => (None, &words[..]),
    };
    let q = Query::parse(&words.join(" "));
    let t = Instant::now();
    let (hits, capped, n, f) = ix.search(&q, scope.as_deref(), 100_000);
    eprintln!(
        "{} names match{} among {n} items in {f} folders in {:.1} ms",
        hits.len(),
        if capped { " (capped)" } else { "" },
        ms(t)
    );
    let t = Instant::now();
    let found: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
    dvfiles::index::entries(hits, &q, &stop, &|b| found.lock().unwrap().extend(b));
    let found = found.into_inner().unwrap();
    eprintln!("{} results with sizes and dates in {:.1} ms", found.len(), ms(t));
    let out = std::io::stdout();
    let mut out = std::io::BufWriter::new(out.lock());
    for e in found.iter().take(if json { usize::MAX } else { 20 }) {
        if json {
            let _ = writeln!(out, "{}", entry_json(e, ""));
        } else {
            let _ = writeln!(out, "{}", e.path(""));
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.iter().any(|a| a == "--json");
    let args: Vec<String> = args.into_iter().filter(|a| a != "--json").collect();
    let Some((cmd, rest)) = args.split_first() else {
        println!("{USAGE}");
        return;
    };
    match cmd.as_str() {
        "ls" | "list" => ls(rest, json),
        "search" | "find" => search(rest, json),
        "size" | "du" => size(rest, json),
        "copy" | "cp" => transfer_or_remove(Kind::Copy, rest),
        "move" | "mv" => transfer_or_remove(Kind::Move, rest),
        "recycle" => transfer_or_remove(Kind::Recycle, rest),
        "delete" | "rm" => transfer_or_remove(Kind::Delete, rest),
        "index" => index(rest, json),
        "--version" | "-V" => println!("dvfiles-cli {}", env!("CARGO_PKG_VERSION")),
        _ => println!("{USAGE}"),
    }
}
