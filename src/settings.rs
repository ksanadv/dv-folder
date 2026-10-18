// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemeMode {
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewMode {
    Details,
    Icons,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Col {
    Name,
    Modified,
    Type,
    Size,
    // search results: the folder an item is in
    Folder,
    // This PC: total and free space
    Total,
    Free,
}

impl Col {
    pub fn key(self) -> &'static str {
        match self {
            Col::Name => "name",
            Col::Modified => "modified",
            Col::Type => "type",
            Col::Size => "size",
            Col::Folder => "folder",
            Col::Total => "total",
            Col::Free => "free",
        }
    }

    pub fn from_key(s: &str) -> Option<Col> {
        [Col::Name, Col::Modified, Col::Type, Col::Size, Col::Folder, Col::Total, Col::Free]
            .into_iter()
            .find(|c| c.key() == s)
    }

    pub fn title(self) -> &'static str {
        match self {
            Col::Name => "Name",
            Col::Modified => "Date modified",
            Col::Type => "Type",
            Col::Size => "Size",
            Col::Folder => "Folder",
            Col::Total => "Total size",
            Col::Free => "Free space",
        }
    }

    /// width at 96 DPI
    pub fn default_width(self) -> i32 {
        match self {
            Col::Name => 300,
            Col::Modified => 150,
            Col::Type => 150,
            Col::Size => 90,
            Col::Folder => 320,
            Col::Total => 100,
            Col::Free => 200,
        }
    }

    pub fn right_aligned(self) -> bool {
        matches!(self, Col::Size | Col::Total | Col::Free)
    }
}

#[derive(Clone)]
pub struct Settings {
    pub theme: ThemeMode,
    pub view: ViewMode,
    pub sort: Col,
    pub sort_desc: bool,
    // None: follow Explorer's own setting
    pub show_hidden: Option<bool>,
    pub show_ext: bool,
    // the Size column also measures folders
    pub folder_sizes: bool,
    // whole-drive searches go through a saved index (see dvfiles::index)
    pub search_index: bool,
    pub nav_width: i32,
    // the preview pane (Alt+P) and its width at 96 DPI
    pub preview: bool,
    pub preview_width: i32,
    pub window: [i32; 4],
    pub maximized: bool,
    pub widths: Vec<(Col, i32)>,
    pub pins: Vec<String>,
    // file types show their program's icon (dvnote's, VLC's) as in Explorer, else dvfiles' own
    pub app_icons: bool,
    // the frame around the list is glass: what is behind the window, blurred and darkened
    pub glass: bool,
    // Ctrl+V with a picture or text on the clipboard (and no files) makes a file of it
    pub paste_files: bool,
    // the undo bubble after a rename, move, copy or delete
    pub undo_bubble: bool,
    // pictures open in dvfiles' own viewer
    pub viewer: bool,
    // Downloads marks what arrived since the last look
    pub inbox: bool,
    // copies wait their turn instead of running side by side
    pub copy_queue: bool,
    // a folder held under a drag for a moment opens, so the drop can go deeper
    pub spring: bool,
    // things slide and fade (Windows' own animation setting still applies)
    pub animations: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: ThemeMode::System,
            view: ViewMode::Details,
            sort: Col::Name,
            sort_desc: false,
            show_hidden: None,
            show_ext: true,
            folder_sizes: false,
            search_index: true,
            nav_width: 230,
            preview: false,
            preview_width: 320,
            window: [0, 0, 1100, 680],
            maximized: false,
            widths: Vec::new(),
            pins: Vec::new(),
            app_icons: true,
            glass: true,
            paste_files: true,
            undo_bubble: true,
            viewer: true,
            inbox: true,
            copy_queue: true,
            spring: true,
            animations: true,
        }
    }
}

pub fn dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("dvfiles"))
}

fn path() -> Option<PathBuf> {
    Some(dir()?.join("settings.txt"))
}

impl Settings {
    pub fn width(&self, c: Col) -> i32 {
        self.widths.iter().find(|(k, _)| *k == c).map_or(c.default_width(), |(_, w)| *w)
    }

    pub fn set_width(&mut self, c: Col, w: i32) {
        match self.widths.iter_mut().find(|(k, _)| *k == c) {
            Some(e) => e.1 = w,
            None => self.widths.push((c, w)),
        }
    }

