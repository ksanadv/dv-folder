// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! The search box language. Words must all appear in the name (any order, any case); `*` and `?` make a word a
//! wildcard pattern for the whole name; `"quoted words"` keep their spaces; `-word` excludes. Filters:
//! `ext:pdf;docx`, `type:folder` / `type:file`, `size:>10MB`, `size:<1KB`, `size:1MB..1GB`, and `modified:24h`,
//! `modified:7d`, `modified:2w`, `modified:today` for what changed in that time.
//!
//! Matching works on the UTF-16 names the directory listing returns, so a search over a million files never builds
//! a `String` for a name that does not match.
use std::sync::OnceLock;

fn fold_table() -> &'static [u16] {
    static TABLE: OnceLock<Box<[u16]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        (0..=0xFFFFu32)
            .map(|u| {
                let Some(c) = char::from_u32(u) else { return u as u16 };
                let mut low = c.to_lowercase();
                match (low.next(), low.next()) {
                    (Some(l), None) if (l as u32) <= 0xFFFF => l as u32 as u16,
                    _ => u as u16,
                }
            })
            .collect()
    })
}

#[inline]
fn fold(t: &[u16], c: u16) -> u16 {
    if c < 128 {
        (c as u8).to_ascii_lowercase() as u16
    } else {
        t[c as usize]
    }
}

fn folded(s: &str) -> Vec<u16> {
    let t = fold_table();
    s.encode_utf16().map(|c| fold(t, c)).collect()
}

#[derive(Debug, Clone, PartialEq)]
enum Word {
    Contains(Vec<u16>),
    Glob(Vec<u16>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Any,
    Folder,
    File,
}

#[derive(Debug, Clone, Default)]
pub struct Query {
    words: Vec<(Word, bool)>,
    exts: Vec<Vec<u16>>,
    kind: Option<Kind>,
    min_size: Option<u64>,
    max_size: Option<u64>,
    // changed at or after this time (FILETIME)
    since: Option<u64>,
}

/// `24h`, `7d`, `2w`, `today` as a span in seconds
fn parse_span(s: &str) -> Option<u64> {
    if s == "today" {
        return Some(86_400);
    }
    let split = s.find(|c: char| !c.is_ascii_digit())?;
    let n: u64 = s[..split].parse().ok()?;
    let unit = match &s[split..] {
        "m" | "min" => 60,
        "h" | "hour" | "hours" => 3600,
        "d" | "day" | "days" => 86_400,
        "w" | "week" | "weeks" => 7 * 86_400,
        _ => return None,
    };
    Some(n * unit)
}

/// now as a FILETIME (100 ns ticks since 1601)
pub fn filetime_now() -> u64 {
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    (unix + 11_644_473_600) * 10_000_000
}

fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().ok()?;
    let mult = match unit.trim() {
        "" | "b" | "bytes" => 1u64,
        "k" | "kb" => 1 << 10,
        "m" | "mb" => 1 << 20,
        "g" | "gb" => 1 << 30,
        "t" | "tb" => 1 << 40,
        _ => return None,
    };
    Some((n * mult as f64) as u64)
}

fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

impl Query {
    pub fn parse(text: &str) -> Query {
        let mut q = Query::default();
        for tok in tokens(text) {
            let lower = tok.to_lowercase();
            if let Some(v) = lower.strip_prefix("ext:") {
                q.exts.extend(
                    v.split([';', ',', '|']).filter(|e| !e.is_empty()).map(|e| folded(e.trim_start_matches('.'))),
                );
                continue;
            }
            if let Some(v) = lower.strip_prefix("type:").or_else(|| lower.strip_prefix("kind:")) {
                q.kind = match v {
                    "folder" | "folders" | "dir" | "directory" => Some(Kind::Folder),
                    "file" | "files" => Some(Kind::File),
                    _ => Some(Kind::Any),
                };
                continue;
            }
            if let Some(v) = lower.strip_prefix("modified:").or_else(|| lower.strip_prefix("changed:")) {
                q.since =
                    parse_span(v.trim_start_matches(['<', '='])).map(|s| filetime_now().saturating_sub(s * 10_000_000));
                continue;
            }
            if let Some(v) = lower.strip_prefix("size:") {
                if let Some((a, b)) = v.split_once("..") {
                    q.min_size = parse_size(a);
                    q.max_size = parse_size(b);
                } else if let Some(n) = v.strip_prefix(">=").or_else(|| v.strip_prefix('>')) {
                    q.min_size = parse_size(n);
                } else if let Some(n) = v.strip_prefix("<=").or_else(|| v.strip_prefix('<')) {
                    q.max_size = parse_size(n);
                } else if let Some(n) = parse_size(v) {
                    q.min_size = Some(n);
                    q.max_size = Some(n);
                }
                continue;
            }
            let (neg, body) = match tok.strip_prefix('-') {
                Some(rest) if !rest.is_empty() => (true, rest),
                _ => (false, tok.as_str()),
            };
            let f = folded(body);
            let word = if body.contains(['*', '?']) { Word::Glob(f) } else { Word::Contains(f) };
            q.words.push((word, neg));
        }
        q
    }

