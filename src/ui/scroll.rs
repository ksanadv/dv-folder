// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Thin overlay scrollbars drawn on the content, as modern Windows apps have them: a slim thumb that widens while
//! the pointer is near it. Nothing is drawn when everything fits.
use crate::ui::gfx::{Surface, Theme, R};

#[derive(Clone, Copy, Debug)]
pub struct Track {
    pub vertical: bool,
    pub track: R,
    pub thumb: R,
    content: i32,
    view: i32,
}

/// `area` is where the bar may run (window coordinates); `content` and `view` are lengths along the bar
pub fn bar(vertical: bool, area: R, content: i32, view: i32, scroll: i32, scale: f32, wide: bool) -> Option<Track> {
    if content <= view || view <= 0 {
        return None;
    }
    let s = |v: f32| (v * scale).round() as i32;
    let thick = if wide { s(8.0) } else { s(4.0) };
    let margin = s(4.0);
    let track = if vertical {
        R::new(area.right() - thick - margin, area.y + margin, thick, area.h - 2 * margin)
    } else {
        R::new(area.x + margin, area.bottom() - thick - margin, area.w - 2 * margin - s(12.0), thick)
    };
    let len = if vertical { track.h } else { track.w };
    let thumb_len = ((len as i64 * view as i64 / content as i64) as i32).max(s(28.0)).min(len);
    let room = (len - thumb_len).max(0);
    let at = (room as i64 * scroll.clamp(0, content - view) as i64 / (content - view).max(1) as i64) as i32;
    let thumb = if vertical {
        R::new(track.x, track.y + at, track.w, thumb_len)
    } else {
        R::new(track.x + at, track.y, thumb_len, track.h)
    };
    Some(Track { vertical, track, thumb, content, view })
}

impl Track {
    /// a pill of a thumb, on a faint pill of a track while the pointer is near
    pub fn draw(&self, surf: &Surface, t: &Theme, hot: bool) {
        let pill = (if self.vertical { self.track.w } else { self.track.h }) as f32 / 2.0;
        if hot {
            surf.round(self.track, pill, t.bg.mix(t.thumb, 0.25));
        }
        surf.round(self.thumb, pill, if hot { t.thumb.mix(t.fg, 0.25) } else { t.thumb });
    }

    /// generous hit area: the bar is thin, the pointer needs room
    pub fn near(&self, x: i32, y: i32, slack: i32) -> bool {
        let r = self.track;
        if self.vertical {
            x >= r.x - slack && x <= r.right() + slack && y >= r.y && y < r.bottom()
        } else {
            y >= r.y - slack && y <= r.bottom() + slack && x >= r.x && x < r.right()
        }
    }

    pub fn on_thumb(&self, x: i32, y: i32) -> bool {
        if self.vertical {
            y >= self.thumb.y && y < self.thumb.bottom()
        } else {
            x >= self.thumb.x && x < self.thumb.right()
        }
    }

    /// where scrolling goes when the thumb is dragged `moved` pixels from where it started at `from`
    pub fn dragged(&self, from: i32, moved: i32) -> i32 {
        let (len, thumb) = if self.vertical { (self.track.h, self.thumb.h) } else { (self.track.w, self.thumb.w) };
        let room = (len - thumb).max(1);
        from + (moved as i64 * (self.content - self.view) as i64 / room as i64) as i32
    }

    /// a click on the track beside the thumb pages towards it
    pub fn page(&self, x: i32, y: i32, scroll: i32) -> i32 {
        let before = if self.vertical { y < self.thumb.y } else { x < self.thumb.x };
        if before {
            scroll - self.view
        } else {
            scroll + self.view
        }
    }
}
