// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
#[path = "src/icon_art.rs"]
#[allow(dead_code)]
mod icon_art;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/icon_art.rs");
    println!("cargo:rerun-if-changed=assets/app.manifest");
    if std::env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }
    // The shell (icons, type names, "Show more options", opening files), COM and OLE are loaded on first use: listing,
    // searching and copying never need them, and the window comes up before the icon thread touches them.
    println!("cargo:rustc-link-lib=delayimp");
    println!("cargo:rustc-link-arg-bins=/IGNORE:4199");
    for dll in ["shell32.dll", "comctl32.dll", "ole32.dll", "oleaut32.dll", "shlwapi.dll", "version.dll"] {
        println!("cargo:rustc-link-arg-bins=/DELAYLOAD:{dll}");
    }
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let manifest = std::fs::canonicalize("assets/app.manifest").unwrap();
    std::fs::write(out.join("icon.ico"), icon_art::ico(&[16, 20, 24, 32, 48, 256])).unwrap();
    // rc.exe wants forward slashes
    let fwd = |p: &std::path::Path| p.display().to_string().trim_start_matches(r"\\?\").replace('\\', "/");
    let rc = format!("1 ICON \"{}\"\n1 24 \"{}\"\n", fwd(&out.join("icon.ico")), fwd(&manifest));
    std::fs::write(out.join("app.rc"), rc).unwrap();
    embed_resource::compile(out.join("app.rc"), embed_resource::NONE).manifest_required().unwrap();
}
