// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Path text: breadcrumb segments, parents, and what the address bar accepts. Paths are compared the Windows way,
//! case-insensitively.

/// `C:\Users\me` -> `[("C:\", "C:\"), ("Users", "C:\Users"), ("me", "C:\Users\me")]`;
/// `\\server\share\dir` -> `[("\\server\share", ...), ("dir", ...)]`
pub fn segments(path: &str) -> Vec<(String, String)> {
    let path = path.trim_end_matches('\\');
    let (root, rest) = split_root(path);
    let mut out = Vec::new();
    if root.is_empty() {
        return out;
    }
    let root_full = if root.ends_with(':') { format!("{root}\\") } else { root.to_string() };
    out.push((root_full.clone(), root_full.clone()));
    let mut acc = root_full.trim_end_matches('\\').to_string();
    for part in rest.split('\\').filter(|p| !p.is_empty()) {
        acc.push('\\');
        acc.push_str(part);
        out.push((part.to_string(), acc.clone()));
    }
    out
}

/// `("C:", "Users\me")` or `("\\server\share", "dir")`
pub fn split_root(path: &str) -> (&str, &str) {
    let b = path.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        return (&path[..2], path[2..].trim_start_matches('\\'));
    }
    if let Some(unc) = path.strip_prefix(r"\\") {
        let mut parts = unc.splitn(3, '\\');
        let (server, share) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        if !server.is_empty() && !share.is_empty() {
            let root_len = 2 + server.len() + 1 + share.len();
            return (&path[..root_len], path[root_len..].trim_start_matches('\\'));
        }
    }
    ("", path)
}

/// the folder above, or None at a drive or share root
pub fn parent(path: &str) -> Option<String> {
    let path = path.trim_end_matches('\\');
    let (root, rest) = split_root(path);
    if rest.is_empty() {
        return None;
    }
    match rest.rfind('\\') {
        Some(i) => Some(format!("{root}\\{}", &rest[..i])),
        None if root.ends_with(':') => Some(format!("{root}\\")),
        None => Some(root.to_string()),
    }
}

/// the last component (a drive root names itself)
pub fn file_name(path: &str) -> &str {
    let t = path.trim_end_matches('\\');
    let (root, rest) = split_root(t);
    if rest.is_empty() {
        return if root.is_empty() { t } else { root };
    }
    match rest.rfind('\\') {
        Some(i) => &rest[i + 1..],
        None => rest,
    }
}

pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('\\') {
        format!("{dir}{name}")
    } else {
        format!("{dir}\\{name}")
    }
}

fn norm(p: &str) -> String {
    p.trim_end_matches('\\').to_lowercase()
}

pub fn same(a: &str, b: &str) -> bool {
    norm(a) == norm(b)
}

/// true when `child` is `dir` or lies somewhere below it
pub fn is_within(child: &str, dir: &str) -> bool {
    let (c, d) = (norm(child), norm(dir));
    c == d || (c.starts_with(&d) && c.as_bytes().get(d.len()) == Some(&b'\\'))
}

/// What was typed into the address bar: quotes and spaces trimmed, `/` turned into `\`, `%VARS%` expanded,
/// and `C:` turned into `C:\`. Relative input is resolved against `cwd`.
pub fn address_input(text: &str, cwd: Option<&str>, env: impl Fn(&str) -> Option<String>) -> String {
    let t = text.trim().trim_matches('"').trim().replace('/', "\\");
    let mut out = String::with_capacity(t.len());
    let mut rest = t.as_str();
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 => match env(&after[..end]) {
                Some(v) => {
                    out.push_str(&v);
                    rest = &after[end + 1..];
                }
                None => {
                    out.push('%');
                    rest = after;
                }
            },
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    if out.len() == 2 && out.as_bytes()[1] == b':' {
        out.push('\\');
    }
    let absolute = split_root(&out).0.len() > 0;
    match cwd {
        Some(dir) if !absolute && !out.is_empty() => {
            let mut path = dir.trim_end_matches('\\').to_string();
            for part in out.split('\\').filter(|p| !p.is_empty() && *p != ".") {
                if part == ".." {
                    if let Some(p) = parent(&path) {
                        path = p.trim_end_matches('\\').to_string();
                    }
                } else {
                    path.push('\\');
                    path.push_str(part);
                }
            }
            if path.ends_with(':') {
                path.push('\\');
            }
            path
        }
        _ => out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breadcrumbs() {
        let s = segments(r"C:\Users\me\");
        assert_eq!(
            s,
            vec![
                (r"C:\".into(), r"C:\".into()),
                ("Users".into(), r"C:\Users".into()),
                ("me".into(), r"C:\Users\me".into())
            ]
        );
        assert_eq!(segments(r"D:\").len(), 1);
        let unc = segments(r"\\nas\media\films");
        assert_eq!(unc[0].1, r"\\nas\media");
        assert_eq!(unc[1], ("films".into(), r"\\nas\media\films".into()));
        assert!(segments("relative").is_empty());
    }

    #[test]
    fn parents() {
        assert_eq!(parent(r"C:\Users\me").as_deref(), Some(r"C:\Users"));
        assert_eq!(parent(r"C:\Users").as_deref(), Some(r"C:\"));
        assert_eq!(parent(r"C:\"), None);
        assert_eq!(parent(r"\\nas\media\films").as_deref(), Some(r"\\nas\media"));
        assert_eq!(parent(r"\\nas\media"), None);
    }

    #[test]
    fn names_and_joins() {
        assert_eq!(file_name(r"C:\Users\me"), "me");
        assert_eq!(file_name(r"C:\"), "C:");
        assert_eq!(file_name(r"\\nas\media"), r"\\nas\media");
        assert_eq!(file_name(r"\\nas\media\x"), "x");
        assert_eq!(join(r"C:\", "a"), r"C:\a");
        assert_eq!(join(r"C:\a", "b"), r"C:\a\b");
    }

    #[test]
    fn containment() {
        assert!(is_within(r"C:\a\b", r"c:\A"));
        assert!(is_within(r"C:\a", r"C:\a\"));
        assert!(!is_within(r"C:\ab", r"C:\a"));
        assert!(is_within(r"C:\a\b", r"C:\"));
        assert!(same(r"C:\X\", r"c:\x"));
    }

    #[test]
    fn address_bar() {
        let env = |k: &str| (k.eq_ignore_ascii_case("userprofile")).then(|| r"C:\Users\me".to_string());
        assert_eq!(address_input(" \"C:/Windows\" ", None, env), r"C:\Windows");
        assert_eq!(address_input("%USERPROFILE%\\Downloads", None, env), r"C:\Users\me\Downloads");
        assert_eq!(address_input("%NOPE%\\x", None, env), r"%NOPE%\x");
        assert_eq!(address_input("100%", None, env), "100%");
        assert_eq!(address_input("d:", None, env), r"d:\");
        assert_eq!(address_input(r"..\x", Some(r"C:\a\b"), env), r"C:\a\x");
        assert_eq!(address_input("sub", Some(r"C:\"), env), r"C:\sub");
        assert_eq!(address_input("..", Some(r"C:\a"), env), r"C:\");
    }
}
