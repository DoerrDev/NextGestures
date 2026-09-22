use anyhow::{Result, bail};
use gesture_core::TriggerButton;
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

use super::hook::MAGIC;
use crate::MouseState;

pub fn cursor_pos() -> (i32, i32) {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p) }.ok();
    (p.x, p.y)
}

fn pressed(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000 != 0 }
}

pub fn mouse_state() -> MouseState {
    let (x, y) = cursor_pos();
    MouseState {
        x,
        y,
        left: pressed(VK_LBUTTON),
        right: pressed(VK_RBUTTON),
        middle: pressed(VK_MBUTTON),
        x1: pressed(VK_XBUTTON1),
        x2: pressed(VK_XBUTTON2),
        wheel: super::hook::take_wheel(),
    }
}

fn mouse_input(flags: MOUSE_EVENT_FLAGS, data: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: MAGIC,
            },
        },
    }
}

/// 候选态直接松手时补发的真实点击
pub fn click(btn: TriggerButton) {
    let (down, up, data) = match btn {
        TriggerButton::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, 0),
        TriggerButton::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, 0),
        TriggerButton::X1 => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, 1),
        TriggerButton::X2 => (MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, 2),
    };
    let batch = [mouse_input(down, data), mouse_input(up, data)];
    unsafe { SendInput(&batch, size_of::<INPUT>() as i32) };
}

fn key_input(vk: VIRTUAL_KEY, up: bool) -> INPUT {
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    if is_extended(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: MAGIC },
        },
    }
}

/// 组合键：按顺序按下，逆序释放
pub fn send_hotkey(keys: &[String]) -> Result<()> {
    if keys.is_empty() {
        bail!("快捷键为空");
    }
    let vks: Vec<VIRTUAL_KEY> = keys
        .iter()
        .map(|k| parse_key(k).ok_or_else(|| anyhow::anyhow!("无法识别的按键: {k}")))
        .collect::<Result<_>>()?;

    let mut batch: Vec<INPUT> = vks.iter().map(|v| key_input(*v, false)).collect();
    batch.extend(vks.iter().rev().map(|v| key_input(*v, true)));
    let sent = unsafe { SendInput(&batch, size_of::<INPUT>() as i32) };
    if sent as usize != batch.len() {
        bail!("SendInput 只发送了 {sent}/{} 个事件", batch.len());
    }
    Ok(())
}

fn is_extended(vk: VIRTUAL_KEY) -> bool {
    matches!(
        vk,
        VK_LEFT
            | VK_RIGHT
            | VK_UP
            | VK_DOWN
            | VK_HOME
            | VK_END
            | VK_PRIOR
            | VK_NEXT
            | VK_INSERT
            | VK_DELETE
            | VK_RCONTROL
            | VK_RMENU
            | VK_NUMLOCK
            | VK_SNAPSHOT
    )
}

pub fn parse_key(name: &str) -> Option<VIRTUAL_KEY> {
    let n = name.trim().to_ascii_lowercase();
    if let Some(num) = n.strip_prefix('f').and_then(|s| s.parse::<u8>().ok())
        && (1..=24).contains(&num)
    {
        return Some(VIRTUAL_KEY(VK_F1.0 + num as u16 - 1));
    }
    if n.len() == 1 {
        let c = n.as_bytes()[0];
        if c.is_ascii_alphanumeric() {
            return Some(VIRTUAL_KEY(c.to_ascii_uppercase() as u16));
        }
    }
    Some(match n.as_str() {
        "ctrl" | "control" => VK_CONTROL,
        "lctrl" => VK_LCONTROL,
        "rctrl" => VK_RCONTROL,
        "alt" | "menu" => VK_MENU,
        "lalt" => VK_LMENU,
        "ralt" => VK_RMENU,
        "shift" => VK_SHIFT,
        "lshift" => VK_LSHIFT,
        "rshift" => VK_RSHIFT,
        "win" | "meta" | "super" | "lwin" => VK_LWIN,
        "rwin" => VK_RWIN,
        "tab" => VK_TAB,
        "enter" | "return" => VK_RETURN,
        "esc" | "escape" => VK_ESCAPE,
        "space" => VK_SPACE,
        "backspace" | "back" => VK_BACK,
        "delete" | "del" => VK_DELETE,
        "insert" | "ins" => VK_INSERT,
        "home" => VK_HOME,
        "end" => VK_END,
        "pgup" | "pageup" | "prior" => VK_PRIOR,
        "pgdn" | "pagedown" | "next" => VK_NEXT,
        "up" => VK_UP,
        "down" => VK_DOWN,
        "left" => VK_LEFT,
        "right" => VK_RIGHT,
        "printscreen" | "prtsc" => VK_SNAPSHOT,
        "capslock" => VK_CAPITAL,
        "-" | "minus" => VK_OEM_MINUS,
        "=" | "plus" => VK_OEM_PLUS,
        "," => VK_OEM_COMMA,
        "." => VK_OEM_PERIOD,
        "/" => VK_OEM_2,
        ";" => VK_OEM_1,
        "'" => VK_OEM_7,
        "[" => VK_OEM_4,
        "]" => VK_OEM_6,
        "\\" => VK_OEM_5,
        "`" => VK_OEM_3,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_keys() {
        assert_eq!(parse_key("Ctrl"), Some(VK_CONTROL));
        assert_eq!(parse_key("w"), Some(VIRTUAL_KEY(b'W' as u16)));
        assert_eq!(parse_key("F12"), Some(VK_F12));
        assert_eq!(parse_key("f1"), Some(VK_F1));
        assert_eq!(parse_key("4"), Some(VIRTUAL_KEY(b'4' as u16)));
        assert_eq!(parse_key("no-such-key"), None);
        assert_eq!(parse_key("f99"), None);
    }
}
