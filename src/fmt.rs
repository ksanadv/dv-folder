// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Numbers, sizes, times and durations as text. Separators come from the user's locale (see `ui::locale`).

#[derive(Clone, Copy)]
pub struct NumFmt {
    pub thousands: char,
    pub decimal: char,
}

impl Default for NumFmt {
    fn default() -> Self {
        NumFmt { thousands: ',', decimal: '.' }
    }
}

impl NumFmt {
    pub fn group(&self, n: u64) -> String {
        let digits = n.to_string();
        let mut out = String::with_capacity(digits.len() + digits.len() / 3);
        for (i, c) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i) % 3 == 0 {
                out.push(self.thousands);
            }
            out.push(c);
        }
        out
    }

    /// Three significant digits, truncated, the way Explorer's status bar and Properties show sizes:
    /// `532 bytes`, `1.30 KB`, `25.4 MB`, `123 GB`.
    pub fn bytes(&self, n: u64) -> String {
        if n < 1024 {
            return if n == 1 { "1 byte".into() } else { format!("{n} bytes") };
        }
        const UNITS: [&str; 6] = ["KB", "MB", "GB", "TB", "PB", "EB"];
        let mut unit = 0;
        let mut scaled = n as f64 / 1024.0;
        while scaled >= 1000.0 && unit + 1 < UNITS.len() {
            scaled /= 1024.0;
            unit += 1;
        }
        let decimals = if scaled >= 100.0 {
            0
        } else if scaled >= 10.0 {
            1
        } else {
            2
        };
        let factor = 10f64.powi(decimals);
        // truncate rather than round, so 1023.9 KB never shows as "1,024 KB"; the epsilon keeps 1.30 from flooring to
        // 1.29 through binary floating point
        let t = ((scaled * factor) + 1e-9).floor() / factor;
        let text = format!("{t:.*}", decimals as usize);
        format!("{} {}", text.replace('.', &self.decimal.to_string()), UNITS[unit])
    }

    /// Explorer's Size column: whole kilobytes, rounded up (`0 KB` only for an empty file).
    pub fn kb(&self, n: u64) -> String {
        format!("{} KB", self.group(n.div_ceil(1024)))
    }

    /// `1.23 GB (1,320,702,443 bytes)`
    pub fn bytes_exact(&self, n: u64) -> String {
        if n < 1024 {
            self.bytes(n)
        } else {
            format!("{} ({} bytes)", self.bytes(n), self.group(n))
        }
    }

    pub fn speed(&self, bytes_per_sec: f64) -> String {
        format!("{}/s", self.bytes(bytes_per_sec.max(0.0) as u64))
    }
}

pub fn items(n: u64, nf: &NumFmt) -> String {
    if n == 1 {
        "1 item".into()
    } else {
        format!("{} items", nf.group(n))
    }
}

pub fn plural(n: u64, one: &str, many: &str, nf: &NumFmt) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{} {many}", nf.group(n))
    }
}

/// "About 12 seconds", the copy dialog's estimate
pub fn time_left(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "Calculating...".into();
    }
    let s = secs.ceil() as u64;
    if s < 60 {
        return if s <= 1 { "About 1 second".into() } else { format!("About {s} seconds") };
    }
    let m = (s + 30) / 60;
    if m < 60 {
        return if m == 1 { "About 1 minute".into() } else { format!("About {m} minutes") };
    }
    let (h, m) = (m / 60, m % 60);
    let hours = if h == 1 { "1 hour".to_string() } else { format!("{h} hours") };
    match m {
        0 => format!("About {hours}"),
        1 => format!("About {hours} and 1 minute"),
        _ => format!("About {hours} and {m} minutes"),
    }
}

/// UTC civil time from a FILETIME (100 ns ticks since 1601-01-01)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Civil {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

pub const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;

