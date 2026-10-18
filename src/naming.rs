// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! File-name rules: what Windows accepts, and the names Explorer picks for copies, "keep both" and new items.

pub const INVALID_CHARS_MESSAGE: &str =
    "A file name can't contain any of the following characters:\n\\ / : * ? \" < > |";

/// `("report", ".pdf")`; a leading dot is part of the stem (`.gitignore` has no extension), folders have none.
pub fn split_ext(name: &str, is_dir: bool) -> (&str, &str) {
    if is_dir {
        return (name, "");
    }
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// lower-case extension without the dot, or ""
pub fn ext_lower(name: &str) -> String {
    let (_, ext) = split_ext(name, false);
    ext.trim_start_matches('.').to_lowercase()
}

fn reserved(stem: &str) -> bool {
    let base = stem.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
    matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (base.len() == 4
            && (base.starts_with("COM") || base.starts_with("LPT"))
            && matches!(base.as_bytes()[3], b'1'..=b'9'))
}

/// Checks a name typed by the user. Trailing spaces and dots are dropped the way Windows drops them.
pub fn validate(input: &str) -> Result<String, &'static str> {
    let name = input.trim_start_matches(' ').trim_end_matches([' ', '.']);
    if name.is_empty() {
        return Err("You must type a file name.");
    }
    if name.chars().any(|c| matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || (c as u32) < 32) {
        return Err(INVALID_CHARS_MESSAGE);
    }
    if reserved(name) {
        return Err("The specified device name is invalid.");
    }
    if name.encode_utf16().count() > 255 {
        return Err("The file name is too long.");
    }
    Ok(name.to_string())
}

/// `report - Copy.pdf`, then `report - Copy (2).pdf`, ... : the name for pasting into the same folder
pub fn copy_name(name: &str, is_dir: bool, exists: impl Fn(&str) -> bool) -> String {
    let (stem, ext) = split_ext(name, is_dir);
    let first = format!("{stem} - Copy{ext}");
    if !exists(&first) {
        return first;
    }
    (2..).map(|n| format!("{stem} - Copy ({n}){ext}")).find(|c| !exists(c)).expect("an unused name")
}

/// `report (2).pdf`, `report (3).pdf`, ... : "Keep both files" in the conflict dialog
pub fn numbered_name(name: &str, is_dir: bool, exists: impl Fn(&str) -> bool) -> String {
    let (stem, ext) = split_ext(name, is_dir);
    (2..).map(|n| format!("{stem} ({n}){ext}")).find(|c| !exists(c)).expect("an unused name")
}

/// `New folder`, then `New folder (2)`, ...
pub fn new_name(base: &str, ext: &str, exists: impl Fn(&str) -> bool) -> String {
    let first = format!("{base}{ext}");
    if !exists(&first) {
        return first;
    }
    (2..).map(|n| format!("{base} ({n}){ext}")).find(|c| !exists(c)).expect("an unused name")
}

