// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Where a shortcut (.lnk) points, read from the file itself ([MS-SHLLINK]: the header, the item id list, the
//! LinkInfo block with the target's local or network path, then the strings, among them the target's path relative
//! to the shortcut). This needs neither COM nor the shell, which IShellLink would load to answer the same question;
//! shortcuts that hold neither (to shell folders, or "advertised" installer shortcuts) return None and the caller
//! asks the shell.

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// a NUL-terminated UTF-16 string at `at`
fn wide_at(b: &[u8], at: usize) -> Option<String> {
    let mut units = Vec::new();
    let mut i = at;
    loop {
        let u = u16_at(b, i)?;
        if u == 0 {
            break;
        }
        units.push(u);
        i += 2;
    }
    Some(String::from_utf16_lossy(&units))
}

/// a NUL-terminated string in the system's ANSI code page at `at`
fn ansi_at(b: &[u8], at: usize) -> Option<String> {
    let s = b.get(at..)?;
    let end = s.iter().position(|&c| c == 0)?;
    let bytes = &s[..end];
    if bytes.is_ascii() {
        return Some(String::from_utf8_lossy(bytes).into_owned());
    }
    use windows::Win32::Globalization::{MultiByteToWideChar, CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS};
    let mut out = vec![0u16; bytes.len() * 2 + 1];
    let n = unsafe { MultiByteToWideChar(CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, Some(&mut out)) };
    (n > 0).then(|| String::from_utf16_lossy(&out[..n as usize]))
}

const HAS_ID_LIST: u32 = 0x1;
const HAS_LINK_INFO: u32 = 0x2;
const HAS_NAME: u32 = 0x4;
const HAS_RELATIVE_PATH: u32 = 0x8;
const HAS_WORKING_DIR: u32 = 0x10;
const HAS_ARGUMENTS: u32 = 0x20;
const HAS_ICON_LOCATION: u32 = 0x40;
const IS_UNICODE: u32 = 0x80;

/// what a shortcut file says
#[derive(Debug, Default, PartialEq)]
pub struct Link {
    /// the target's full path, from the LinkInfo block
    pub full: Option<String>,
    /// the target's path relative to the shortcut
    pub relative: Option<String>,
    /// the icon it names (a file, maybe with %variables%) and the icon's index in it
    pub icon: Option<(String, i32)>,
}

pub fn parse(b: &[u8]) -> Link {
    let mut link = Link::default();
    // ShellLinkHeader: 0x4C bytes, with the class id 00021401-0000-0000-C000-000000000046
    if u32_at(b, 0) != Some(0x4C) || b.get(4..8) != Some(&[0x01, 0x14, 0x02, 0x00][..]) {
        return link;
    }
    let Some(flags) = u32_at(b, 0x14) else { return link };
    let icon_index = u32_at(b, 0x38).unwrap_or(0) as i32;
    let mut at = 0x4C;
    if flags & HAS_ID_LIST != 0 {
        let Some(n) = u16_at(b, at) else { return link };
        at += 2 + n as usize;
    }
    if flags & HAS_LINK_INFO != 0 {
        link.full = link_info(b, at);
        let Some(size) = u32_at(b, at) else { return link };
        at += size as usize;
    }
    // StringData: counted strings, in this order, each only if its flag is set
    let unicode = flags & IS_UNICODE != 0;
    let string = |at: &mut usize| -> Option<String> {
        let n = u16_at(b, *at)? as usize;
        *at += 2;
        let len = if unicode { n * 2 } else { n };
        let raw = b.get(*at..*at + len)?;
        *at += len;
        Some(if unicode {
            String::from_utf16_lossy(&raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>())
        } else {
            String::from_utf8_lossy(raw).into_owned()
        })
    };
    let mut strings = [None, None, None, None, None];
    for (k, bit) in [HAS_NAME, HAS_RELATIVE_PATH, HAS_WORKING_DIR, HAS_ARGUMENTS, HAS_ICON_LOCATION].iter().enumerate()
    {
        if flags & bit != 0 {
            match string(&mut at) {
                Some(v) => strings[k] = Some(v),
                None => break,
            }
        }
    }
    link.relative = strings[1].take().filter(|r| !r.is_empty());
    link.icon = strings[4].take().filter(|i| !i.is_empty()).map(|i| (i, icon_index));
    link
}