pub fn civil(filetime: u64) -> Civil {
    let secs = (filetime / 10_000_000) as i64 - (FILETIME_UNIX_EPOCH / 10_000_000) as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400) as u32;
    // Howard Hinnant's days-to-civil
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    Civil {
        year: (y + (m <= 2) as i64) as i32,
        month: m,
        day: d,
        hour: rem / 3600,
        minute: rem / 60 % 60,
        second: rem % 60,
    }
}

/// ISO 8601 date and time, for the console tool
pub fn iso(filetime: u64) -> String {
    let c = civil(filetime);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", c.year, c.month, c.day, c.hour, c.minute, c.second)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouping() {
        let nf = NumFmt::default();
        assert_eq!(nf.group(0), "0");
        assert_eq!(nf.group(999), "999");
        assert_eq!(nf.group(1000), "1,000");
        assert_eq!(nf.group(1_234_567), "1,234,567");
        let de = NumFmt { thousands: '.', decimal: ',' };
        assert_eq!(de.group(1_234_567), "1.234.567");
        assert_eq!(de.bytes(1_363_149), "1,30 MB");
    }

    #[test]
    fn byte_sizes_like_explorer() {
        let nf = NumFmt::default();
        assert_eq!(nf.bytes(0), "0 bytes");
        assert_eq!(nf.bytes(1), "1 byte");
        assert_eq!(nf.bytes(1023), "1023 bytes");
        assert_eq!(nf.bytes(1024), "1.00 KB");
        assert_eq!(nf.bytes(1331), "1.29 KB");
        assert_eq!(nf.bytes(1_363_149), "1.30 MB");
        assert_eq!(nf.bytes(26_633_831), "25.4 MB");
        assert_eq!(nf.bytes(132_070_244_352), "123 GB");
        // just under a unit boundary stays in the smaller unit's three digits
        assert_eq!(nf.bytes(1_048_575), "0.99 MB");
        assert_eq!(nf.bytes(1000 * 1024 - 1), "999 KB");
    }

    #[test]
    fn explorer_kb_column() {
        let nf = NumFmt::default();
        assert_eq!(nf.kb(0), "0 KB");
        assert_eq!(nf.kb(1), "1 KB");
        assert_eq!(nf.kb(1024), "1 KB");
        assert_eq!(nf.kb(1025), "2 KB");
        assert_eq!(nf.kb(4 << 30), "4,194,304 KB");
    }

    #[test]
    fn exact_sizes() {
        let nf = NumFmt::default();
        assert_eq!(nf.bytes_exact(10), "10 bytes");
        assert_eq!(nf.bytes_exact(1_320_702_443), "1.22 GB (1,320,702,443 bytes)");
    }

    #[test]
    fn remaining_time() {
        assert_eq!(time_left(0.2), "About 1 second");
        assert_eq!(time_left(12.1), "About 13 seconds");
        assert_eq!(time_left(95.0), "About 2 minutes");
        assert_eq!(time_left(3600.0), "About 1 hour");
        assert_eq!(time_left(3900.0), "About 1 hour and 5 minutes");
        assert_eq!(time_left(f64::INFINITY), "Calculating...");
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil(FILETIME_UNIX_EPOCH), Civil { year: 1970, month: 1, day: 1, hour: 0, minute: 0, second: 0 });
        // 2026-10-15 06:19:22 UTC
        let t = FILETIME_UNIX_EPOCH + 1_792_045_162 * 10_000_000;
        assert_eq!(iso(t), "2026-10-15T06:19:22Z");
        // leap day
        let t = FILETIME_UNIX_EPOCH + 951_782_400 * 10_000_000;
        assert_eq!(iso(t), "2000-02-29T00:00:00Z");
        assert_eq!(civil(0).year, 1601);
    }

    #[test]
    fn counts() {
        let nf = NumFmt::default();
        assert_eq!(items(1, &nf), "1 item");
        assert_eq!(items(1500, &nf), "1,500 items");
        assert_eq!(plural(2, "file", "files", &nf), "2 files");
    }
}
