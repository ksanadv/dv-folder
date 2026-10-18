// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Reading folders and volumes.
//!
//! A folder is read with `GetFileInformationByHandleEx(FileFullDirectoryInfo)` into a 64 KB buffer: one call returns
//! hundreds of entries with their sizes, times and attributes, and no 8.3 short names are produced. There is no
//! per-file open or stat, and file contents are never touched, so cloud placeholders are not downloaded.
use crate::wide::{long, pcwstr, wide};
use std::io;
use std::sync::Arc;
use windows::Win32::Foundation::{CloseHandle, ERROR_NO_MORE_FILES, HANDLE};
use windows::Win32::Storage::FileSystem::*;

pub const FA_READONLY: u32 = 0x1;
pub const FA_HIDDEN: u32 = 0x2;
pub const FA_SYSTEM: u32 = 0x4;
pub const FA_DIRECTORY: u32 = 0x10;
pub const FA_ARCHIVE: u32 = 0x20;
pub const FA_NORMAL: u32 = 0x80;
pub const FA_SPARSE: u32 = 0x200;
pub const FA_REPARSE: u32 = 0x400;
pub const FA_COMPRESSED: u32 = 0x800;
pub const FA_OFFLINE: u32 = 0x1000;
pub const FA_ENCRYPTED: u32 = 0x4000;
pub const FA_PINNED: u32 = 0x80000;
pub const FA_UNPINNED: u32 = 0x100000;
pub const FA_RECALL_ON_OPEN: u32 = 0x40000;
pub const FA_RECALL_ON_DATA: u32 = 0x400000;

/// one item of a listing or of search results
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: Box<str>,
    /// the folder it is in, for search results; None for the folder being shown
    pub dir: Option<Arc<str>>,
    pub attrs: u32,
    pub size: u64,
    pub modified: u64,
    pub created: u64,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.attrs & FA_DIRECTORY != 0
    }

    pub fn is_hidden(&self) -> bool {
        self.attrs & FA_HIDDEN != 0
    }

    /// hidden and system together: the "protected operating system files" Explorer hides separately
    pub fn is_super_hidden(&self) -> bool {
        self.attrs & (FA_HIDDEN | FA_SYSTEM) == (FA_HIDDEN | FA_SYSTEM)
    }

    pub fn path(&self, folder: &str) -> String {
        crate::pathx::join(self.dir.as_deref().unwrap_or(folder), &self.name)
    }

    pub fn from_raw(e: &RawEnt, dir: Option<Arc<str>>) -> Entry {
        Entry {
            name: String::from_utf16_lossy(e.name).into_boxed_str(),
            dir,
            attrs: e.attrs,
            size: if e.attrs & FA_DIRECTORY != 0 { 0 } else { e.size },
            modified: e.modified,
            created: e.created,
        }
    }
}

pub fn is_cloud_only(attrs: u32) -> bool {
    attrs & (FA_OFFLINE | FA_RECALL_ON_DATA | FA_RECALL_ON_OPEN) != 0
}

/// an entry as the file system returned it, borrowed from the read buffer
pub struct RawEnt<'a> {
    pub name: &'a [u16],
    pub attrs: u32,
    pub size: u64,
    pub alloc: u64,
    pub created: u64,
    pub accessed: u64,
    pub modified: u64,
    /// the file id (NTFS: table record and sequence), only from a reader made `with_ids`
    pub id: u64,
}

impl RawEnt<'_> {
    pub fn is_dir(&self) -> bool {
        self.attrs & FA_DIRECTORY != 0
    }

    /// a directory that can be walked into: not a junction, symlink, mount point or cloud folder placeholder
    pub fn is_plain_dir(&self) -> bool {
        self.attrs & (FA_DIRECTORY | FA_REPARSE) == FA_DIRECTORY
    }
}

pub fn last_error() -> io::Error {
    io::Error::last_os_error()
}

pub fn win_err(e: windows::core::Error) -> io::Error {
    let code = e.code().0 as u32;
    // HRESULT_FROM_WIN32 wraps Win32 codes as 0x8007xxxx
    if code & 0xFFFF_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((code & 0xFFFF) as i32)
    } else {
        io::Error::other(e.message())
    }
}