    /// True when every name this query matches is also matched by `old`, so its results can be picked out of old's
    /// finished results instead of searching the disk again ("note" -> "notepad", "a" -> "a b"). Conservative: each
    /// of old's plain words must be contained in one of this query's plain words, and old's excluded words, wildcard
    /// patterns and filters must all be here unchanged.
    pub fn narrows(&self, old: &Query) -> bool {
        if self.exts != old.exts
            || self.kind != old.kind
            || self.min_size != old.min_size
            || self.max_size != old.max_size
            || self.since != old.since
        {
            return false;
        }
        old.words.iter().all(|(w, neg)| match (w, neg) {
            (Word::Contains(o), false) => self
                .words
                .iter()
                .any(|(nw, nneg)| !nneg && matches!(nw, Word::Contains(n) if n.windows(o.len().max(1)).any(|win| win == o.as_slice()) || o.is_empty())),
            _ => self.words.iter().any(|(nw, nneg)| nw == w && nneg == neg),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
            && self.exts.is_empty()
            && self.kind.is_none()
            && self.min_size.is_none()
            && self.max_size.is_none()
            && self.since.is_none()
    }

    /// whether an item changed at `modified` (FILETIME) is recent enough for a `modified:` filter
    pub fn fresh(&self, modified: u64) -> bool {
        self.since.is_none_or(|s| modified >= s)
    }

    /// whether it filters by size
    pub fn has_size(&self) -> bool {
        self.min_size.is_some() || self.max_size.is_some()
    }

    /// the same query without its size filters (for matching names where sizes are not known)
    pub fn without_size(&self) -> Query {
        Query { min_size: None, max_size: None, ..self.clone() }
    }

    /// Lower-case ASCII pieces every matching name must contain: the plain words, and the literal runs of wildcard
    /// patterns. Words with other characters are left out (a name is then matched in full anyway), so the list may
    /// be empty.
    pub fn ascii_needles(&self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for (w, neg) in &self.words {
            if *neg {
                continue;
            }
            let units = match w {
                Word::Contains(p) | Word::Glob(p) => p,
            };
            for run in units.split(|&c| matches!(w, Word::Glob(_)) && (c == b'*' as u16 || c == b'?' as u16)) {
                if !run.is_empty() && run.iter().all(|&c| c < 128) {
                    out.push(run.iter().map(|&c| c as u8).collect());
                }
            }
        }
        out
    }

    /// the size filters apply to files only; folders have no size in a listing
    pub fn matches(&self, name: &[u16], is_dir: bool, size: u64) -> bool {
        match self.kind {
            Some(Kind::Folder) if !is_dir => return false,
            Some(Kind::File) if is_dir => return false,
            _ => {}
        }
        if (self.min_size.is_some() || self.max_size.is_some()) && is_dir {
            return false;
        }
        if self.min_size.is_some_and(|m| size < m) || self.max_size.is_some_and(|m| size > m) {
            return false;
        }
        let t = fold_table();
        if !self.exts.is_empty() {
            if is_dir {
                return false;
            }
            let dot = name.iter().rposition(|&c| c == b'.' as u16);
            let Some(dot) = dot.filter(|&d| d > 0) else { return false };
            let ext = &name[dot + 1..];
            let hit =
                self.exts.iter().any(|e| e.len() == ext.len() && e.iter().zip(ext).all(|(&a, &b)| a == fold(t, b)));
            if !hit {
                return false;
            }
        }
        self.words.iter().all(|(w, neg)| {
            let hit = match w {
                Word::Contains(p) => contains(t, name, p),
                Word::Glob(p) => glob(t, name, p),
            };
            hit != *neg
        })
    }

    pub fn matches_str(&self, name: &str, is_dir: bool, size: u64) -> bool {
        let units: Vec<u16> = name.encode_utf16().collect();
        self.matches(&units, is_dir, size)
    }
}

fn contains(t: &[u16], hay: &[u16], needle: &[u16]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > hay.len() {
        return false;
    }
    let first = needle[0];
    (0..=hay.len() - needle.len())
        .any(|i| fold(t, hay[i]) == first && needle[1..].iter().zip(&hay[i + 1..]).all(|(&n, &h)| n == fold(t, h)))
}

// whole-name wildcard match; iterative with one backtrack point, so it is linear for patterns like *a*b*
fn glob(t: &[u16], name: &[u16], pat: &[u16]) -> bool {
    let (star, quest) = (b'*' as u16, b'?' as u16);
    let (mut n, mut p) = (0usize, 0usize);
    let mut back: Option<(usize, usize)> = None;
    while n < name.len() {
        if p < pat.len() && (pat[p] == quest || pat[p] == fold(t, name[n])) {
            n += 1;
            p += 1;
        } else if p < pat.len() && pat[p] == star {
            back = Some((p, n));
            p += 1;
        } else if let Some((bp, bn)) = back {
            p = bp + 1;
            n = bn + 1;
            back = Some((bp, bn + 1));
        } else {
            return false;
        }
    }
    pat[p..].iter().all(|&c| c == star)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(q: &str, name: &str) -> bool {
        Query::parse(q).matches_str(name, false, 1000)
    }

    #[test]
    fn needles() {
        let n = |q: &str| Query::parse(q).ascii_needles();
        assert_eq!(n("Report 2024"), vec![b"report".to_vec(), b"2024".to_vec()]);
        assert_eq!(n("*.tmp -old"), vec![b".tmp".to_vec()]);
        assert_eq!(n("a?c* école"), vec![b"a".to_vec(), b"c".to_vec()]);
        assert!(n("ext:pdf").is_empty());
    }

    #[test]
    fn words_any_order_any_case() {
        assert!(m("report", "Annual REPORT 2024.pdf"));
        assert!(m("2024 annual", "Annual REPORT 2024.pdf"));
        assert!(!m("2023 annual", "Annual REPORT 2024.pdf"));
        assert!(m("ÉCOLE", "photos école.jpg"));
        assert!(m("", "anything"));
    }

    #[test]
    fn wildcards_match_whole_name() {
        assert!(m("*.pdf", "a.pdf"));
        assert!(!m("*.pdf", "a.pdf.exe"));
        assert!(m("IMG_??12*", "img_0012.jpg"));
        assert!(m("*a*b*c*", "xxaxxbxxcxx"));
        assert!(!m("*a*b*c", "xxaxxbxxcxx"));
        assert!(m("*", ""));
    }

    #[test]
    fn quotes_and_negation() {
        assert!(m("\"annual report\"", "The Annual Report.docx"));
        assert!(!m("\"annual report\"", "Annual summary report.docx"));
        assert!(m("report -draft", "report final.docx"));
        assert!(!m("report -draft", "report DRAFT.docx"));
    }

    #[test]
    fn narrowing_is_only_claimed_when_it_holds() {
        let n = |new: &str, old: &str| Query::parse(new).narrows(&Query::parse(old));
        assert!(n("notepad", "note"));
        assert!(n("Note pad", "note"));
        assert!(n("a b c", "a b"));
        assert!(n("report -draft", "report"));
        assert!(n("ext:pdf report 2024", "ext:pdf report"));
        assert!(n("*.pdf annual", "*.pdf"));
        assert!(!n("note", "notepad"), "shorter word widens");
        assert!(!n("a", "a b"), "dropping a word widens");
        assert!(!n("report", "report -draft"), "dropping an exclusion widens");
        assert!(!n("report -dra", "report -draft"), "a different exclusion is not narrower");
        assert!(!n("*.pdfx", "*.pdf"), "patterns are compared as they are");
        assert!(!n("report ext:pdf", "report"), "filters must match");
        assert!(!n("size:>1MB a", "a"));
        // whatever narrows really is a subset on sample names
        let names = ["notepad.exe", "notes.txt", "Note Pad.lnk", "report draft.pdf", "report 2024.pdf", "a b c", "a b"];
        for (new, old) in [("notepad", "note"), ("a b c", "a b"), ("report -draft", "report")] {
            let (qn, qo) = (Query::parse(new), Query::parse(old));
            for name in names {
                if qn.matches_str(name, false, 1) {
                    assert!(qo.matches_str(name, false, 1), "{name}: {new} vs {old}");
                }
            }
        }
    }

    #[test]
    fn filters() {
        let q = Query::parse("ext:pdf;DOCX");
        assert!(q.matches_str("a.PDF", false, 1));
        assert!(q.matches_str("b.docx", false, 1));
        assert!(!q.matches_str("c.doc", false, 1));
        assert!(!q.matches_str("pdf", false, 1));
        assert!(!q.matches_str("x.pdf", true, 0));
        let folders = Query::parse("type:folder src");
        assert!(folders.matches_str("src", true, 0));
        assert!(!folders.matches_str("src.rs", false, 10));
        let big = Query::parse("size:>1MB");
        assert!(big.matches_str("x", false, 2 << 20));
        assert!(!big.matches_str("x", false, 1000));
        assert!(!big.matches_str("dir", true, 0));
        let range = Query::parse("size:1kb..2kb");
        assert!(range.matches_str("x", false, 1500));
        assert!(!range.matches_str("x", false, 3000));
        assert!(Query::parse("  ").is_empty());
        assert!(!Query::parse("ext:txt").is_empty());
    }
}
