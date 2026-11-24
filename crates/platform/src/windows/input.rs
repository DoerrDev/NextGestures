use anyhow::{Result, bail};
use gesture_core::{KeySequenceEvent, TriggerButton};
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

pub fn replay_sequence(events: &[KeySequenceEvent]) -> Result<()> {
    use std::time::{Duration, Instant};

    static PLAYING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _playing = PLAYING.try_lock().map_err(|_| anyhow::anyhow!("快捷键序列正在回放"))?;
    let mut held = Vec::<&KeySequenceEvent>::new();
    let mut due = Instant::now();
    let result = (|| {
        for event in events {
            due += Duration::from_millis(event.delay_ms.into());
            std::thread::sleep(due.saturating_duration_since(Instant::now()));
            send_key_event(event, !event.down)?;
            let id = (event.vk, event.scan, event.extended);
            if event.down {
                held.push(event);
            } else {
                held.retain(|e| (e.vk, e.scan, e.extended) != id);
            }
        }
        Ok(())
    })();
    for event in held.into_iter().rev() {
        let _ = send_key_event(event, true);
    }
    result
}

fn send_key_event(event: &KeySequenceEvent, up: bool) -> Result<()> {
    let vk = VIRTUAL_KEY(event.vk);
    let mut data = key_input(vk, up);
    let key = unsafe { &mut data.Anonymous.ki };
    key.dwFlags = if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) };
    if event.extended {
        key.dwFlags |= KEYEVENTF_EXTENDEDKEY;
    }
    // Pause and Print Screen use special multi-byte hardware sequences.
    if event.scan != 0 && !matches!(vk, VK_PAUSE | VK_SNAPSHOT) {
        key.wVk = VIRTUAL_KEY(0);
        key.wScan = event.scan;
        key.dwFlags |= KEYEVENTF_SCANCODE;
    }
    if unsafe { SendInput(&[data], size_of::<INPUT>() as i32) } != 1 {
        bail!("发送按键失败: {}", event.label);
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

/// parse_key 的反向：录制快捷键时把虚拟键码转回配置里用的名字。修饰键不在此列
pub fn key_name(vk: VIRTUAL_KEY) -> Option<String> {
    let c = vk.0;
    if (VK_F1.0..=VK_F24.0).contains(&c) {
        return Some(format!("f{}", c - VK_F1.0 + 1));
    }
    if (0x30..=0x39).contains(&c) || (0x41..=0x5A).contains(&c) {
        return Some((c as u8 as char).to_ascii_lowercase().to_string());
    }
    let name = match vk {
        VK_TAB => "tab",
        VK_RETURN => "enter",
        VK_ESCAPE => "esc",
        VK_SPACE => "space",
        VK_BACK => "backspace",
        VK_DELETE => "delete",
        VK_INSERT => "insert",
        VK_HOME => "home",
        VK_END => "end",
        VK_PRIOR => "pgup",
        VK_NEXT => "pgdn",
        VK_UP => "up",
        VK_DOWN => "down",
        VK_LEFT => "left",
        VK_RIGHT => "right",
        VK_SNAPSHOT => "printscreen",
        VK_CAPITAL => "capslock",
        VK_OEM_MINUS => "-",
        VK_OEM_PLUS => "=",
        VK_OEM_COMMA => ",",
        VK_OEM_PERIOD => ".",
        VK_OEM_2 => "/",
        VK_OEM_1 => ";",
        VK_OEM_7 => "'",
        VK_OEM_4 => "[",
        VK_OEM_6 => "]",
        VK_OEM_5 => "\\",
        VK_OEM_3 => "`",
        _ => return None,
    };
    Some(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_names_round_trip_through_parse_key() {
        for code in 0..=0xFFu16 {
            let vk = VIRTUAL_KEY(code);
            if let Some(name) = key_name(vk) {
                assert_eq!(parse_key(&name), Some(vk), "{name}");
            }
        }
        assert_eq!(key_name(VK_F1).as_deref(), Some("f1"));
        assert_eq!(key_name(VK_LEFT).as_deref(), Some("left"));
        assert_eq!(key_name(VIRTUAL_KEY(b'A' as u16)).as_deref(), Some("a"));
        assert_eq!(key_name(VK_LWIN), None);
    }

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
