// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! What Properties' Details and Security tabs show, read by dvfiles: a program's version information (from its
//! version resource), and an item's owner and permissions (from its security descriptor), read-only. Changing
//! permissions stays Windows' (its security settings): getting an access list right is not something to redo.
use crate::ui::wstr;
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HLOCAL};
use windows::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
use windows::Win32::Security::*;
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};

/// The version resource's fields, as Explorer's Details tab lists them ("File description", "File version", ...)
pub fn version_info(path: &str) -> Vec<(&'static str, String)> {
    let p = wstr(path);
    let mut out = Vec::new();
    unsafe {
        let n = GetFileVersionInfoSizeW(PCWSTR(p.as_ptr()), None);
        if n == 0 || n > 16 << 20 {
            return out;
        }
        let mut buf = vec![0u8; n as usize];
        if GetFileVersionInfoW(PCWSTR(p.as_ptr()), None, n, buf.as_mut_ptr() as *mut _).is_err() {
            return out;
        }
        let query = |sub: &str| -> Option<(*const u8, u32)> {
            let s = wstr(sub);
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            VerQueryValueW(buf.as_ptr() as *const _, PCWSTR(s.as_ptr()), &mut ptr, &mut len)
                .as_bool()
                .then_some((ptr as *const u8, len))
                .filter(|(p, l)| !p.is_null() && *l > 0)
        };
        // the first language the resource has, else US English in Unicode
        let lang = query(r"\VarFileInfo\Translation")
            .filter(|(_, l)| *l >= 4)
            .map(|(p, _)| {
                let w = std::slice::from_raw_parts(p as *const u16, 2);
                format!("{:04x}{:04x}", w[0], w[1])
            })
            .unwrap_or_else(|| "040904b0".to_string());
        let text = |name: &str| -> Option<String> {
            let (p, len) = query(&format!(r"\StringFileInfo\{lang}\{name}"))?;
            let w = std::slice::from_raw_parts(p as *const u16, len as usize);
            let s = String::from_utf16_lossy(w.split(|&c| c == 0).next().unwrap_or(&[])).trim().to_string();
            (!s.is_empty()).then_some(s)
        };
        // the version numbers themselves, from the fixed part
        let fixed = query(r"\")
            .filter(|(_, l)| {
                *l as usize >= std::mem::size_of::<windows::Win32::Storage::FileSystem::VS_FIXEDFILEINFO>()
            })
            .map(|(p, _)| {
                let f = &*(p as *const windows::Win32::Storage::FileSystem::VS_FIXEDFILEINFO);
                (
                    format!(
                        "{}.{}.{}.{}",
                        f.dwFileVersionMS >> 16,
                        f.dwFileVersionMS & 0xFFFF,
                        f.dwFileVersionLS >> 16,
                        f.dwFileVersionLS & 0xFFFF
                    ),
                    format!(
                        "{}.{}.{}.{}",
                        f.dwProductVersionMS >> 16,
                        f.dwProductVersionMS & 0xFFFF,
                        f.dwProductVersionLS >> 16,
                        f.dwProductVersionLS & 0xFFFF
                    ),
                )
            });
        for (label, key) in [("File description", "FileDescription"), ("Product name", "ProductName")] {
            if let Some(v) = text(key) {
                out.push((label, v));
            }
        }
        if let Some((file, product)) = &fixed {
            out.push(("File version", file.clone()));
            out.push(("Product version", text("ProductVersion").unwrap_or_else(|| product.clone())));
        }
        for (label, key) in
            [("Company", "CompanyName"), ("Copyright", "LegalCopyright"), ("Original name", "OriginalFilename")]
        {
            if let Some(v) = text(key) {
                out.push((label, v));
            }
        }
    }
    out
}

/// What the Details tab lists for a file: a program's version information; more kinds as dvfiles learns to read them
pub fn details(path: &str) -> Vec<(String, String)> {
    let ext = dvfiles::naming::ext_lower(path);
    let mut out: Vec<(String, String)> = Vec::new();
    if matches!(ext.as_str(), "exe" | "dll" | "sys" | "ocx" | "cpl" | "scr" | "drv" | "efi" | "mui" | "ax" | "tlb") {
        out.extend(version_info(path).into_iter().map(|(k, v)| (k.to_string(), v)));
    }
    // reading a file that is only in the cloud would download it
    let local = dvfiles::fsx::stat(path).is_ok_and(|e| !dvfiles::fsx::is_cloud_only(e.attrs));
    if local && is_media(&ext) {
        if let Some(m) = std::fs::File::open(path).ok().and_then(|mut f| dvfiles::media::read(&mut f, &ext)) {
            out.extend(media_rows(&m));
        }
    }
    out
}

fn is_media(ext: &str) -> bool {
    matches!(
        ext,
        "wav"
            | "mp3"
            | "mp4"
            | "m4a"
            | "m4v"
            | "mov"
            | "3gp"
            | "3g2"
            | "flac"
            | "ogg"
            | "oga"
            | "opus"
            | "mkv"
            | "mka"
            | "webm"
            | "avi"
            | "wma"
            | "wmv"
            | "asf"
    )
}

/// a song's or video's facts, in Explorer's words
fn media_rows(m: &dvfiles::media::MediaInfo) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(ms) = m.duration_ms {
        out.push(("Length".into(), dvfiles::media::format_duration(ms)));
    }
    if let (Some(w), Some(h)) = (m.width, m.height) {
        out.push(("Frame size".into(), format!("{w} \u{00D7} {h}")));
    }
    if let Some(b) = m.bitrate_kbps {
        out.push(("Bit rate".into(), format!("{b} kbps")));
    }
    if let Some(r) = m.sample_rate {
        let khz = if r % 1000 == 0 { format!("{} kHz", r / 1000) } else { format!("{:.1} kHz", r as f64 / 1000.0) };
        out.push(("Audio sample rate".into(), khz));
    }
    if let Some(c) = m.channels {
        let words = match c {
            1 => "1 (mono)".to_string(),
            2 => "2 (stereo)".to_string(),
            n => n.to_string(),
        };
        out.push(("Audio channels".into(), words));
    }
    out
}

