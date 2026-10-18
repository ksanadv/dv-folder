// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! dvfiles' own message boxes, in the window's style (dark or light, dvfiles' icons, the filled default button),
//! where `MessageBoxW` is always Windows' light box. A question defaults to No, as the box before it did: Enter on
//! opening, Esc, the close box and N all answer No until Yes is chosen. Tab and the arrows move between the buttons,
//! a button's first letter presses it, and Ctrl+C copies the text, as in Windows' boxes.
//!
//! The box is modal like Windows': the window behind it is disabled while it is open, and enabled again before the
//! box goes, so Windows hands the activation back to it and not to another program.
use crate::ui::gfx::{self, Surface, Theme, R};
use crate::ui::thumbs::Image;
use crate::ui::{wstr, x_of, y_of};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::WM_MOUSELEAVE;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Icon {
    Info,
    Warning,
    Error,
}

struct Dlg {
    hwnd: HWND,
    text: String,
    icon: Rc<Image>,
    buttons: Vec<&'static str>,
    cancel: usize,
    focus: usize,
    // the focus ring shows once the keyboard is used, as in Windows
    keyboard: bool,
    hot: Option<usize>,
    pressed: Option<usize>,
    answer: Option<usize>,
    theme: Theme,
    ui: HFONT,
    bold: HFONT,
    scale: f32,
    text_h: i32,
    surface: Option<Surface>,
}

impl Dlg {
    fn s(&self, v: f32) -> i32 {
        (v * self.scale).round() as i32
    }

    fn footer_h(&self) -> i32 {
        self.s(60.0)
    }

    /// the buttons' places, right-aligned in the footer
    fn button_rects(&self, w: i32, h: i32) -> Vec<R> {
        let probe = Surface::new(1, 1);
        let bw: Vec<i32> = self
            .buttons
            .iter()
            .map(|b| {
                (probe.as_ref().map_or(40, |p| gfx::text_width(p.dc, b, self.bold)) + self.s(32.0)).max(self.s(92.0))
            })
            .collect();
        let (bh, gap, pad) = (self.s(32.0), self.s(8.0), self.s(20.0));
        let y = h - self.footer_h() + (self.footer_h() - bh) / 2;
        let mut x = w - pad - bw.iter().sum::<i32>() - gap * (bw.len() as i32 - 1);
        bw.iter()
            .map(|&bw| {
                let r = R::new(x, y, bw, bh);
                x += bw + gap;
                r
            })
            .collect()
    }
}

thread_local! {
    static DLG: RefCell<Option<Dlg>> = const { RefCell::new(None) };
    static CLASS: Cell<bool> = const { Cell::new(false) };
}

const WRAP: DRAW_TEXT_FORMAT = DRAW_TEXT_FORMAT(DT_LEFT.0 | DT_WORDBREAK.0 | DT_NOPREFIX.0 | DT_EXPANDTABS.0);

fn paint(hwnd: HWND) {
    DLG.with(|d| {
        let Ok(mut d) = d.try_borrow_mut() else { return };
        let Some(d) = d.as_mut() else { return };
        let (w, h) = crate::ui::client_size(hwnd);
        if Surface::ensure(&mut d.surface, w, h).is_none() {
            return;
        }
        let rects = d.button_rects(w, h);
        let t = d.theme;
        let s = |v: f32| (v * d.scale).round() as i32;
        let surf = d.surface.as_ref().unwrap();
        surf.fill(R::new(0, 0, w, h), t.bg);
        let pad = s(20.0);
        let icon = s(32.0);
        surf.image_in(R::new(pad, pad, icon, icon), &d.icon);
        let tx = pad + icon + s(14.0);
        // one line sits level with the icon's middle; more start at its top
        let ty = if d.text_h < icon { pad + (icon - d.text_h) / 2 } else { pad };
        surf.text(R::new(tx, ty, w - tx - pad, d.text_h + s(4.0)), &d.text, d.ui, t.fg, WRAP);
        let fy = h - d.footer_h();
        surf.fill(R::new(0, fy, w, d.footer_h()), t.chrome);
        surf.fill(R::new(0, fy, w, 1), t.card_border);
        for (i, (r, label)) in rects.iter().zip(d.buttons.iter()).enumerate() {
            let default = i == d.focus;
            let hot = d.hot == Some(i) || d.pressed == Some(i);
            if default {
                surf.round(*r, s(5.0) as f32, if hot { t.primary_hot } else { t.primary });
                surf.text(*r, label, d.bold, t.primary_fg, gfx::CENTER);
            } else {
                surf.round_frame(
                    *r,
                    s(5.0) as f32,
                    if hot { t.input_hot } else { t.input_border },
                    if hot { t.btn_hover } else { t.bg },
                );
                surf.text(*r, label, d.ui, t.fg, gfx::CENTER);
            }
            if default && d.keyboard {
                surf.round_ring(r.inset(s(3.0), s(3.0)), s(3.0) as f32, t.primary_fg.mix(t.primary, 0.35));
            }
        }
        surf.present(hwnd);
    });
}

