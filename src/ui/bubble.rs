// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! The undo bubble: after a rename, a move, a copy or a delete, a small card at the bottom of the list says what
//! happened, with Undo, for six seconds (as Gmail's does), and stays while the pointer rests on it. It slides in and
//! out when animations are on. Its timer runs only while it is there; turned off in Settings, nothing shows.
use crate::ui::gfx::{self, Rgb, Surface, R};
use crate::ui::{invalidate, App};
use std::time::Instant;
use windows::Win32::UI::WindowsAndMessaging::{
    KillTimer, SetTimer, SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
};

/// the list window's timer for the bubble
pub const T_BUBBLE: usize = 9;

const SHOW_MS: f32 = 6000.0;
const SLIDE_MS: f32 = 180.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    Undo,
    Close,
    Card,
}

pub struct Bubble {
    text: String,
    // when it came in, and (going) when it began to leave
    since: Instant,
    leaving: Option<Instant>,
    pub hot: Option<Part>,
    // where it was last drawn: the card, Undo and the close mark
    rects: Option<(R, R, R)>,
}

/// whether things move: dvfiles' setting, and Windows' own "Show animations in Windows"
pub fn animations(a: &App) -> bool {
    if !a.settings.animations {
        return false;
    }
    let mut on = windows::core::BOOL(1);
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some(&mut on as *mut _ as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    on.as_bool()
}

/// a quick start that settles softly
pub fn ease(t: f32) -> f32 {
    let u = 1.0 - t.clamp(0.0, 1.0);
    1.0 - u * u * u
}

/// shows the bubble for something just done
pub fn show(a: &mut App, text: String) {
    if !a.settings.undo_bubble {
        return;
    }
    a.bubble = Some(Bubble { text, since: Instant::now(), leaving: None, hot: None, rects: None });
    unsafe {
        SetTimer(Some(a.list), T_BUBBLE, 15, None);
    }
    invalidate(a.list);
}

/// takes the bubble away at once (Undo was used, or it was closed)
pub fn hide(a: &mut App) {
    if a.bubble.take().is_some() {
        unsafe {
            let _ = KillTimer(Some(a.list), T_BUBBLE);
        }
        invalidate(a.list);
    }
}

/// a timer tick: the slide moves on, the time runs out (not while the pointer is on it), the slide out ends
pub fn tick(a: &mut App) {
    let anim = animations(a);
    let Some(b) = a.bubble.as_mut() else {
        unsafe {
            let _ = KillTimer(Some(a.list), T_BUBBLE);
        }
        return;
    };
    let now = Instant::now();
    if b.hot.is_some() {
        b.since = now;
    }
    match b.leaving {
        Some(t) if !anim || now.duration_since(t).as_secs_f32() * 1000.0 >= SLIDE_MS => {
            hide(a);
            return;
        }
        None if now.duration_since(b.since).as_secs_f32() * 1000.0 >= SHOW_MS => b.leaving = Some(now),
        _ => {}
    }
    // between the slides nothing moves: a slow tick until it is time to leave
    let sliding = anim && (b.leaving.is_some() || now.duration_since(b.since).as_secs_f32() * 1000.0 < SLIDE_MS);
    unsafe {
        SetTimer(Some(a.list), T_BUBBLE, if sliding { 15 } else { 250 }, None);
    }
    if sliding || b.leaving.is_some() {
        invalidate(a.list);
    }
}

/// what part of the bubble is under a point of the list
pub fn hit(a: &App, x: i32, y: i32) -> Option<Part> {
    let (card, undo, close) = a.bubble.as_ref()?.rects?;
    if undo.contains(x, y) {
        Some(Part::Undo)
    } else if close.contains(x, y) {
        Some(Part::Close)
    } else {
        card.contains(x, y).then_some(Part::Card)
    }
}

/// draws the bubble at the bottom of the list (`w` by `h`), last, over the rows
pub fn draw(a: &mut App, w: i32, h: i32) {
    let anim = animations(a);
    let s = |v: f32| (v * a.scale).round() as i32;
    let Some(b) = a.bubble.as_ref() else { return };
    let ui = a.fonts.ui;
    let bold = a.fonts.bold;
    let t = a.theme;
    let (rad, small) = (a.rad_card(), a.rad());
    let surf: &Surface = a.surf_list.as_ref().unwrap();
    let text_w = gfx::text_width(surf.dc, &b.text, ui);
    let undo_w = gfx::text_width(surf.dc, "Undo", bold) + s(28.0);
    let ch = s(48.0);
    let cw = (s(18.0) + text_w + s(20.0) + undo_w + s(6.0) + s(36.0)).min(w - s(32.0)).max(s(160.0));
    let now = Instant::now();
    // how far it is in: 0 hidden below the list's edge, 1 in place
    let shown = if !anim {
        1.0
    } else if let Some(t) = b.leaving {
        1.0 - ease(now.duration_since(t).as_secs_f32() * 1000.0 / SLIDE_MS)
    } else {
        ease(now.duration_since(b.since).as_secs_f32() * 1000.0 / SLIDE_MS)
    };
    let rest = h - ch - s(20.0);
    let y = rest + ((1.0 - shown) * (ch + s(24.0)) as f32) as i32;
    let card = R::new((w - cw) / 2, y, cw, ch);
    // a soft shadow under a floating card, a step up from the list in either theme
    let shade = if t.dark { 0.30f32 } else { 0.10 };
    for k in 1..=s(10.0) {
        let spread = k as f32;
        surf.round_blend(
            R::new(card.x - k + s(1.0), card.y - k + s(4.0), card.w + 2 * k - s(2.0), card.h + 2 * k - s(4.0)),
            rad + spread,
            Rgb(0, 0, 0),
            shade / (s(10.0) as f32) * (1.0 - spread / (s(10.0) as f32 + 1.0)),
        );
    }
    let (face, edge, text, hover) = if t.dark {
        (Rgb::hex(0x2B2B2B), Rgb::hex(0x404040), Rgb::hex(0xF2F2F2), Rgb::hex(0x383838))
    } else {
        (Rgb::hex(0xFFFFFF), Rgb::hex(0xD9DADD), t.fg, Rgb::hex(0xEEEFF1))
    };
    surf.round(card, rad, edge);
    surf.round(card.inset(1, 1), rad - 1.0, face);
    let close = R::new(card.right() - s(42.0), card.y + s(8.0), s(32.0), ch - s(16.0));
    let undo = R::new(close.x - s(6.0) - undo_w, card.y + s(8.0), undo_w, ch - s(16.0));
    // Undo is the card's one button, in the accent
    surf.round(undo, small, if b.hot == Some(Part::Undo) { t.primary_hot } else { t.primary });
    if b.hot == Some(Part::Close) {
        surf.round(close, small, hover);
    }
    surf.text(R::new(card.x + s(18.0), card.y, undo.x - card.x - s(26.0), ch), &b.text, ui, text, gfx::LEFT);
    surf.text(undo, "Undo", bold, t.primary_fg, gfx::CENTER);
    let mark = a.art.mark(crate::ui::art::Mark::Close, s(11.0), t.muted);
    let surf = a.surf_list.as_ref().unwrap();
    surf.image_in(close, &mark);
    if let Some(b) = a.bubble.as_mut() {
        b.rects = Some((card, undo, close));
    }
}