/// What a name typed for several items at once asks for. `/` and `:` cannot be in a name, so they mark the two
/// other kinds: `old/new` replaces "old" with "new" in every name (any case), `:lower`, `:upper` and `:title` change
/// the names' case.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Batch<'a> {
    /// `#` (or `##`, `###` for that many digits) is a running number from 1; `*` is each item's own name; with neither,
    /// they are numbered as Explorer numbers them ("Holiday (1).jpg", "Holiday (2).jpg")
    Pattern(&'a str),
    Replace(&'a str, &'a str),
    Case(Case),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Case {
    Lower,
    Upper,
    Title,
}

impl<'a> Batch<'a> {
    pub fn parse(text: &'a str) -> Batch<'a> {
        match text.trim().to_ascii_lowercase().as_str() {
            ":lower" => return Batch::Case(Case::Lower),
            ":upper" => return Batch::Case(Case::Upper),
            ":title" => return Batch::Case(Case::Title),
            _ => {}
        }
        match text.split_once('/') {
            Some((from, to)) if !from.is_empty() => Batch::Replace(from, to),
            _ => Batch::Pattern(text),
        }
    }
}

/// "my holiday_photo" -> "My Holiday_Photo": a capital after the start, a space, `_`, `-` or `.`
fn title_case(s: &str) -> String {
    let mut up = true;
    s.chars()
        .flat_map(|c| {
            let out: Vec<char> = if up { c.to_uppercase().collect() } else { c.to_lowercase().collect() };
            up = matches!(c, ' ' | '_' | '-' | '.');
            out
        })
        .collect()
}

/// every `from` in `s` (ASCII case ignored, which keeps byte offsets) replaced by `to`
fn replace_any_case(s: &str, from: &str, to: &str) -> String {
    let (hay, needle) = (s.to_ascii_lowercase(), from.to_ascii_lowercase());
    let (mut out, mut at) = (String::with_capacity(s.len()), 0);
    while let Some(i) = hay[at..].find(&needle) {
        out.push_str(&s[at..at + i]);
        out.push_str(to);
        at += i + needle.len();
    }
    out.push_str(&s[at..]);
    out
}

/// The new names for several items renamed at once, in the order given (name, is a folder). Files keep their own
/// extensions.
pub fn batch_names(batch: Batch, items: &[(String, bool)]) -> Vec<String> {
    let n = items.len();
    items
        .iter()
        .enumerate()
        .map(|(k, (old, is_dir))| {
            let (stem, ext) = split_ext(old, *is_dir);
            let base = match batch {
                Batch::Pattern(pattern) => {
                    if let Some(at) = pattern.find('#') {
                        let run = pattern[at..].chars().take_while(|&c| c == '#').count();
                        let width = run.max(if run == 1 { 1 } else { n.to_string().len() });
                        format!("{}{:0width$}{}", &pattern[..at], k + 1, &pattern[at + run..])
                    } else if pattern.contains('*') {
                        pattern.replace('*', stem)
                    } else {
                        format!("{pattern} ({})", k + 1)
                    }
                }
                Batch::Replace(from, to) => replace_any_case(stem, from, to),
                Batch::Case(Case::Lower) => stem.to_lowercase(),
                Batch::Case(Case::Upper) => stem.to_uppercase(),
                Batch::Case(Case::Title) => title_case(stem),
            };
            format!("{base}{ext}")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn set(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_lowercase()).collect()
    }

    #[test]
    fn extensions() {
        assert_eq!(split_ext("report.final.pdf", false), ("report.final", ".pdf"));
        assert_eq!(split_ext(".gitignore", false), (".gitignore", ""));
        assert_eq!(split_ext("archive.", false), ("archive.", ""));
        assert_eq!(split_ext("my.folder", true), ("my.folder", ""));
        assert_eq!(ext_lower("Photo.JPG"), "jpg");
        assert_eq!(ext_lower("README"), "");
    }

    #[test]
    fn validation() {
        assert_eq!(validate("  notes.txt  "), Ok("notes.txt".into()));
        assert_eq!(validate("name..."), Ok("name".into()));
        assert!(validate("").is_err());
        assert!(validate(" . ").is_err());
        assert!(validate("a/b").is_err());
        assert!(validate("what?").is_err());
        assert!(validate("CON").is_err());
        assert!(validate("com1.txt").is_err());
        assert!(validate("LPT9").is_err());
        assert!(validate("COM0").is_ok());
        assert!(validate("console").is_ok());
        assert!(validate(&"x".repeat(256)).is_err());
    }

    #[test]
    fn copies_like_explorer() {
        let taken = set(&["a.txt", "a - copy.txt", "a - copy (2).txt"]);
        let exists = |n: &str| taken.contains(&n.to_lowercase());
        assert_eq!(copy_name("b.txt", false, exists), "b - Copy.txt");
        assert_eq!(copy_name("a.txt", false, exists), "a - Copy (3).txt");
        assert_eq!(copy_name("folder.v2", true, exists), "folder.v2 - Copy");
    }

    #[test]
    fn keep_both_and_new() {
        let taken = set(&["a.txt", "a (2).txt", "new folder", "new folder (2)"]);
        let exists = |n: &str| taken.contains(&n.to_lowercase());
        assert_eq!(numbered_name("a.txt", false, exists), "a (3).txt");
        assert_eq!(new_name("New folder", "", exists), "New folder (3)");
        assert_eq!(new_name("New Text Document", ".txt", exists), "New Text Document.txt");
    }
}

#[cfg(test)]
mod batch_tests {
    use super::{batch_names, Batch, Case};

    #[test]
    fn names_for_many() {
        let items = vec![("a.jpg".to_string(), false), ("b.png".to_string(), false), ("Old".to_string(), true)];
        let p = |t| batch_names(Batch::parse(t), &items);
        assert_eq!(p("Holiday #"), ["Holiday 1.jpg", "Holiday 2.png", "Holiday 3"]);
        assert_eq!(p("img_###"), ["img_001.jpg", "img_002.png", "img_003"]);
        assert_eq!(p("2026 *"), ["2026 a.jpg", "2026 b.png", "2026 Old"]);
        assert_eq!(p("Trip"), ["Trip (1).jpg", "Trip (2).png", "Trip (3)"]);
        assert_eq!(Batch::parse(":Upper"), Batch::Case(Case::Upper));
        let items = vec![("IMG_photo Of me.JPG".to_string(), false), ("my-trip.png".to_string(), false)];
        assert_eq!(batch_names(Batch::parse("img_/"), &items), ["photo Of me.JPG", "my-trip.png"]);
        assert_eq!(batch_names(Batch::parse("trip/Holiday"), &items)[1], "my-Holiday.png");
        assert_eq!(batch_names(Batch::parse(":lower"), &items)[0], "img_photo of me.JPG");
        assert_eq!(batch_names(Batch::parse(":title"), &items), ["Img_Photo Of Me.JPG", "My-Trip.png"]);
    }
}
