// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! dvfiles' engine: listing, searching, copying, moving and recycling, shared by the window (`main.rs`) and the
//! console tool (`cli.rs`). The first group has no Windows imports and is unit-tested on its own.

pub mod fmt;
pub mod naming;
pub mod natsort;
pub mod pastefile;
pub mod pathx;
pub mod query;
pub mod recyclefmt;
pub mod selection;
pub mod visits;

pub mod fsx;
pub mod img;
pub mod index;
pub mod lnk;
pub mod media;
pub mod ops;
pub mod recycle;
pub mod thumbcache;
pub mod undo;
pub mod usn;
pub mod walker;
pub mod wide;

/// a line for the trace file (`DVFILES_TRACE=1`) of the program using the engine
pub fn trace_log(s: &str) {
    if std::env::var_os("DVFILES_TRACE").is_some() {
        use std::io::Write;
        let path = std::env::temp_dir().join("dvfiles-trace.txt");
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "[engine] {s}");
        }
    }
}

/// Gives unused memory back to Windows (when idle, and after letting go of an index): the heaps release the free
/// blocks they keep cached, and the working set is emptied.
pub fn trim_memory() {
    use windows::Win32::System::Memory::{
        HeapOptimizeResources, HeapSetInformation, SetProcessWorkingSetSizeEx, SETPROCESSWORKINGSETSIZEEX_FLAGS,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;
    unsafe {
        // HEAP_OPTIMIZE_RESOURCES_INFORMATION, version 1
        let info: [u32; 2] = [1, 0];
        let _ = HeapSetInformation(
            None,
            HeapOptimizeResources,
            Some(&info as *const _ as *const _),
            std::mem::size_of_val(&info),
        );
        let _ = SetProcessWorkingSetSizeEx(
            GetCurrentProcess(),
            usize::MAX,
            usize::MAX,
            SETPROCESSWORKINGSETSIZEEX_FLAGS(0),
        );
    }
}
