// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! NTFS's change journal, as any user may read it: which files changed (by file id) and in which folder, since a
//! given point. Windows leaves the names out for non-administrators; the ids are enough for the search index
//! (`index`) to know which folders to read again.
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_HANDLE_EOF, HANDLE};
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Ioctl::{
    FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_UNPRIVILEGED_USN_JOURNAL, USN_JOURNAL_DATA_V0,
};
use windows::Win32::System::IO::DeviceIoControl;

/// one journal record
pub struct Rec<'a> {
    pub frn: u64,
    pub parent: u64,
    // why the record was written (journal reads only)
    pub reason: u32,
    pub attrs: u32,
    pub name: &'a [u16],
}

pub struct Volume {
    h: HANDLE,
}

impl Drop for Volume {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.h);
        }
    }
}

impl Volume {
    /// Opens a volume (`C:` or `C:\`) through its root folder, which any user may do and which is enough for the
    /// change journal.
    pub fn open(root: &str) -> std::io::Result<Volume> {
        let path = crate::wide::wide(&format!("{}\\", root.trim_end_matches('\\')));
        let h = unsafe {
            CreateFileW(
                crate::wide::pcwstr(&path),
                FILE_READ_ATTRIBUTES.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                None,
            )
        }
        .map_err(|e| std::io::Error::from_raw_os_error(e.code().0 & 0xFFFF))?;
        Ok(Volume { h })
    }

    /// the change journal: its id, its oldest and its next change number; None when the volume keeps none
    pub fn journal(&self) -> Option<Journal> {
        let mut out = USN_JOURNAL_DATA_V0::default();
        let mut got = 0u32;
        unsafe {
            DeviceIoControl(
                self.h,
                FSCTL_QUERY_USN_JOURNAL,
                None,
                0,
                Some(&mut out as *mut _ as *mut _),
                std::mem::size_of::<USN_JOURNAL_DATA_V0>() as u32,
                Some(&mut got),
                None,
            )
            .ok()?;
        }
        Some(Journal { id: out.UsnJournalID, first: out.FirstUsn, next: out.NextUsn })
    }

    /// Calls `f` for every change recorded since `from` in journal `j`, oldest first, and returns where the journal
    /// ends now. Any user may do this. Fails when the journal was replaced or `from` has been overwritten (then the
    /// caller must look again from scratch).
    pub fn changes(&self, j: &Journal, from: i64, mut f: impl FnMut(&Rec)) -> std::io::Result<i64> {
        if from < j.first {
            return Err(std::io::Error::other("the change journal has moved past the last look"));
        }
        let mut start = from;
        let mut buf = vec![0u64; 64 * 1024];
        loop {
            // READ_USN_JOURNAL_DATA_V1, no waiting
            let mut input = [0u8; 48];
            input[0..8].copy_from_slice(&start.to_le_bytes());
            input[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
            input[32..40].copy_from_slice(&j.id.to_le_bytes());
            input[40..42].copy_from_slice(&2u16.to_le_bytes());
            input[42..44].copy_from_slice(&3u16.to_le_bytes());
            let Some(bytes) = self.ioctl(FSCTL_READ_UNPRIVILEGED_USN_JOURNAL, input.as_ptr(), 48, &mut buf)? else {
                return Ok(start);
            };
            if bytes.len() < 8 {
                return Ok(start);
            }
            let next = i64::from_le_bytes(bytes[0..8].try_into().unwrap());
            records(&bytes[8..], &mut |r| {
                f(r);
                true
            });
            if next <= start || bytes.len() == 8 {
                return Ok(next.max(start));
            }
            start = next;
        }
    }

    /// one control call; None at the end of the data
    fn ioctl<'b>(
        &self,
        code: u32,
        input: *const u8,
        in_len: u32,
        buf: &'b mut [u64],
    ) -> std::io::Result<Option<&'b [u8]>> {
        let mut got = 0u32;
        let r = unsafe {
            DeviceIoControl(
                self.h,
                code,
                Some(input as *const _),
                in_len,
                Some(buf.as_mut_ptr() as *mut _),
                (buf.len() * 8) as u32,
                Some(&mut got),
                None,
            )
        };
        if r.is_err() {
            let e = unsafe { GetLastError() };
            if e == ERROR_HANDLE_EOF {
                return Ok(None);
            }
            return Err(std::io::Error::from_raw_os_error(e.0 as i32));
        }
        Ok(Some(unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, got as usize) }))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Journal {
    pub id: u64,
    pub first: i64,
    pub next: i64,
}

/// walks USN_RECORD_V2/V3 records; false when `f` asked to stop
fn records(bytes: &[u8], f: &mut impl FnMut(&Rec) -> bool) -> bool {
    let mut at = 0usize;
    while at + 60 <= bytes.len() {
        let rd = |o: usize, n: usize| &bytes[at + o..at + o + n];
        let len = u32::from_le_bytes(rd(0, 4).try_into().unwrap()) as usize;
        if len < 60 || at + len > bytes.len() {
            break;
        }
        let u16_at = |o: usize| u16::from_le_bytes(rd(o, 2).try_into().unwrap()) as usize;
        let u32_at = |o: usize| u32::from_le_bytes(rd(o, 4).try_into().unwrap());
        let u64_at = |o: usize| u64::from_le_bytes(rd(o, 8).try_into().unwrap());
        let rec = match u16_at(4) {
            2 => Some((u64_at(8), u64_at(16), u32_at(40), u32_at(52), u16_at(58), u16_at(56))),
            // 128-bit ids: NTFS ids fit the low half
            3 if len >= 76 => Some((u64_at(8), u64_at(24), u32_at(56), u32_at(68), u16_at(74), u16_at(72))),
            _ => None,
        };
        if let Some((frn, parent, reason, attrs, off, nl)) = rec {
            if off + nl <= len && nl % 2 == 0 {
                // records are 8-byte aligned in a u64-aligned buffer, so the name is u16-aligned
                let p = unsafe { bytes.as_ptr().add(at + off) as *const u16 };
                let name = unsafe { std::slice::from_raw_parts(p, nl / 2) };
                if !f(&Rec { frn, parent, reason, attrs, name }) {
                    return false;
                }
            }
        }
        at += len;
    }
    true
}

pub const ID_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
// the root folder's table record on NTFS
pub const ROOT_ID: u64 = 5;
