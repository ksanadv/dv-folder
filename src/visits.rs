// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! The folders the user goes to, how often and how lately, so a few letters typed in the address bar jump to the
//! right one ("dl" to Downloads, "proj api" to C:\work\projects\api), as zoxide does in a terminal. Kept in
//! %APPDATA%\dvfiles\visits.txt, one folder a line: count, last visit (Unix seconds), path. No Windows dependencies.

/// the most folders remembered; the least used go first
const KEEP: usize = 500;

#[derive(Default)]
pub struct Visits {
    list: Vec<(String, u32, u64)>,
}

impl Visits {
    pub fn parse(text: &str) -> Visits {
        let list = text
            .lines()
            .filter_map(|l| {
                let mut it = l.splitn(3, '\t');
                let count = it.next()?.parse().ok()?;
                let last = it.next()?.parse().ok()?;
                let path = it.next()?.trim_end();
                (!path.is_empty()).then(|| (path.to_string(), count, last))
            })
            .collect();
        Visits { list }
    }

    pub fn text(&self) -> String {
        self.list.iter().map(|(p, c, t)| format!("{c}\t{t}\t{p}\n")).collect()
    }

    /// when `path` was last visited (Unix seconds)
    pub fn last(&self, path: &str) -> Option<u64> {
        self.list.iter().find(|(p, _, _)| p.eq_ignore_ascii_case(path)).map(|e| e.2)
    }

    /// a visit to `path` at `now`
    pub fn record(&mut self, path: &str, now: u64) {
        match self.list.iter_mut().find(|(p, _, _)| p.eq_ignore_ascii_case(path)) {
            Some(e) => {
                e.1 = e.1.saturating_add(1);
                e.2 = now;
            }
            None => self.list.push((path.to_string(), 1, now)),
        }
        if self.list.len() > KEEP {
            self.list.sort_by(|a, b| frecency(b.1, b.2, now).total_cmp(&frecency(a.1, a.2, now)));
            self.list.truncate(KEEP);
        }
    }

    /// The folders `query` names, best first: every word of it appears in the path in order, the last word in the
    /// folder's own name; ranked by how often and how lately each was visited.
    pub fn find(&self, query: &str, now: u64) -> Vec<String> {
        let words: Vec<String> = query.split_whitespace().map(|w| w.to_lowercase()).collect();
        if words.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<(f32, &str)> = self
            .list
            .iter()
            .filter_map(|(p, c, t)| {
                let lower = p.to_lowercase();
                let name = lower.rsplit(['\\', '/']).next().unwrap_or(&lower);
                let last = words.last()?;
                if !name.contains(last.as_str()) {
                    return None;
                }
                let mut at = 0;
                for w in &words {
                    at += lower[at..].find(w.as_str())? + w.len();
                }
                // a name that starts with what was typed, or is it, comes first among equals
                let bonus = if name == last {
                    3.0
                } else if name.starts_with(last.as_str()) {
                    1.5
                } else {
                    1.0
                };
                Some((frecency(*c, *t, now) * bonus, p.as_str()))
            })
            .collect();
        hits.sort_by(|a, b| b.0.total_cmp(&a.0));
        hits.into_iter().map(|(_, p)| p.to_string()).collect()
    }
}

/// how much a folder counts: its visits, worth more the more recent the last one
fn frecency(count: u32, last: u64, now: u64) -> f32 {
    let age = now.saturating_sub(last);
    let w = if age < 3600 {
        4.0
    } else if age < 86_400 {
        2.0
    } else if age < 7 * 86_400 {
        1.0
    } else {
        0.5
    };
    count as f32 * w
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jumps_to_the_folder_meant() {
        let mut v = Visits::default();
        let now = 10_000_000;
        for _ in 0..5 {
            v.record(r"C:\Users\me\Downloads", now - 100);
        }
        v.record(r"C:\work\projects\api", now - 10);
        v.record(r"C:\work\old\api-docs", now - 30 * 86_400);
        v.record(r"C:\Users\me\Documents", now - 50);
        assert_eq!(v.find("down", now).first().map(String::as_str), Some(r"C:\Users\me\Downloads"));
        assert_eq!(v.find("proj api", now), vec![r"C:\work\projects\api".to_string()]);
        assert_eq!(v.find("api", now).first().map(String::as_str), Some(r"C:\work\projects\api"));
        assert!(v.find("zzz", now).is_empty());
        // saved and read back the same
        let back = Visits::parse(&v.text());
        assert_eq!(back.find("doc", now), v.find("doc", now));
    }
}