/// who owns an item, and who may do what with it
pub struct Security {
    pub owner: String,
    /// (who, "Allow: Full control" / "Deny: Write" ...), in the order the list has them
    pub entries: Vec<(String, String)>,
}

/// an account's name, "DOMAIN\name" (or "BUILTIN\Users", "NT AUTHORITY\SYSTEM"), else its SID
unsafe fn account(sid: PSID) -> String {
    let (mut n, mut d) = (256u32, 256u32);
    let (mut name, mut domain) = (vec![0u16; 256], vec![0u16; 256]);
    let mut kind = SID_NAME_USE::default();
    if LookupAccountSidW(
        PCWSTR::null(),
        sid,
        Some(PWSTR(name.as_mut_ptr())),
        &mut n,
        Some(PWSTR(domain.as_mut_ptr())),
        &mut d,
        &mut kind,
    )
    .is_ok()
    {
        let name = String::from_utf16_lossy(&name[..n as usize]);
        let domain = String::from_utf16_lossy(&domain[..d as usize]);
        return if domain.is_empty() { name } else { format!(r"{domain}\{name}") };
    }
    let mut s = PWSTR::null();
    if windows::Win32::Security::Authorization::ConvertSidToStringSidW(sid, &mut s).is_ok() {
        let out = s.to_string().unwrap_or_default();
        let _ = LocalFree(Some(HLOCAL(s.0 as *mut _)));
        // an account that no longer exists, as Explorer writes it
        return format!("Unknown account ({out})");
    }
    "(unknown)".into()
}

/// what an access mask allows, in the words of Windows' Security tab
pub fn access_words(mask: u32) -> String {
    // generic rights as the file rights they stand for
    let mut m = mask & 0x01FF_FFFF;
    if mask & 0x1000_0000 != 0 {
        m |= 0x1F01FF;
    }
    if mask & 0x8000_0000 != 0 {
        m |= 0x120089;
    }
    if mask & 0x4000_0000 != 0 {
        m |= 0x100116;
    }
    if mask & 0x2000_0000 != 0 {
        m |= 0x1200A0;
    }
    let has = |bits: u32| m & bits == bits;
    if has(0x1F01FF) {
        return "Full control".into();
    }
    if has(0x1301BF) {
        return "Modify".into();
    }
    let mut parts = Vec::new();
    if has(0x1200A9) {
        parts.push("Read & execute");
    } else if has(0x120089) {
        parts.push("Read");
    }
    if has(0x100116) {
        parts.push("Write");
    }
    if parts.is_empty() {
        "Special permissions".into()
    } else {
        parts.join(", ")
    }
}

