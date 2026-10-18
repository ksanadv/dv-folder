// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
#[path = "../src/icon_art.rs"]
#[allow(dead_code)]
mod icon_art;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../src/icon_art.rs");
    println!("cargo:rerun-if-changed=../assets/app.manifest");
    println!("cargo:rerun-if-env-changed=DVFILES_EXE");
    println!("cargo:rerun-if-env-changed=DVFILES_CLI");
    for var in ["DVFILES_EXE", "DVFILES_CLI"] {
        if std::env::var_os(var).is_none() {
            panic!("set {var} to the release exe (build-installer.ps1 does this)");
        }
    }
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let manifest = std::fs::canonicalize("../assets/app.manifest").unwrap();
    std::fs::write(out.join("icon.ico"), icon_art::ico(&[16, 20, 24, 32, 48, 64, 96, 256])).unwrap();
    // rc.exe wants forward slashes
    let fwd = |p: &std::path::Path| p.display().to_string().trim_start_matches(r"\\?\").replace('\\', "/");
    let rc = format!("1 ICON \"{}\"\n1 24 \"{}\"\n", fwd(&out.join("icon.ico")), fwd(&manifest));
    std::fs::write(out.join("app.rc"), rc).unwrap();
    embed_resource::compile(out.join("app.rc"), embed_resource::NONE).manifest_required().unwrap();
}
