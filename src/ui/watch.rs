// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Keeps the shown folder current. One thread waits in `ReadDirectoryChangesW` for the folder to change; it uses no
//! CPU while nothing happens (no polling), and tells the window, which reloads after a short pause so a burst of
//! changes costs one reload.
use dvfiles::wide::long;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Threading::{CreateEventW, ResetEvent, SetEvent, WaitForMultipleObjects, INFINITE};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub const WM_DIRCHANGED: u32 = WM_APP + 11;

pub struct Watcher {
    stop: isize,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watcher {
    pub fn start(hwnd: HWND, path: &str, generation: usize) -> Option<Watcher> {
        let w = long(path);
        unsafe {
            let dir = CreateFileW(
                PCWSTR(w.as_ptr()),
                FILE_LIST_DIRECTORY.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                None,
            )
            .ok()?;
            let stop = CreateEventW(None, true, false, PCWSTR::null()).ok()?;
            let (dir_raw, stop_raw, hwnd_raw) = (dir.0 as isize, stop.0 as isize, hwnd.0 as isize);
            let thread = std::thread::spawn(move || run(dir_raw, stop_raw, hwnd_raw, generation));
            Some(Watcher { stop: stop.0 as isize, thread: Some(thread) })
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        unsafe {
            let _ = SetEvent(HANDLE(self.stop as *mut _));
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        unsafe {
            let _ = CloseHandle(HANDLE(self.stop as *mut _));
        }
    }
}

fn run(dir: isize, stop: isize, hwnd: isize, generation: usize) {
    let (dir, stop) = (HANDLE(dir as *mut _), HANDLE(stop as *mut _));
    unsafe {
        let Ok(io) = CreateEventW(None, true, false, PCWSTR::null()) else {
            let _ = CloseHandle(dir);
            return;
        };
        let mut buf = vec![0u64; 8 * 1024 / 8];
        loop {
            let mut ov = OVERLAPPED { hEvent: io, ..Default::default() };
            let _ = ResetEvent(io);
            let filter = FILE_NOTIFY_CHANGE_FILE_NAME
                | FILE_NOTIFY_CHANGE_DIR_NAME
                | FILE_NOTIFY_CHANGE_ATTRIBUTES
                | FILE_NOTIFY_CHANGE_SIZE
                | FILE_NOTIFY_CHANGE_LAST_WRITE;
            if ReadDirectoryChangesW(
                dir,
                buf.as_mut_ptr() as *mut _,
                (buf.len() * 8) as u32,
                false,
                filter,
                None,
                Some(&mut ov),
                None,
            )
            .is_err()
            {
                // the folder is gone or cannot be watched: let the window find out by reloading
                let _ = PostMessageW(
                    Some(HWND(hwnd as *mut _)),
                    WM_DIRCHANGED,
                    windows::Win32::Foundation::WPARAM(generation),
                    Default::default(),
                );
                break;
            }
            let woke = WaitForMultipleObjects(&[io, stop], false, INFINITE);
            if woke != WAIT_OBJECT_0 {
                let _ = CancelIoEx(dir, Some(&ov));
                let mut n = 0u32;
                let _ = GetOverlappedResult(dir, &ov, &mut n, true);
                break;
            }
            let mut n = 0u32;
            let _ = GetOverlappedResult(dir, &ov, &mut n, false);
            let _ = PostMessageW(
                Some(HWND(hwnd as *mut _)),
                WM_DIRCHANGED,
                windows::Win32::Foundation::WPARAM(generation),
                Default::default(),
            );
        }
        let _ = CloseHandle(io);
        let _ = CloseHandle(dir);
    }
}
