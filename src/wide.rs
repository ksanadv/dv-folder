// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
use windows::core::PCWSTR;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub fn from_wide(p: &[u16]) -> String {
    let end = p.iter().position(|&c| c == 0).unwrap_or(p.len());
    String::from_utf16_lossy(&p[..end])
}

/// A NUL-terminated path for the file APIs, with the `\\?\` prefix on absolute paths so nothing is limited to
/// MAX_PATH and Windows skips re-parsing the path. Engine paths always come from listings, so they never hold the
/// `.`/`..` parts that the prefix would leave unresolved.
pub fn long(path: &str) -> Vec<u16> {
    let b = path.as_bytes();
    let mut out: Vec<u16> = Vec::with_capacity(path.len() + 9);
    if path.starts_with(r"\\?\") || path.starts_with(r"\\.\") {
        out.extend(path.encode_utf16());
    } else if let Some(unc) = path.strip_prefix(r"\\") {
        out.extend(r"\\?\UNC\".encode_utf16());
        out.extend(unc.encode_utf16());
    } else if b.len() >= 2 && b[1] == b':' {
        out.extend(r"\\?\".encode_utf16());
        out.extend(path.encode_utf16());
        // `\\?\C:` names the volume device, not its root folder
        if b.len() == 2 {
            out.push(b'\\' as u16);
        }
    } else {
        out.extend(path.encode_utf16());
    }
    out.push(0);
    out
}

pub fn pcwstr(v: &[u16]) -> PCWSTR {
    PCWSTR(v.as_ptr())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_paths() {
        assert_eq!(from_wide(&long(r"C:\a")), r"\\?\C:\a");
        assert_eq!(from_wide(&long("C:")), r"\\?\C:\");
        assert_eq!(from_wide(&long(r"\\srv\share\x")), r"\\?\UNC\srv\share\x");
        assert_eq!(from_wide(&long(r"\\?\C:\a")), r"\\?\C:\a");
        assert_eq!(from_wide(&wide("é")), "é");
    }
}