/// the target's path in a LinkInfo block at `li`
fn link_info(b: &[u8], li: usize) -> Option<String> {
    let header = u32_at(b, li + 4)? as usize;
    let info_flags = u32_at(b, li + 8)?;
    let suffix = if header >= 0x24 {
        wide_at(b, li + u32_at(b, li + 0x20)? as usize)?
    } else {
        ansi_at(b, li + u32_at(b, li + 0x18)? as usize)?
    };
    let base = if info_flags & 0x1 != 0 {
        // VolumeIDAndLocalBasePath
        if header >= 0x24 {
            wide_at(b, li + u32_at(b, li + 0x1C)? as usize)?
        } else {
            ansi_at(b, li + u32_at(b, li + 0x10)? as usize)?
        }
    } else if info_flags & 0x2 != 0 {
        // CommonNetworkRelativeLink: the share's name
        let net = li + u32_at(b, li + 0x14)? as usize;
        let name_off = u32_at(b, net + 8)? as usize;
        if name_off > 0x14 {
            wide_at(b, net + u32_at(b, net + 0x14)? as usize)?
        } else {
            ansi_at(b, net + name_off)?
        }
    } else {
        return None;
    };
    if base.is_empty() {
        return None;
    }
    Some(if suffix.is_empty() {
        base
    } else if base.ends_with('\\') {
        format!("{base}{suffix}")
    } else {
        format!("{base}\\{suffix}")
    })
}

/// the icon a .lnk file names, read without the shell: its file (variables expanded) and index
pub fn icon(path: &str) -> Option<(String, i32)> {
    let b = std::fs::read(path).ok().filter(|b| b.len() <= 1 << 20)?;
    let (file, index) = parse(&b).icon?;
    Some((crate::fsx::expand_env(&file), index))
}

/// where a .lnk file points, read without the shell: its full path, else its relative path from the shortcut's
/// folder (when that exists)
pub fn target(path: &str) -> Option<String> {
    let b = std::fs::read(path).ok()?;
    if b.len() > 1 << 20 {
        return None;
    }
    let Link { full, relative, .. } = parse(&b);
    if full.is_some() {
        return full;
    }
    let dir = crate::pathx::parent(path)?;
    let mut parts: Vec<String> = dir.split('\\').filter(|p| !p.is_empty()).map(String::from).collect();
    for part in relative?.split('\\') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.len() > 1 {
                    parts.pop();
                }
            }
            p => parts.push(p.to_string()),
        }
    }
    let joined = if dir.starts_with("\\\\") { format!("\\\\{}", parts.join("\\")) } else { parts.join("\\") };
    let joined = if joined.len() == 2 { format!("{joined}\\") } else { joined };
    crate::fsx::exists(&joined).then_some(joined)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// every Start menu shortcut this reads must point where Windows' own shortcut object says it does
    #[test]
    fn same_targets_as_windows() {
        let script = "$w = New-Object -ComObject WScript.Shell; Get-ChildItem \"$env:ProgramData\\Microsoft\\Windows\\Start Menu\\Programs\",\"$env:APPDATA\\Microsoft\\Windows\\Start Menu\\Programs\" -Recurse -Filter *.lnk -ErrorAction SilentlyContinue | Select-Object -First 60 | ForEach-Object { $_.FullName + '|' + $w.CreateShortcut($_.FullName).TargetPath }";
        let Ok(out) = std::process::Command::new("powershell").args(["-NoProfile", "-Command", script]).output() else {
            return;
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let (mut read, mut checked) = (0, 0);
        for line in text.lines() {
            let Some((lnk, windows)) = line.split_once('|') else { continue };
            checked += 1;
            if let Some(ours) = target(lnk) {
                read += 1;
                assert!(ours.eq_ignore_ascii_case(windows.trim()), "{lnk}: read {ours}, Windows says {windows}");
            }
        }
        eprintln!("{read} of {checked} Start menu shortcuts read without the shell");
    }

    #[test]
    fn not_a_shortcut() {
        assert_eq!(parse(b""), Link::default());
        assert_eq!(parse(&[0u8; 200]), Link::default());
    }

    /// a shortcut made by Windows (WScript.Shell, which writes it through IShellLink) reads back to its target
    #[test]
    fn reads_what_windows_wrote() {
        let dir = std::env::temp_dir().join(format!("dvfiles_lnk_{}", std::process::id()));
        // (Windows writes a relative path in the ANSI code page, so a name outside it is left to the shell)
        let target_dir = dir.join("Target folder");
        std::fs::create_dir_all(&target_dir).unwrap();
        let lnk = dir.join("to target.lnk");
        let script = format!(
            "$s = (New-Object -ComObject WScript.Shell).CreateShortcut('{}'); $s.TargetPath = '{}'; $s.Save()",
            lnk.display(),
            target_dir.display()
        );
        let made = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if made {
            assert_eq!(target(lnk.to_str().unwrap()).as_deref(), Some(target_dir.to_str().unwrap()));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
