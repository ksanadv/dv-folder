// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
#![windows_subsystem = "windows"]

mod install;
mod settings;
mod trace;
mod ui;

use settings::Settings;
use ui::model::Loc;

fn message(text: &str, error: bool) {
    use windows::core::{w, PCWSTR};
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK};
    let t = ui::wstr(text);
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(t.as_ptr()),
            w!("dvfiles"),
            MB_OK | if error { MB_ICONERROR } else { MB_ICONINFORMATION },
        );
    }
}

fn cli(result: Result<String, String>) -> ! {
    match result {
        Ok(t) => {
            message(&t, false);
            std::process::exit(0)
        }
        Err(e) => {
            message(&e, true);
            std::process::exit(1)
        }
    }
}

/// A shell verb passes `"%1"`, and for a drive root that is `"C:\"`, which the C runtime's argument rules read as
/// `C:"`. Undo that, and accept `C:` for `C:\`.
fn clean_arg(a: &str) -> String {
    let mut s = a.trim().trim_end_matches('"').trim_start_matches('"').to_string();
    if s.len() == 2 && s.ends_with(':') {
        s.push('\\');
    }
    s
}

fn main() {
    trace::mark("main");
    trace::log(|| format!("{:8.2} ms  from the process's creation to main", trace::before_main()));
    std::thread::spawn(ui::gfx::warm_up);
    std::panic::set_hook(Box::new(|info| {
        message(&format!("dvfiles hit an internal error and must close.\n\n{info}"), true);
    }));
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--install") => cli(install::install().map(|exe| {
            format!(
                "dvfiles is installed ({}).\n\nIt is in the Start menu, \"Open in dvfiles\" is on every folder's menu, and dvfiles-cli is on your Path.\nTo open all folders with it, use ... > Open folders with dvfiles.",
                exe.display()
            )
        })),
        Some("--uninstall") => cli(install::uninstall().map(|()| "dvfiles has been uninstalled.".to_string())),
        // started by a window to build a drive's search index in its own process (see dvfiles::index)
        Some("--build-index") => std::process::exit(dvfiles::index::build_file(&args.get(1).cloned().unwrap_or_default())),
        Some("--default") => {
            let on = args.get(1).map(String::as_str) != Some("off");
            let r = std::env::current_exe().map_err(|e| e.to_string());
            cli(r.and_then(|exe| install::set_default_handler(&exe, on)).map(|()| {
                if on { "Folders now open in dvfiles." } else { "Folders open in File Explorer again." }.to_string()
            }))
        }
        _ => {}
    }

    let mut select: Option<String> = None;
    let mut target: Option<String> = None;
    // `--copy a b --to dir` / `--move a b --to dir`: run the job with dvfiles' progress window
    let mut job: Option<(dvfiles::ops::Kind, Vec<String>)> = None;
    let mut job_dest: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--select" => select = it.next().map(|s| clean_arg(s)),
            "--copy" => job = Some((dvfiles::ops::Kind::Copy, Vec::new())),
            "--move" => job = Some((dvfiles::ops::Kind::Move, Vec::new())),
            "--to" => job_dest = it.next().map(|s| dvfiles::fsx::full_path(&clean_arg(s))),
            _ if a.starts_with("--") => {}
            _ => match &mut job {
                Some((_, items)) if job_dest.is_none() => items.push(dvfiles::fsx::full_path(&clean_arg(a))),
                _ => target = Some(clean_arg(a)),
            },
        }
    }
    let job = match (job, job_dest) {
        (Some((kind, items)), Some(dest)) if !items.is_empty() && dvfiles::fsx::is_dir(&dest) => {
            target = Some(dest.clone());
            Some((kind, items, dest))
        }
        (Some(_), _) => {
            message("Usage: dvfiles --copy <items...> --to <folder>   (or --move)", true);
            std::process::exit(2)
        }
        _ => None,
    };
    // `dvfiles file.txt` and `--select` open the folder with the item selected
    if let Some(sel) = select.take() {
        target = dvfiles::pathx::parent(&dvfiles::fsx::full_path(&sel));
        select = Some(dvfiles::pathx::file_name(&sel).to_string());
    }
    let loc = match target.map(|t| dvfiles::fsx::full_path(&t)) {
        Some(t) if t.eq_ignore_ascii_case("this pc") => Loc::Pc,
        Some(t) if dvfiles::fsx::is_dir(&t) => Loc::Dir(t),
        Some(t) if dvfiles::fsx::exists(&t) => {
            select = Some(dvfiles::pathx::file_name(&t).to_string());
            dvfiles::pathx::parent(&t).map(Loc::Dir).unwrap_or(Loc::Pc)
        }
        _ => Loc::Pc,
    };
    // read the start folder while the window is being made, and put it in name order there (folders first), which
    // the window's own sort then finds done: sorting System32's 4,825 names took 3 ms of the first frame
    let listing = match &loc {
        Loc::Dir(d) => {
            let d = d.clone();
            Some(std::thread::spawn(move || {
                let mut v = dvfiles::fsx::list(&d).map_err(|e| e.to_string())?;
                v.sort_by(|x, y| y.is_dir().cmp(&x.is_dir()).then_with(|| dvfiles::natsort::cmp(&x.name, &y.name)));
                Ok(v)
            }))
        }
        Loc::Pc => None,
    };
    ui::run(ui::Start { loc, select, listing, job }, Settings::load());
}