pub struct Handle(pub HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub fn open_dir(path_w: &[u16]) -> io::Result<Handle> {
    unsafe {
        CreateFileW(
            pcwstr(path_w),
            FILE_LIST_DIRECTORY.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
        .map(Handle)
        .map_err(win_err)
    }
}

/// Reads folders; keep one per thread, it owns the 64 KB buffer.
pub struct DirReader {
    buf: Vec<u64>,
    ids: bool,
}

impl Default for DirReader {
    fn default() -> Self {
        DirReader { buf: vec![0u64; 64 * 1024 / 8], ids: false }
    }
}

impl DirReader {
    /// a reader that also returns file ids (FILE_ID_EXTD_DIR_INFO: the same fields plus the id)
    pub fn with_ids() -> Self {
        DirReader { ids: true, ..Default::default() }
    }

    /// calls `f` for every entry except `.` and `..`; `path_w` is NUL-terminated (see `wide::long`)
    pub fn read(&mut self, path_w: &[u16], mut f: impl FnMut(&RawEnt)) -> io::Result<()> {
        let h = open_dir(path_w)?;
        self.read_handle(&h, &mut f)
    }

    pub fn read_handle(&mut self, h: &Handle, f: &mut impl FnMut(&RawEnt)) -> io::Result<()> {
        let bytes = self.buf.len() * 8;
        let (mut class, more, name_at) = if self.ids {
            (FileIdExtdDirectoryRestartInfo, FileIdExtdDirectoryInfo, 88)
        } else {
            (FileFullDirectoryRestartInfo, FileFullDirectoryInfo, 68)
        };
        loop {
            let r = unsafe { GetFileInformationByHandleEx(h.0, class, self.buf.as_mut_ptr() as *mut _, bytes as u32) };
            if let Err(e) = r {
                if e.code() == ERROR_NO_MORE_FILES.to_hresult() {
                    return Ok(());
                }
                return Err(win_err(e));
            }
            class = more;
            let base = self.buf.as_ptr() as *const u8;
            let mut off = 0usize;
            loop {
                // FILE_FULL_DIR_INFO: entries are 8-byte aligned inside the buffer
                let p = unsafe { base.add(off) };
                let rd32 = |at: usize| unsafe { (p.add(at) as *const u32).read_unaligned() };
                let rd64 = |at: usize| unsafe { (p.add(at) as *const u64).read_unaligned() };
                let next = rd32(0) as usize;
                let name_len = rd32(60) as usize / 2;
                let name = unsafe { std::slice::from_raw_parts(p.add(name_at) as *const u16, name_len) };
                let dots = name == [b'.' as u16] || name == [b'.' as u16, b'.' as u16];
                if !dots {
                    f(&RawEnt {
                        name,
                        created: rd64(8),
                        accessed: rd64(16),
                        modified: rd64(24),
                        size: rd64(40),
                        alloc: rd64(48),
                        attrs: rd32(56),
                        // FILE_ID_128 at 72: NTFS ids fit its low half
                        id: if name_at == 88 { rd64(72) } else { 0 },
                    });
                }
                if next == 0 {
                    break;
                }
                off += next;
                if off + name_at > bytes {
                    break;
                }
            }
        }
    }
}

/// a file's or folder's id (NTFS: table record and sequence)
pub fn file_id(path: &str) -> io::Result<u64> {
    unsafe {
        let h = CreateFileW(
            pcwstr(&long(path)),
            FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
        .map(Handle)
        .map_err(win_err)?;
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        GetFileInformationByHandle(h.0, &mut info).map_err(win_err)?;
        Ok((info.nFileIndexHigh as u64) << 32 | info.nFileIndexLow as u64)
    }
}

/// `%SystemRoot%\\x` with its variables filled in
pub fn expand_env(s: &str) -> String {
    use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
    if !s.contains('%') {
        return s.to_string();
    }
    let w = wide(s);
    let mut buf = vec![0u16; 4096];
    let n = unsafe { ExpandEnvironmentStringsW(pcwstr(&w), Some(&mut buf)) } as usize;
    if n == 0 || n > buf.len() {
        return s.to_string();
    }
    crate::wide::from_wide(&buf)
}

/// the whole folder as entries
pub fn list(path: &str) -> io::Result<Vec<Entry>> {
    let mut out = Vec::new();
    DirReader::default().read(&long(path), |e| out.push(Entry::from_raw(e, None)))?;
    Ok(out)
}

/// one item's metadata without opening it
pub fn stat(path: &str) -> io::Result<Entry> {
    let mut data = WIN32_FILE_ATTRIBUTE_DATA::default();
    unsafe {
        GetFileAttributesExW(pcwstr(&long(path)), GetFileExInfoStandard, &mut data as *mut _ as *mut _)
            .map_err(win_err)?;
    }
    let ft = |t: windows::Win32::Foundation::FILETIME| (t.dwHighDateTime as u64) << 32 | t.dwLowDateTime as u64;
    let is_dir = data.dwFileAttributes & FA_DIRECTORY != 0;
    Ok(Entry {
        name: crate::pathx::file_name(path).into(),
        dir: None,
        attrs: data.dwFileAttributes,
        size: if is_dir { 0 } else { (data.nFileSizeHigh as u64) << 32 | data.nFileSizeLow as u64 },
        modified: ft(data.ftLastWriteTime),
        created: ft(data.ftCreationTime),
    })
}

pub fn exists(path: &str) -> bool {
    unsafe { GetFileAttributesW(pcwstr(&long(path))) != INVALID_FILE_ATTRIBUTES }
}

pub fn attributes(path: &str) -> Option<u32> {
    let a = unsafe { GetFileAttributesW(pcwstr(&long(path))) };
    (a != INVALID_FILE_ATTRIBUTES).then_some(a)
}

pub fn set_attributes(path: &str, attrs: u32) -> io::Result<()> {
    unsafe { SetFileAttributesW(pcwstr(&long(path)), FILE_FLAGS_AND_ATTRIBUTES(attrs)).map_err(win_err) }
}

pub fn is_dir(path: &str) -> bool {
    attributes(path).is_some_and(|a| a & FA_DIRECTORY != 0)
}

/// `C:\` for any path on C:, or the mount point / share root the path lies on
pub fn volume_root(path: &str) -> Option<String> {
    let mut buf = [0u16; 1024];
    unsafe { GetVolumePathNameW(pcwstr(&wide(path)), &mut buf).ok()? };
    Some(crate::wide::from_wide(&buf))
}

/// `\\?\Volume{guid}\` of a volume root
pub fn volume_guid(root: &str) -> Option<String> {
    let mut buf = [0u16; 64];
    unsafe { GetVolumeNameForVolumeMountPointW(pcwstr(&wide(root)), &mut buf).ok()? };
    Some(crate::wide::from_wide(&buf))
}

pub fn same_volume(a: &str, b: &str) -> bool {
    match (volume_root(a), volume_root(b)) {
        (Some(x), Some(y)) => x.eq_ignore_ascii_case(&y),
        _ => false,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DriveKind {
    Fixed,
    Removable,
    Network,
    Optical,
    Ram,
    Unknown,
}

pub fn drive_kind(root: &str) -> DriveKind {
    match unsafe { GetDriveTypeW(pcwstr(&wide(root))) } {
        2 => DriveKind::Removable,
        3 => DriveKind::Fixed,
        4 => DriveKind::Network,
        5 => DriveKind::Optical,
        6 => DriveKind::Ram,
        _ => DriveKind::Unknown,
    }
}

#[derive(Clone, Debug)]
pub struct Drive {
    pub root: String,
    pub kind: DriveKind,
    pub label: String,
    pub fs: String,
    pub total: u64,
    pub free: u64,
    pub ready: bool,
}

impl Drive {
    /// `Local Disk (C:)`, `Data (D:)`
    pub fn title(&self) -> String {
        let letter = self.root.trim_end_matches('\\');
        let label = if !self.label.is_empty() {
            self.label.clone()
        } else {
            match self.kind {
                DriveKind::Fixed => "Local Disk".into(),
                DriveKind::Removable => "USB Drive".into(),
                DriveKind::Network => "Network Drive".into(),
                DriveKind::Optical => "DVD Drive".into(),
                _ => "Drive".into(),
            }
        };
        format!("{label} ({letter})")
    }

    pub fn type_name(&self) -> &'static str {
        match self.kind {
            DriveKind::Fixed => "Local Disk",
            DriveKind::Removable => "Removable Disk",
            DriveKind::Network => "Network Drive",
            DriveKind::Optical => "CD Drive",
            DriveKind::Ram => "RAM Disk",
            DriveKind::Unknown => "Drive",
        }
    }
}

/// drive letters only, instantly (no disk is touched)
pub fn drive_roots() -> Vec<String> {
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8).filter(|i| mask & (1 << i) != 0).map(|i| format!("{}:\\", (b'A' + i) as char)).collect()
}

/// Label, file system and space. This can wait for a slow or sleeping disk or a network drive, so the UI calls it
/// on a thread.
pub fn drive_info(root: &str) -> Drive {
    let kind = drive_kind(root);
    let mut d =
        Drive { root: root.into(), kind, label: String::new(), fs: String::new(), total: 0, free: 0, ready: false };
    let w = wide(root);
    let (mut label, mut fs) = ([0u16; 261], [0u16; 64]);
    unsafe {
        if GetVolumeInformationW(pcwstr(&w), Some(&mut label), None, None, None, Some(&mut fs)).is_ok() {
            d.label = crate::wide::from_wide(&label);
            d.fs = crate::wide::from_wide(&fs);
            d.ready = true;
        }
        let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
        if GetDiskFreeSpaceExW(pcwstr(&w), Some(&mut avail), Some(&mut total), Some(&mut free)).is_ok() {
            d.total = total;
            d.free = avail;
        }
    }
    d
}

pub fn cluster_size(root: &str) -> u64 {
    let (mut spc, mut bps, mut free, mut total) = (0u32, 0u32, 0u32, 0u32);
    unsafe {
        if GetDiskFreeSpaceW(pcwstr(&wide(root)), Some(&mut spc), Some(&mut bps), Some(&mut free), Some(&mut total))
            .is_ok()
        {
            return (spc as u64 * bps as u64).max(512);
        }
    }
    4096
}

/// Whether the disk under `root` is a spinning one. Asked through the volume device, which needs no administrator
/// rights; None when Windows cannot say (several disks, network, virtual drives).
pub fn seek_penalty(root: &str) -> Option<bool> {
    use windows::Win32::System::Ioctl::{
        PropertyStandardQuery, StorageDeviceSeekPenaltyProperty, DEVICE_SEEK_PENALTY_DESCRIPTOR,
        IOCTL_STORAGE_QUERY_PROPERTY, STORAGE_PROPERTY_QUERY,
    };
    use windows::Win32::System::IO::DeviceIoControl;
    let letter = root.trim_end_matches('\\');
    if letter.len() != 2 {
        return None;
    }
    let dev = wide(&format!(r"\\.\{letter}"));
    unsafe {
        let h = CreateFileW(
            pcwstr(&dev),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
        .ok()?;
        let h = Handle(h);
        let q = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageDeviceSeekPenaltyProperty,
            QueryType: PropertyStandardQuery,
            ..Default::default()
        };
        let mut out = DEVICE_SEEK_PENALTY_DESCRIPTOR::default();
        let mut got = 0u32;
        DeviceIoControl(
            h.0,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&q as *const _ as *const _),
            std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(&mut out as *mut _ as *mut _),
            std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
            Some(&mut got),
            None,
        )
        .ok()?;
        Some(out.IncursSeekPenalty)
    }
}

/// an absolute, normalized path (`..` resolved, `/` turned into `\`) for what the user typed
pub fn full_path(path: &str) -> String {
    let w = wide(path);
    let mut buf = vec![0u16; 1024];
    loop {
        let n = unsafe { GetFullPathNameW(pcwstr(&w), Some(&mut buf), None) } as usize;
        if n == 0 {
            return path.to_string();
        }
        if n < buf.len() {
            let s = String::from_utf16_lossy(&buf[..n]);
            // keep a drive root's backslash, drop any other trailing one
            return if s.len() > 3 { s.trim_end_matches('\\').to_string() } else { s };
        }
        buf.resize(n + 1, 0);
    }
}

pub fn filetime_now() -> u64 {
    let ft = unsafe { windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime() };
    (ft.dwHighDateTime as u64) << 32 | ft.dwLowDateTime as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_a_folder_like_std() {
        let dir = std::env::temp_dir().join(format!("dvfiles_fsx_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        for i in 0..300 {
            std::fs::write(dir.join(format!("file {i} é.txt")), vec![1u8; i]).unwrap();
        }
        let d = dir.to_str().unwrap();
        let mut ours = list(d).unwrap();
        ours.sort_by(|a, b| a.name.cmp(&b.name));
        let mut theirs: Vec<(String, u64)> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap())
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.metadata().unwrap().len()))
            .collect();
        theirs.sort();
        assert_eq!(ours.len(), 301);
        for (a, (n, len)) in ours.iter().zip(&theirs) {
            assert_eq!(&*a.name, n);
            if !a.is_dir() {
                assert_eq!(a.size, *len);
            }
            assert!(a.modified > 0);
        }
        assert!(ours.iter().any(|e| e.is_dir() && &*e.name == "sub"));
        let s = stat(&format!(r"{d}\file 5 é.txt")).unwrap();
        assert_eq!((s.size, s.is_dir()), (5, false));
        assert!(stat(&format!(r"{d}\nope")).is_err());
        assert!(list(&format!(r"{d}\nope")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn volumes() {
        let windir = std::env::var("WINDIR").unwrap_or(r"C:\Windows".into());
        let root = volume_root(&windir).unwrap();
        assert!(root.ends_with(":\\"), "{root}");
        assert!(drive_roots().contains(&root.to_uppercase()));
        assert!(same_volume(&windir, &root));
        let d = drive_info(&root);
        assert!(d.ready && d.total > 0 && d.kind == DriveKind::Fixed);
        assert!(cluster_size(&root) >= 512);
        assert!(volume_guid(&root).unwrap().starts_with(r"\\?\Volume{"));
    }
}

/// A file mapped read-only (the saved search index, a picture being made into a thumbnail): its pages belong to the
/// file, not to the process's private memory; Windows can drop and re-read them at will, and processes share them.
pub struct Mapped {
    file: HANDLE,
    map: HANDLE,
    view: *const u8,
    len: usize,
}

// the view is read-only and lives as long as the struct
unsafe impl Send for Mapped {}
unsafe impl Sync for Mapped {}

impl Mapped {
    pub fn open(path: &std::path::Path) -> io::Result<Mapped> {
        use windows::Win32::Storage::FileSystem::*;
        use windows::Win32::System::Memory::*;
        let w = crate::wide::wide(&path.to_string_lossy());
        unsafe {
            let file = CreateFileW(
                crate::wide::pcwstr(&w),
                FILE_GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
            .map_err(win_err)?;
            let mut size = 0i64;
            if GetFileSizeEx(file, &mut size).is_err() || size <= 0 {
                let _ = CloseHandle(file);
                return Err(io::Error::other("the file is empty"));
            }
            let map = match CreateFileMappingW(file, None, PAGE_READONLY, 0, 0, None) {
                Ok(m) => m,
                Err(e) => {
                    let _ = CloseHandle(file);
                    return Err(win_err(e));
                }
            };
            let view = MapViewOfFile(map, FILE_MAP_READ, 0, 0, 0);
            if view.Value.is_null() {
                let _ = CloseHandle(map);
                let _ = CloseHandle(file);
                return Err(io::Error::last_os_error());
            }
            Ok(Mapped { file, map, view: view.Value as *const u8, len: size as usize })
        }
    }

    pub fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.view, self.len) }
    }
}

impl Drop for Mapped {
    fn drop(&mut self) {
        use windows::Win32::System::Memory::{UnmapViewOfFile, MEMORY_MAPPED_VIEW_ADDRESS};
        unsafe {
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS { Value: self.view as *mut _ });
            let _ = CloseHandle(self.map);
            let _ = CloseHandle(self.file);
        }
    }
}