    pub fn load() -> Self {
        let mut s = Settings::default();
        let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) else { return s };
        let num = |v: &str, d: i32| v.trim().parse::<i32>().unwrap_or(d);
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = v.trim();
            match k.trim() {
                "theme" => {
                    s.theme = match v {
                        "light" => ThemeMode::Light,
                        "dark" => ThemeMode::Dark,
                        _ => ThemeMode::System,
                    }
                }
                "view" => s.view = if v == "icons" { ViewMode::Icons } else { ViewMode::Details },
                "sort" => s.sort = Col::from_key(v).unwrap_or(Col::Name),
                "sort_desc" => s.sort_desc = v == "1",
                "show_hidden" => {
                    s.show_hidden = match v {
                        "1" => Some(true),
                        "0" => Some(false),
                        _ => None,
                    }
                }
                "show_ext" => s.show_ext = v != "0",
                "folder_sizes" => s.folder_sizes = v == "1",
                "search_index" => s.search_index = v == "1",
                "nav_width" => s.nav_width = num(v, s.nav_width).clamp(0, 1200),
                "preview" => s.preview = v == "1",
                "preview_width" => s.preview_width = num(v, s.preview_width).clamp(180, 900),
                "window" => {
                    let p: Vec<i32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                    if p.len() == 4 && p[2] >= 320 && p[3] >= 240 {
                        s.window = [p[0], p[1], p[2], p[3]];
                    }
                }
                "maximized" => s.maximized = v == "1",
                "app_icons" => s.app_icons = v != "0",
                "glass" => s.glass = v != "0",
                "paste_files" => s.paste_files = v != "0",
                "undo_bubble" => s.undo_bubble = v != "0",
                "viewer" => s.viewer = v != "0",
                "inbox" => s.inbox = v != "0",
                "copy_queue" => s.copy_queue = v != "0",
                "spring" => s.spring = v != "0",
                "animations" => s.animations = v != "0",
                "pin" if !v.is_empty() => s.pins.push(v.to_string()),
                k if k.starts_with("width.") => {
                    if let Some(c) = Col::from_key(&k[6..]) {
                        s.set_width(c, num(v, c.default_width()).clamp(40, 2000));
                    }
                }
                _ => {}
            }
        }
        s
    }

    pub fn save(&self) {
        let Some(p) = path() else { return };
        let mut t = String::new();
        let theme = match self.theme {
            ThemeMode::System => "system",
            ThemeMode::Light => "light",
            ThemeMode::Dark => "dark",
        };
        t.push_str(&format!("theme={theme}\n"));
        t.push_str(&format!("view={}\n", if self.view == ViewMode::Icons { "icons" } else { "details" }));
        t.push_str(&format!("sort={}\nsort_desc={}\n", self.sort.key(), self.sort_desc as u8));
        let hidden = match self.show_hidden {
            Some(true) => "1",
            Some(false) => "0",
            None => "explorer",
        };
        t.push_str(&format!(
            "show_hidden={hidden}\nshow_ext={}\nfolder_sizes={}\n",
            self.show_ext as u8, self.folder_sizes as u8
        ));
        t.push_str(&format!("search_index={}\n", self.search_index as u8));
        t.push_str(&format!("nav_width={}\n", self.nav_width));
        t.push_str(&format!("preview={}\npreview_width={}\n", self.preview as u8, self.preview_width));
        t.push_str(&format!(
            "app_icons={}\nglass={}\npaste_files={}\n",
            self.app_icons as u8, self.glass as u8, self.paste_files as u8
        ));
        t.push_str(&format!("undo_bubble={}\nanimations={}\n", self.undo_bubble as u8, self.animations as u8));
        t.push_str(&format!("viewer={}\n", self.viewer as u8));
        t.push_str(&format!("inbox={}\n", self.inbox as u8));
        t.push_str(&format!("copy_queue={}\n", self.copy_queue as u8));
        t.push_str(&format!("spring={}\n", self.spring as u8));
        let w = self.window;
        t.push_str(&format!("window={},{},{},{}\nmaximized={}\n", w[0], w[1], w[2], w[3], self.maximized as u8));
        for (c, w) in &self.widths {
            t.push_str(&format!("width.{}={w}\n", c.key()));
        }
        for pin in &self.pins {
            t.push_str(&format!("pin={pin}\n"));
        }
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, t);
    }
}

/// Explorer's "Hidden items" setting (View tab), so dvfiles starts out the way the user set Explorer up
pub fn explorer_shows_hidden() -> bool {
    use winreg::enums::HKEY_CURRENT_USER;
    let v: u32 = winreg::RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced")
        .and_then(|k| k.get_value("Hidden"))
        .unwrap_or(2);
    v == 1
}
