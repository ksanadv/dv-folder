// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! The Recycle Bin's on-disk records. Each recycled item becomes two entries in `<volume>\$Recycle.Bin\<user SID>`:
//! `$R<6 chars><ext>` (the item itself, renamed) and `$I<same 6 chars><ext>` (this record). Version 2, used since
//! Windows 10, verified on build 26300 against items recycled by the shell:
//!
//! | offset | size | field |
//! |---|---|---|
//! | 0 | 8 | version, 2 |
//! | 8 | 8 | size in bytes (a folder's total) |
//! | 16 | 8 | deletion time, FILETIME UTC |
//! | 24 | 4 | path length in UTF-16 units, including the terminating NUL |
//! | 28 | 2n | original full path, NUL-terminated |
//!
//! Version 1 (Vista to 8.1) has the path in a fixed 260-unit field at offset 24 instead.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub size: u64,
    pub deleted: u64,
    pub path: String,
}

pub fn encode(r: &Record) -> Vec<u8> {
    let units: Vec<u16> = r.path.encode_utf16().chain(Some(0)).collect();
    let mut out = Vec::with_capacity(28 + units.len() * 2);
    out.extend_from_slice(&2u64.to_le_bytes());
    out.extend_from_slice(&r.size.to_le_bytes());
    out.extend_from_slice(&r.deleted.to_le_bytes());
    out.extend_from_slice(&(units.len() as u32).to_le_bytes());
    for u in units {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

fn path_from(b: &[u8]) -> String {
    let units: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
    String::from_utf16_lossy(&units)
}

pub fn decode(b: &[u8]) -> Option<Record> {
    let (version, size, deleted) = (u64_at(b, 0)?, u64_at(b, 8)?, u64_at(b, 16)?);
    let path = match version {
        1 => path_from(b.get(24..24 + 520)?),
        2 => {
            let n = u32::from_le_bytes(b.get(24..28)?.try_into().ok()?) as usize;
            path_from(b.get(28..28 + n.checked_mul(2)?)?)
        }
        _ => return None,
    };
    Some(Record { size, deleted, path })
}

const ALPHABET: &[u8; 36] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// The 6-character id shared by the `$R` and `$I` names, from a random seed; the shell uses the same alphabet.
pub fn id(mut seed: u64) -> String {
    (0..6)
        .map(|_| {
            let c = ALPHABET[(seed % 36) as usize] as char;
            seed /= 36;
            c
        })
        .collect()
}

/// The extension the bin entries carry: the original's, folders included (`probe.dir` becomes `$R1A2B3C.dir`).
pub fn bin_ext(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[i..],
        _ => "",
    }
}

/// xorshift64*, enough to spread bin ids; seeded from the clock and the process
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }

    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // the first 28 bytes of a $I record the Windows shell wrote for a 1000-byte file (build 26300)
    const SHELL_HEADER: [u8; 28] = [
        0x02, 0, 0, 0, 0, 0, 0, 0, 0xE8, 0x03, 0, 0, 0, 0, 0, 0, 0x10, 0xD6, 0xA0, 0x43, 0x86, 0x5C, 0xDD, 0x01, 0xAC,
        0, 0, 0,
    ];

    #[test]
    fn matches_the_shells_layout() {
        let path: String = "C:\\".to_string() + &"x".repeat(0xAC - 1 - 3);
        let r = Record { size: 1000, deleted: 0x01DD_5C86_43A0_D610, path };
        let bytes = encode(&r);
        assert_eq!(bytes[..28], SHELL_HEADER);
        assert_eq!(bytes.len(), 28 + 0xAC * 2);
        assert_eq!(decode(&bytes), Some(r));
    }

    #[test]
    fn round_trip_non_ascii() {
        let r = Record { size: 800, deleted: 133_000_000_000_000_000, path: r"D:\Fotos\café 日本\probe.dir".into() };
        assert_eq!(decode(&encode(&r)), Some(r));
    }

    #[test]
    fn version_1() {
        let mut b = vec![0u8; 24 + 520];
        b[0] = 1;
        b[8] = 5;
        for (i, u) in r"C:\old.txt".encode_utf16().enumerate() {
            b[24 + i * 2..26 + i * 2].copy_from_slice(&u.to_le_bytes());
        }
        let r = decode(&b).unwrap();
        assert_eq!((r.size, r.path.as_str()), (5, r"C:\old.txt"));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(decode(&[1, 2, 3]), None);
        let mut b = encode(&Record { size: 1, deleted: 1, path: "C:\\a".into() });
        b[0] = 9;
        assert_eq!(decode(&b), None);
        let mut b = encode(&Record { size: 1, deleted: 1, path: "C:\\a".into() });
        b[24] = 200; // claims more path than there is
        assert_eq!(decode(&b), None);
    }

    #[test]
    fn ids_and_extensions() {
        let a = id(0);
        assert_eq!(a, "000000");
        let mut rng = Rng::new(42);
        let ids: std::collections::HashSet<String> = (0..1000).map(|_| id(rng.next())).collect();
        assert_eq!(ids.len(), 1000);
        assert!(ids.iter().all(|s| s.len() == 6 && s.bytes().all(|c| ALPHABET.contains(&c))));
        assert_eq!(bin_ext("probe-file.txt"), ".txt");
        assert_eq!(bin_ext("probe.dir"), ".dir");
        assert_eq!(bin_ext("Makefile"), "");
        assert_eq!(bin_ext(".hidden"), "");
    }
}
