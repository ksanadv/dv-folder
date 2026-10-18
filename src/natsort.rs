// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Name order as Explorer shows it: case-insensitive, runs of digits compared by value (`file2` before `file10`),
//! punctuation before digits before letters. Ties are broken ordinally so the order is total and stable.
use std::cmp::Ordering;

fn class(c: char) -> u8 {
    if c.is_alphabetic() {
        2
    } else if c.is_numeric() {
        1
    } else {
        0
    }
}

fn fold(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    let l = c.to_lowercase().next().unwrap_or(c);
    // accented Latin letters sort with their base letter, as Windows' linguistic order does (é with e, not after z)
    match l {
        'à'..='å' | 'ā' | 'ă' | 'ą' => 'a',
        'ç' | 'ć' | 'č' => 'c',
        'ď' => 'd',
        'è'..='ë' | 'ē' | 'ė' | 'ę' | 'ě' => 'e',
        'ì'..='ï' | 'ī' | 'į' => 'i',
        'ł' => 'l',
        'ñ' | 'ń' | 'ň' => 'n',
        'ò'..='ö' | 'ø' | 'ō' | 'ő' => 'o',
        'ř' => 'r',
        'ś' | 'š' | 'ş' => 's',
        'ť' => 't',
        'ù'..='ü' | 'ū' | 'ů' | 'ű' => 'u',
        'ý' | 'ÿ' => 'y',
        'ź' | 'ż' | 'ž' => 'z',
        _ => l,
    }
}

pub fn cmp(a: &str, b: &str) -> Ordering {
    let (a_b, b_b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0usize, 0usize);
    // set when two digit runs had the same value but different leading zeros
    let mut zeros = Ordering::Equal;
    loop {
        if i >= a_b.len() || j >= b_b.len() {
            let ends = (a_b.len() - i).cmp(&(b_b.len() - j));
            if ends != Ordering::Equal {
                return ends;
            }
            break;
        }
        let (ca, cb) = (a_b[i], b_b[j]);
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            let si = i;
            while i < a_b.len() && a_b[i].is_ascii_digit() {
                i += 1;
            }
            let sj = j;
            while j < b_b.len() && b_b[j].is_ascii_digit() {
                j += 1;
            }
            let (ra, rb) = (&a_b[si..i], &b_b[sj..j]);
            let (ta, tb) = (trim_zeros(ra), trim_zeros(rb));
            let by_value = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
            if by_value != Ordering::Equal {
                return by_value;
            }
            if zeros == Ordering::Equal {
                // fewer leading zeros first: "1" before "01"
                zeros = ra.len().cmp(&rb.len());
            }
            continue;
        }
        // decode one char from each side (ASCII fast path)
        let (xa, la) = decode(&a[i..]);
        let (xb, lb) = decode(&b[j..]);
        let (fa, fb) = (fold(xa), fold(xb));
        if fa != fb {
            let by_class = class(fa).cmp(&class(fb));
            if by_class != Ordering::Equal {
                return by_class;
            }
            return fa.cmp(&fb);
        }
        i += la;
        j += lb;
    }
    zeros.then_with(|| a.cmp(b))
}

fn trim_zeros(d: &[u8]) -> &[u8] {
    let n = d.iter().take_while(|&&c| c == b'0').count();
    &d[n.min(d.len().saturating_sub(1))..]
}

fn decode(s: &str) -> (char, usize) {
    let c = s.chars().next().unwrap_or('\0');
    (c, c.len_utf8().max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(mut v: Vec<&str>) -> Vec<&str> {
        v.sort_by(|a, b| cmp(a, b));
        v
    }

    #[test]
    fn numbers_by_value() {
        assert_eq!(sorted(vec!["file10", "file2", "file1"]), vec!["file1", "file2", "file10"]);
        assert_eq!(sorted(vec!["a100b", "a20b", "a3b"]), vec!["a3b", "a20b", "a100b"]);
        assert_eq!(
            sorted(vec!["IMG_0010.jpg", "IMG_0009.jpg", "IMG_0100.jpg"]),
            vec!["IMG_0009.jpg", "IMG_0010.jpg", "IMG_0100.jpg"]
        );
    }

    #[test]
    fn case_insensitive_and_total() {
        assert_eq!(sorted(vec!["beta", "Alpha", "alpha", "Beta"]), vec!["Alpha", "alpha", "Beta", "beta"]);
        assert_eq!(cmp("abc", "abc"), Ordering::Equal);
        assert_ne!(cmp("a", "A"), Ordering::Equal);
    }

    #[test]
    fn prefixes_and_classes() {
        assert_eq!(cmp("abc", "abcd"), Ordering::Less);
        assert_eq!(sorted(vec!["b", "_a", "1"]), vec!["_a", "1", "b"]);
        assert_eq!(sorted(vec!["x01", "x1", "x001"]), vec!["x1", "x01", "x001"]);
    }

    #[test]
    fn non_ascii() {
        assert_eq!(sorted(vec!["Ébène", "zèbre", "école"]), vec!["Ébène", "école", "zèbre"]);
        assert_eq!(cmp("日本2", "日本10"), Ordering::Less);
    }

    #[test]
    fn huge_digit_runs_do_not_overflow() {
        assert_eq!(cmp("v99999999999999999999999", "v100000000000000000000000"), Ordering::Less);
    }
}
