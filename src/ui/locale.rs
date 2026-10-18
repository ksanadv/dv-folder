// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Dates and numbers in the user's regional format, as Explorer shows them.
use crate::ui::from_wide;
use dvfiles::fmt::NumFmt;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::Globalization::*;
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

pub fn numbers() -> NumFmt {
    let get = |kind: u32, d: char| {
        let mut buf = [0u16; 8];
        let n = unsafe { GetLocaleInfoEx(PCWSTR::null(), kind, Some(&mut buf)) };
        if n > 1 {
            from_wide(&buf).chars().next().unwrap_or(d)
        } else {
            d
        }
    };
    NumFmt { thousands: get(LOCALE_STHOUSAND, ','), decimal: get(LOCALE_SDECIMAL, '.') }
}

fn local(ft: u64) -> Option<SYSTEMTIME> {
    if ft == 0 {
        return None;
    }
    let f = FILETIME { dwLowDateTime: ft as u32, dwHighDateTime: (ft >> 32) as u32 };
    let (mut utc, mut loc) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    unsafe {
        FileTimeToSystemTime(&f, &mut utc).ok()?;
        SystemTimeToTzSpecificLocalTime(None, &utc, &mut loc).ok()?;
    }
    Some(loc)
}

fn date(st: &SYSTEMTIME, flags: ENUM_DATE_FORMATS_FLAGS) -> String {
    let mut buf = [0u16; 96];
    let n = unsafe { GetDateFormatEx(PCWSTR::null(), flags, Some(st), PCWSTR::null(), Some(&mut buf), PCWSTR::null()) };
    if n > 0 {
        from_wide(&buf)
    } else {
        format!("{}-{:02}-{:02}", st.wYear, st.wMonth, st.wDay)
    }
}

fn time(st: &SYSTEMTIME, flags: TIME_FORMAT_FLAGS) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetTimeFormatEx(PCWSTR::null(), flags, Some(st), PCWSTR::null(), Some(&mut buf)) };
    if n > 0 {
        from_wide(&buf)
    } else {
        format!("{:02}:{:02}", st.wHour, st.wMinute)
    }
}

/// `15/10/2026 06:19`, the Date modified column
pub fn short(ft: u64) -> String {
    match local(ft) {
        Some(st) => format!("{} {}", date(&st, DATE_SHORTDATE), time(&st, TIME_NOSECONDS)),
        None => String::new(),
    }
}

/// `Thursday, 15 October 2026, 06:19:22`, as in Properties
pub fn long(ft: u64) -> String {
    match local(ft) {
        Some(st) => format!("{}, {}", date(&st, DATE_LONGDATE), time(&st, TIME_FORMAT_FLAGS(0))),
        None => String::new(),
    }
}