fn hit(hwnd: HWND, x: i32, y: i32) -> Option<usize> {
    let (w, h) = crate::ui::client_size(hwnd);
    DLG.with(|d| d.borrow().as_ref().and_then(|d| d.button_rects(w, h).iter().position(|r| r.contains(x, y))))
}

fn with<R>(f: impl FnOnce(&mut Dlg) -> R) -> Option<R> {
    DLG.with(|d| d.try_borrow_mut().ok()?.as_mut().map(f))
}

extern "system" fn proc_(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
                paint(hwnd);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_MOUSEMOVE => {
                let h = hit(hwnd, x_of(lp), y_of(lp));
                if with(|d| std::mem::replace(&mut d.hot, h) != h) == Some(true) {
                    paint(hwnd);
                }
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut tme);
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                if with(|d| d.hot.take().is_some()) == Some(true) {
                    paint(hwnd);
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                let h = hit(hwnd, x_of(lp), y_of(lp));
                with(|d| d.pressed = h);
                if h.is_some() {
                    SetCapture(hwnd);
                }
                paint(hwnd);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let h = hit(hwnd, x_of(lp), y_of(lp));
                with(|d| {
                    if d.pressed.take().is_some_and(|p| Some(p) == h) {
                        d.answer = h;
                    }
                });
                let _ = ReleaseCapture();
                paint(hwnd);
                LRESULT(0)
            }
            WM_CLOSE => {
                with(|d| d.answer = Some(d.cancel));
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn register() {
    if CLASS.get() {
        return;
    }
    unsafe {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc_),
            hInstance: GetModuleHandleW(None).unwrap_or_default().into(),
            hIcon: crate::ui::app_icon(),
            hIconSm: crate::ui::app_icon(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("dvfilesBox"),
            ..Default::default()
        };
        RegisterClassExW(&wc);
    }
    CLASS.set(true);
}

/// a key in the box; Some(button) when it answers
fn key(vk: VIRTUAL_KEY, d: &mut Dlg) -> Option<usize> {
    let n = d.buttons.len();
    let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
    match vk {
        VK_TAB | VK_LEFT | VK_RIGHT | VK_UP | VK_DOWN => {
            let back = vk == VK_LEFT || vk == VK_UP || (vk == VK_TAB && shift);
            d.focus = if back { (d.focus + n - 1) % n } else { (d.focus + 1) % n };
            d.keyboard = true;
            None
        }
        VK_RETURN | VK_SPACE => Some(d.focus),
        VK_ESCAPE => Some(d.cancel),
        _ => None,
    }
}

/// Shows the box over `owner` until it is answered, and returns the button pressed (an index into `buttons`).
fn run(owner: HWND, text: &str, icon: Icon, buttons: Vec<&'static str>, default: usize, cancel: usize) -> usize {
    let Some((theme, ui, bold, scale, img)) = crate::ui::with_app(|a| {
        use crate::ui::art::{Art, Tool};
        let tool = match icon {
            Icon::Info => Tool::Props,
            Icon::Warning => Tool::Warning,
            Icon::Error => Tool::Stop,
        };
        (a.theme, a.fonts.ui, a.fonts.bold, a.scale, a.art.get(Art::Tool(tool), a.s(32.0), &a.theme))
    }) else {
        // the app is busy (or gone): Windows' box says it all the same
        return windows_box(owner, text, icon, buttons.len() > 1, default, cancel);
    };
    register();
    let s = |v: f32| (v * scale).round() as i32;
    // the text, wrapped at a comfortable width
    let (text_w, text_h) = Surface::new(1, 1).map_or((s(300.0), s(40.0)), |p| {
        let mut rc = RECT { left: 0, top: 0, right: s(400.0), bottom: 0 };
        let mut wtext: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            SelectObject(p.dc, ui.into());
            DrawTextW(p.dc, &mut wtext, &mut rc, WRAP | DT_CALCRECT);
        }
        (rc.right - rc.left, rc.bottom - rc.top)
    });
    let mut d = Dlg {
        hwnd: HWND::default(),
        text: text.to_string(),
        icon: img,
        buttons,
        cancel,
        focus: default,
        keyboard: false,
        hot: None,
        pressed: None,
        answer: None,
        theme,
        ui,
        bold,
        scale,
        text_h,
        surface: None,
    };
    let pad = s(20.0);
    let buttons_w: i32 = d.button_rects(10_000, 1000).iter().map(|r| r.w + s(8.0)).sum::<i32>() - s(8.0);
    let cw = (pad + s(32.0) + s(14.0) + text_w + pad).max(buttons_w + 2 * pad).max(s(340.0));
    let ch = pad + text_h.max(s(32.0)) + pad + d.footer_h();
    let style = WS_POPUP | WS_CAPTION | WS_SYSMENU;
    let mut wr = RECT { left: 0, top: 0, right: cw, bottom: ch };
    unsafe {
        let _ = AdjustWindowRectEx(&mut wr, style, false, WS_EX_DLGMODALFRAME);
    }
    let (ww, wh) = (wr.right - wr.left, wr.bottom - wr.top);
    // centred over the window, inside its screen
    let mut or = RECT::default();
    unsafe {
        let _ = GetWindowRect(owner, &mut or);
    }
    let work = {
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        unsafe {
            let _ = GetMonitorInfoW(MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST), &mut mi);
        }
        mi.rcWork
    };
    let x = ((or.left + or.right - ww) / 2).clamp(work.left, (work.right - ww).max(work.left));
    let y = ((or.top + or.bottom - wh) / 2).clamp(work.top, (work.bottom - wh).max(work.top));
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_DLGMODALFRAME,
            w!("dvfilesBox"),
            w!("dvfiles"),
            style,
            x,
            y,
            ww,
            wh,
            Some(owner),
            None,
            Some(GetModuleHandleW(None).unwrap_or_default().into()),
            None,
        )
    };
    let Ok(hwnd) = hwnd else { return cancel };
    d.hwnd = hwnd;
    DLG.with(|c| *c.borrow_mut() = Some(d));
    unsafe {
        let dark = theme.dark as i32;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const i32 as *const _, 4);
    }
    crate::ui::tint_frame(hwnd, theme.bg, theme.fg, theme.frame);
    unsafe {
        let _ = EnableWindow(owner, false);
        let _ = ShowWindow(hwnd, if crate::ui::quiet() { SW_SHOWNA } else { SW_SHOW });
        let _ = MessageBeep(match icon {
            Icon::Info => MB_ICONINFORMATION,
            Icon::Warning => MB_ICONWARNING,
            Icon::Error => MB_ICONERROR,
        });
    }
    let mut msg = MSG::default();
    let answer = loop {
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r.0 == 0 {
            unsafe { PostQuitMessage(msg.wParam.0 as i32) };
            break cancel;
        }
        if r.0 == -1 {
            break cancel;
        }
        // the box's keys (and those posted to the window behind it, which is disabled) are the box's
        let ours = msg.hwnd == hwnd || msg.hwnd == owner || unsafe { IsChild(owner, msg.hwnd).as_bool() };
        let handled = match msg.message {
            WM_KEYDOWN | WM_SYSKEYDOWN if ours => {
                let vk = VIRTUAL_KEY(msg.wParam.0 as u16);
                let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
                if ctrl && vk == VK_C {
                    crate::ui::clip::set_text(hwnd, text);
                } else if msg.message == WM_SYSKEYDOWN && vk == VK_F4 {
                    with(|d| d.answer = Some(d.cancel));
                } else {
                    with(|d| {
                        if let Some(a) = key(vk, d) {
                            d.answer = Some(a);
                        }
                    });
                    unsafe {
                        let _ = TranslateMessage(&msg);
                    }
                    paint(hwnd);
                }
                true
            }
            WM_CHAR | WM_SYSCHAR if ours => {
                // a button's first letter: Y, N, O
                if let Some(c) = char::from_u32(msg.wParam.0 as u32).and_then(|c| c.to_lowercase().next()) {
                    with(|d| {
                        if let Some(i) = d
                            .buttons
                            .iter()
                            .position(|b| b.chars().next().and_then(|f| f.to_lowercase().next()) == Some(c))
                        {
                            d.answer = Some(i);
                        }
                    });
                }
                true
            }
            WM_KEYUP | WM_SYSKEYUP if ours => true,
            _ => false,
        };
        if !handled {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if let Some(a) = with(|d| d.answer).flatten() {
            break a;
        }
    };
    crate::ui::menu::swallow_chars();
    unsafe {
        let _ = EnableWindow(owner, true);
        let _ = DestroyWindow(hwnd);
    }
    DLG.with(|c| *c.borrow_mut() = None);
    answer
}

