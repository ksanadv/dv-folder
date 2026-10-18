// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Opt-in event log for debugging: set DVFILES_TRACE=1 and lines go to %TEMP%\dvfiles-trace.txt.
use std::io::Write;
use std::sync::OnceLock;

fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("DVFILES_TRACE").is_some_and(|v| v == "1"))
}

pub fn log(line: impl FnOnce() -> String) {
    if !enabled() {
        return;
    }
    let path = std::env::temp_dir().join("dvfiles-trace.txt");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{}", line());
    }
}

/// a timestamped line, in milliseconds since the process started (DVFILES_TRACE=1)
/// how long Windows took to start the process before main ran: loading the exe and the DLLs it imports
pub fn before_main() -> f64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut c, mut e, mut k, mut u) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    let now = unsafe {
        let _ = GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u);
        GetSystemTimePreciseAsFileTime()
    };
    let t = |f: FILETIME| (f.dwHighDateTime as u64) << 32 | f.dwLowDateTime as u64;
    t(now).saturating_sub(t(c)) as f64 / 10_000.0
}

pub fn mark(what: &str) {
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    let t = START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0;
    log(|| format!("{t:8.2} ms  {what}"));
}
