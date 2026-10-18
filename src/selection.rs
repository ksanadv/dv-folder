// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Which items are selected. Selection is kept per item (entry index), so it survives sorting and filtering;
//! clicks and keys arrive as display positions together with the display order.

#[derive(Default, Clone)]
pub struct Selection {
    picked: Vec<bool>,
    count: usize,
    pub focus: Option<u32>,
    anchor: Option<u32>,
}

impl Selection {
    pub fn new(n: usize) -> Selection {
        Selection { picked: vec![false; n], count: 0, focus: None, anchor: None }
    }

    pub fn len(&self) -> usize {
        self.picked.len()
    }

    pub fn count(&self) -> usize {
        self.count
    }

    pub fn is(&self, entry: u32) -> bool {
        self.picked.get(entry as usize).copied().unwrap_or(false)
    }

    pub fn set(&mut self, entry: u32, on: bool) {
        if let Some(p) = self.picked.get_mut(entry as usize) {
            if *p != on {
                *p = on;
                if on {
                    self.count += 1;
                } else {
                    self.count -= 1;
                }
            }
        }
    }

    /// new items were appended (search results streaming in)
    pub fn grow(&mut self, n: usize) {
        if n > self.picked.len() {
            self.picked.resize(n, false);
        }
    }

    pub fn clear(&mut self) {
        if self.count > 0 {
            self.picked.iter_mut().for_each(|p| *p = false);
            self.count = 0;
        }
    }

    pub fn only(&mut self, entry: u32) {
        self.clear();
        self.set(entry, true);
        self.focus = Some(entry);
        self.anchor = Some(entry);
    }

    pub fn focus_pos(&self, order: &[u32]) -> Option<usize> {
        let f = self.focus?;
        order.iter().position(|&e| e == f)
    }

    fn anchor_pos(&self, order: &[u32]) -> Option<usize> {
        let a = self.anchor?;
        order.iter().position(|&e| e == a)
    }

    fn select_range(&mut self, order: &[u32], a: usize, b: usize) {
        let (lo, hi) = (a.min(b), a.max(b).min(order.len().saturating_sub(1)));
        for &e in &order[lo..=hi] {
            self.set(e, true);
        }
    }

    pub fn click(&mut self, order: &[u32], pos: usize, ctrl: bool, shift: bool) {
        let Some(&entry) = order.get(pos) else { return };
        if shift {
            let from = self.anchor_pos(order).unwrap_or(pos);
            if !ctrl {
                self.clear();
            }
            self.select_range(order, from, pos);
            self.focus = Some(entry);
            if self.anchor.is_none() {
                self.anchor = Some(entry);
            }
        } else if ctrl {
            let on = !self.is(entry);
            self.set(entry, on);
            self.focus = Some(entry);
            self.anchor = Some(entry);
        } else {
            self.only(entry);
        }
    }

    /// arrow keys, Home/End, Page Up/Down: Ctrl moves only the focus, Shift extends from the anchor
    pub fn key_move(&mut self, order: &[u32], pos: usize, ctrl: bool, shift: bool) {
        let Some(&entry) = order.get(pos) else { return };
        if shift {
            self.click(order, pos, ctrl, true);
        } else if ctrl {
            self.focus = Some(entry);
        } else {
            self.only(entry);
        }
    }

    /// Ctrl+Space
    pub fn toggle_focus(&mut self) {
        if let Some(f) = self.focus {
            let on = !self.is(f);
            self.set(f, on);
            self.anchor = Some(f);
        }
    }

    pub fn all(&mut self, order: &[u32]) {
        for &e in order {
            self.set(e, true);
        }
    }

    pub fn invert(&mut self, order: &[u32]) {
        for &e in order {
            let on = !self.is(e);
            self.set(e, on);
        }
    }

    /// rubber-band selection: the items under the band, on top of `base` (Ctrl) or alone
    pub fn band(&mut self, order: &[u32], base: Option<&Selection>, hits: impl Iterator<Item = usize>) {
        self.clear();
        if let Some(b) = base {
            for &e in order {
                if b.is(e) {
                    self.set(e, true);
                }
            }
        }
        for pos in hits {
            if let Some(&e) = order.get(pos) {
                let on = base.is_some_and(|b| b.is(e));
                self.set(e, !on);
                self.focus = Some(e);
                self.anchor = Some(e);
            }
        }
    }

    /// selected entries in display order
    pub fn picked(&self, order: &[u32]) -> Vec<u32> {
        if self.count == 0 {
            return Vec::new();
        }
        order.iter().copied().filter(|&e| self.is(e)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(n: u32) -> Vec<u32> {
        // a shuffled display order, as after sorting
        (0..n).rev().collect()
    }

    #[test]
    fn plain_ctrl_and_shift_clicks() {
        let o = order(10);
        let mut s = Selection::new(10);
        s.click(&o, 2, false, false);
        assert_eq!(s.picked(&o), vec![7]);
        s.click(&o, 5, false, true);
        assert_eq!(s.picked(&o), vec![7, 6, 5, 4]);
        s.click(&o, 0, false, true);
        assert_eq!(s.picked(&o), vec![9, 8, 7], "shift-click again re-ranges from the anchor");
        s.click(&o, 5, true, false);
        assert_eq!(s.picked(&o), vec![9, 8, 7, 4]);
        s.click(&o, 5, true, false);
        assert_eq!(s.count(), 3);
        assert_eq!(s.focus_pos(&o), Some(5));
    }

    #[test]
    fn ctrl_shift_adds_a_range() {
        let o = order(10);
        let mut s = Selection::new(10);
        s.click(&o, 0, false, false);
        s.click(&o, 5, true, false);
        s.click(&o, 7, true, true);
        assert_eq!(s.picked(&o), vec![9, 4, 3, 2]);
    }

    #[test]
    fn keyboard() {
        let o = order(5);
        let mut s = Selection::new(5);
        s.key_move(&o, 0, false, false);
        s.key_move(&o, 1, false, true);
        s.key_move(&o, 2, false, true);
        assert_eq!(s.count(), 3);
        s.key_move(&o, 4, true, false);
        assert_eq!(s.count(), 3, "ctrl+arrow moves only the focus");
        s.toggle_focus();
        assert_eq!(s.count(), 4);
        s.key_move(&o, 3, false, false);
        assert_eq!(s.picked(&o), vec![1]);
    }

    #[test]
    fn all_invert_and_band() {
        let o = order(6);
        let mut s = Selection::new(6);
        s.all(&o);
        assert_eq!(s.count(), 6);
        s.click(&o, 0, false, false);
        s.invert(&o);
        assert_eq!(s.count(), 5);
        let base = s.clone();
        s.band(&o, None, 1..3);
        assert_eq!(s.picked(&o), vec![4, 3]);
        s.band(&o, Some(&base), 0..2);
        // ctrl-band toggles relative to what was selected before the drag
        assert_eq!(s.picked(&o), vec![5, 3, 2, 1, 0]);
        s.grow(8);
        assert_eq!(s.len(), 8);
        assert!(!s.is(7));
    }
}