pub fn security(path: &str) -> Option<Security> {
    let p = wstr(path);
    unsafe {
        let (mut owner, mut dacl, mut sd) =
            (PSID::default(), std::ptr::null_mut::<ACL>(), PSECURITY_DESCRIPTOR::default());
        let r = GetNamedSecurityInfoW(
            PCWSTR(p.as_ptr()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            Some(&mut dacl),
            None,
            &mut sd,
        );
        if r != ERROR_SUCCESS {
            return None;
        }
        let out = (|| {
            let owner = if owner.0.is_null() { "(unknown)".into() } else { account(owner) };
            let mut entries: Vec<(String, bool, u32)> = Vec::new();
            if dacl.is_null() {
                // no list at all: everyone may do everything
                return Some(Security { owner, entries: vec![("Everyone".into(), "Allow: Full control".into())] });
            }
            let mut info = ACL_SIZE_INFORMATION::default();
            GetAclInformation(
                dacl,
                &mut info as *mut _ as *mut _,
                std::mem::size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
            .ok()?;
            for i in 0..info.AceCount.min(256) {
                let mut ace = std::ptr::null_mut();
                if GetAce(dacl, i, &mut ace).is_err() || ace.is_null() {
                    continue;
                }
                let header = &*(ace as *const ACE_HEADER);
                // only allow and deny entries, and only those that apply to the item itself
                let allow = match header.AceType as u32 {
                    0 => true,
                    1 => false,
                    _ => continue,
                };
                if header.AceFlags as u32 & 0x08 != 0 {
                    continue;
                }
                let a = &*(ace as *const ACCESS_ALLOWED_ACE);
                let sid = PSID(&a.SidStart as *const u32 as *mut _);
                let who = account(sid);
                match entries.iter_mut().find(|(w, al, _)| *w == who && *al == allow) {
                    Some(e) => e.2 |= a.Mask,
                    None => entries.push((who, allow, a.Mask)),
                }
            }
            let entries = entries
                .into_iter()
                .map(|(who, allow, mask)| {
                    (who, format!("{}: {}", if allow { "Allow" } else { "Deny" }, access_words(mask)))
                })
                .collect();
            Some(Security { owner, entries })
        })();
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sound_has_its_length() {
        let wav = std::env::var("SystemRoot").unwrap() + r"\Media\Windows Ding.wav";
        if !std::path::Path::new(&wav).exists() {
            return;
        }
        let d = details(&wav);
        assert!(d.iter().any(|(k, v)| k == "Length" && v.starts_with("00:00:")), "{d:?}");
        assert!(d.iter().any(|(k, _)| k == "Audio sample rate"), "{d:?}");
    }

    #[test]
    fn rights_in_words() {
        assert_eq!(access_words(0x1F01FF), "Full control");
        assert_eq!(access_words(0x1000_0000), "Full control");
        assert_eq!(access_words(0x1301BF), "Modify");
        assert_eq!(access_words(0x1200A9), "Read & execute");
        assert_eq!(access_words(0x120089), "Read");
        assert_eq!(access_words(0x1200A9 | 0x100116), "Read & execute, Write");
        assert_eq!(access_words(0xA000_0000), "Read & execute");
        assert_eq!(access_words(0x40), "Special permissions");
    }

    #[test]
    fn a_program_and_a_file() {
        let exe = std::env::var("SystemRoot").unwrap() + r"\System32\notepad.exe";
        let v = version_info(&exe);
        let get = |k: &str| v.iter().find(|(l, _)| *l == k).map(|(_, v)| v.clone());
        assert!(get("File version").is_some_and(|v| v.split('.').count() == 4), "{v:?}");
        assert!(get("Product name").is_some_and(|p| p.contains("Windows")), "{v:?}");
        // a file of ours: owned by us, and we may at least read it
        let f = std::env::temp_dir().join(format!("dvfiles_sec_{}.txt", std::process::id()));
        std::fs::write(&f, "x").unwrap();
        let s = security(f.to_str().unwrap()).expect("security");
        let me = std::env::var("USERNAME").unwrap().to_lowercase();
        assert!(s.owner.to_lowercase().ends_with(&me) || s.owner.contains("Administrators"), "owner {}", s.owner);
        assert!(!s.entries.is_empty());
        let _ = std::fs::remove_file(&f);
    }
}
