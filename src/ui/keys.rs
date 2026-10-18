// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 dvfiles contributors
//! Keys handled before the focused window sees them: shortcuts, Tab between panes, typing to search.
use super::*;

/// Keys that work wherever the focus is. Returns true when the key was used.
pub(super) fn pretranslate(msg: &MSG) -> bool {
    // a text box's menu, by the right button or the menu key: dvfiles' own
    let menu_key = matches!(msg.message, WM_KEYDOWN | WM_SYSKEYDOWN) && {
        let vk = VIRTUAL_KEY(msg.wParam.0 as u16);
        vk == VK_APPS || (vk == VK_F10 && key_down(VK_SHIFT))
    };
    if msg.message == WM_RBUTTONUP || menu_key {
        let edits = with_app(|a| (a.main, [a.addr, a.search, a.lv.rename_edit.unwrap_or_default()]));
        if let Some((main, _)) = edits.filter(|(_, e)| !msg.hwnd.0.is_null() && e.contains(&msg.hwnd)) {
            // by the key, under the box; by the button, where it was pressed
            let p = unsafe {
                if menu_key {
                    let mut r = RECT::default();
                    let _ = GetWindowRect(msg.hwnd, &mut r);
                    POINT { x: r.left, y: r.bottom }
                } else {
                    let mut p = POINT { x: x_of(msg.lParam), y: y_of(msg.lParam) };
                    let _ = ClientToScreen(msg.hwnd, &mut p);
                    p
                }
            };
            menu::edit_menu(main, msg.hwnd, p.x, p.y);
            return true;
        }
    }
    if msg.message != WM_KEYDOWN && msg.message != WM_SYSKEYDOWN {
        return false;
    }
    let vk = VIRTUAL_KEY(msg.wParam.0 as u16);
    let (ctrl, shift, alt) = (key_down(VK_CONTROL), key_down(VK_SHIFT), key_down(VK_MENU));
    let Some((addr, search, rename)) = with_app(|a| (a.addr, a.search, a.lv.rename_edit)) else { return false };
    let focus = msg.hwnd;
    let in_addr = focus == addr;
    let in_search = focus == search;
    let in_rename = rename.is_some_and(|r| r == focus);
    let in_edit = in_addr || in_search || in_rename;
    use cmds::Cmd;
    let cmd = match vk {
        VK_LEFT if alt => Some(Cmd::Back),
        VK_RIGHT if alt => Some(Cmd::Forward),
        VK_UP if alt => Some(Cmd::Up),
        VK_BROWSER_BACK => Some(Cmd::Back),
        VK_BROWSER_FORWARD => Some(Cmd::Forward),
        VK_D if alt => Some(Cmd::FocusAddress),
        VK_L if ctrl => Some(Cmd::FocusAddress),
        VK_R if ctrl && shift => Some(Cmd::ChangedRecently),
        VK_F4 if !alt && !in_edit => Some(Cmd::FocusAddress),
        VK_F if ctrl => Some(Cmd::FocusSearch),
        VK_P if alt => Some(Cmd::TogglePreview),
        VK_E if ctrl => Some(Cmd::FocusSearch),
        VK_F3 => Some(Cmd::FocusSearch),
        VK_F5 if !in_edit => Some(Cmd::Refresh),
        VK_N if ctrl && shift && !in_edit => Some(Cmd::NewFolder),
        VK_N if ctrl && !shift => Some(Cmd::NewWindow),
        VK_OEM_COMMA if ctrl && !in_edit => Some(Cmd::Settings),
        VK_W if ctrl => Some(Cmd::CloseTab),
        VK_T if ctrl => Some(Cmd::NewTab),
        VK_TAB if ctrl => Some(Cmd::NextTab(if shift { -1 } else { 1 })),
        VIRTUAL_KEY(k) if ctrl && !alt && (0x31..=0x39).contains(&k) => Some(Cmd::TabAt((k - 0x31) as usize)),
        _ => None,
    };
    if let Some(c) = cmd {
        cmds::run(c);
        return true;
    }
    if in_addr {
        match vk {
            VK_RETURN => {
                bar::commit_address();
                return true;
            }
            VK_ESCAPE => {
                bar::cancel_address();
                return true;
            }
            _ => {}
        }
    }
    if in_search {
        match vk {
            VK_RETURN => {
                unsafe {
                    if let Some(m) = with_app(|a| a.main) {
                        let _ = KillTimer(Some(m), T_SEARCH);
                    }
                }
                start_search(window_text(search));
                return true;
            }
            VK_ESCAPE => {
                set_search_text("");
                start_search(String::new());
                cmds::focus_list();
                return true;
            }
            VK_DOWN => {
                cmds::focus_list();
                return true;
            }
            _ => {}
        }
    }
    if in_rename {
        match vk {
            VK_RETURN => {
                list::end_rename(true);
                return true;
            }
            VK_ESCAPE => {
                list::end_rename(false);
                return true;
            }
            _ => {}
        }
    }
    false
}