fn windows_box(owner: HWND, text: &str, icon: Icon, question: bool, default: usize, cancel: usize) -> usize {
    let t = wstr(text);
    let flags = match icon {
        Icon::Info => MB_ICONINFORMATION,
        Icon::Warning => MB_ICONWARNING,
        Icon::Error => MB_ICONERROR,
    } | if question { MB_YESNO | if default == 1 { MB_DEFBUTTON2 } else { MB_DEFBUTTON1 } } else { MB_OK };
    let r = unsafe { MessageBoxW(Some(owner), PCWSTR(t.as_ptr()), w!("dvfiles"), flags) };
    if question {
        if r == IDYES {
            0
        } else {
            1
        }
    } else {
        cancel
    }
}

/// a notice with OK
pub fn message(owner: HWND, text: &str, icon: Icon) {
    run(owner, text, icon, vec!["OK"], 0, 0);
}

/// a question with Yes and No, No by default: true for Yes
pub fn ask(owner: HWND, text: &str) -> bool {
    run(owner, text, Icon::Warning, vec!["Yes", "No"], 1, 1) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dlg(buttons: Vec<&'static str>, default: usize) -> Dlg {
        Dlg {
            hwnd: HWND::default(),
            text: String::new(),
            icon: Rc::new(Image { w: 1, h: 1, px: vec![0].into_boxed_slice() }),
            buttons,
            cancel: 1,
            focus: default,
            keyboard: false,
            hot: None,
            pressed: None,
            answer: None,
            theme: Theme::dark(),
            ui: HFONT::default(),
            bold: HFONT::default(),
            scale: 1.0,
            text_h: 20,
            surface: None,
        }
    }

    #[test]
    fn questions_default_to_no() {
        let mut d = dlg(vec!["Yes", "No"], 1);
        // Enter at once is No, and so is Escape
        assert_eq!(key(VK_RETURN, &mut d), Some(1));
        assert_eq!(key(VK_ESCAPE, &mut d), Some(1));
        // moving to Yes and pressing Enter is Yes
        assert_eq!(key(VK_LEFT, &mut d), None);
        assert!(d.keyboard);
        assert_eq!(key(VK_RETURN, &mut d), Some(0));
        // and round again
        assert_eq!(key(VK_RIGHT, &mut d), None);
        assert_eq!(key(VK_SPACE, &mut d), Some(1));
    }

    #[test]
    fn buttons_sit_right_aligned_in_the_footer() {
        let d = dlg(vec!["Yes", "No"], 1);
        let r = d.button_rects(400, 200);
        assert_eq!(r.len(), 2);
        assert!(r[0].right() < r[1].x);
        assert_eq!(r[1].right(), 400 - 20);
        assert!(r[0].y > 200 - d.footer_h());
    }
}
