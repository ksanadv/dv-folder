// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! dvfiles' setup: the window that installs the program it carries, or with `--quiet` the same install with the
//! current choices and no window. `--preview` walks through the window without installing anything.
#![windows_subsystem = "windows"]

#[allow(dead_code)]
#[path = "../../src/install.rs"]
mod install;

use install::Options;
use std::path::{Path, PathBuf};
use windows::core::{w, PCWSTR};
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK};

const PAYLOAD: &[u8] = include_bytes!(env!("DVFILES_EXE"));
const CLI_PAYLOAD: &[u8] = include_bytes!(env!("DVFILES_CLI"));
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn message(error: bool, text: &str) {
    let t: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(t.as_ptr()),
            w!("dvfiles Setup"),
            MB_OK | if error { MB_ICONERROR } else { MB_ICONINFORMATION },
        );
    }
}

/// the boxes, in the order the window shows them
fn ticks(o: Options) -> [bool; 5] {
    [o.start_menu, o.desktop, o.path, o.folder_menu, o.default]
}

fn options(t: &[bool]) -> Options {
    Options { start_menu: t[0], desktop: t[1], path: t[2], folder_menu: t[3], default: t[4] }
}

fn choices(o: Options) -> Vec<dv_setup::Choice> {
    let labels = [
        "Add dvfiles to the Start menu",
        "Add a shortcut to the desktop",
        "Put dvfiles-cli on the Path, for the command line",
        "Add \"Open in dvfiles\" to folders' menus",
        "Open folders with dvfiles instead of File Explorer",
    ];
    labels
        .iter()
        .zip(ticks(o))
        .enumerate()
        .map(|(i, (l, on))| dv_setup::Choice { label: l.to_string(), on, needs: (i == 4).then_some(3) })
        .collect()
}

struct Install;

impl dv_setup::Work for Install {
    fn install(&self, ticked: &[bool], step: &dyn Fn(&str, f32)) -> Result<PathBuf, String> {
        let dir = install::install_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
        let target = dir.join(install::EXE_NAME);
        step("Copying dvfiles.exe", 0.1);
        install::replace_exe(&target, |t| std::fs::write(t, PAYLOAD))?;
        step("Copying dvfiles-cli.exe", 0.5);
        install::replace_exe(&dir.join(install::CLI_NAME), |t| std::fs::write(t, CLI_PAYLOAD))?;
        step("Adding the shortcuts and menus", 0.7);
        install::finish_install(&target, options(ticked))?;
        step("Done", 1.0);
        Ok(target)
    }

    fn launch(&self, exe: &Path) {
        let _ = std::process::Command::new(exe).spawn();
    }
}

/// the window's pages with nothing written: each step only waits
struct Preview;

impl dv_setup::Work for Preview {
    fn install(&self, _: &[bool], step: &dyn Fn(&str, f32)) -> Result<PathBuf, String> {
        for (t, f) in
            [("Copying dvfiles.exe", 0.1), ("Copying dvfiles-cli.exe", 0.5), ("Adding the shortcuts and menus", 0.7)]
        {
            step(t, f);
            std::thread::sleep(std::time::Duration::from_millis(1500));
        }
        Ok(install::install_dir().join(install::EXE_NAME))
    }

    fn launch(&self, _: &Path) {}
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let has = |a: &str| args.iter().any(|x| x.eq_ignore_ascii_case(a));
    if has("--uninstall") {
        let quiet = has("--quiet") || has("/S");
        match install::uninstall() {
            Ok(()) if !quiet => message(false, "dvfiles has been removed."),
            Ok(()) => {}
            Err(e) => {
                if !quiet {
                    message(true, &e);
                }
                std::process::exit(1);
            }
        }
        return;
    }
    let installed = install::installed_version();
    let current = Options::for_install();
    if has("--quiet") || has("/S") {
        let r = dv_setup::Work::install(&Install, &ticks(current), &|_, _| {});
        std::process::exit(r.is_err() as i32);
    }
    let update = installed.is_some();
    let mut done = "The ... menu in its toolbar holds the settings, and Apps & features removes it.".to_string();
    if update {
        done += "\n\nOpen dvfiles windows keep the old version until they are closed.";
    }
    let product = dv_setup::Product {
        name: "dvfiles",
        about: "A fast file manager for Windows",
        location: install::install_dir(),
        scope: dv_setup::Scope::App { version: VERSION, installed },
        choices: choices(current),
        done,
        launch: Some("Open dvfiles now".into()),
        activate: std::env::var_os("DVFILES_NOACTIVATE").is_none(),
    };
    if has("--preview") {
        dv_setup::run(product, Preview);
    } else {
        dv_setup::run(product, Install);
    }
}
