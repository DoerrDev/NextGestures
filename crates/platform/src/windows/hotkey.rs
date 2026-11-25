use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock, mpsc};

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use super::input::parse_key;

const WM_UPDATE: u32 = WM_APP + 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Chord(u32, u32);

impl Chord {
    pub fn parse(keys: &[String]) -> Result<Self> {
        let mut mods = HOT_KEY_MODIFIERS::default();
        let mut key = None;
        for name in keys {
            match parse_key(name).with_context(|| format!("未知按键 {name}"))? {
                VK_CONTROL | VK_LCONTROL | VK_RCONTROL => mods |= MOD_CONTROL,
                VK_MENU | VK_LMENU | VK_RMENU => mods |= MOD_ALT,
                VK_SHIFT | VK_LSHIFT | VK_RSHIFT => mods |= MOD_SHIFT,
                VK_LWIN | VK_RWIN => mods |= MOD_WIN,
                v if key.is_none() => key = Some(v.0 as u32),
                _ => bail!("快捷键只支持一个主键"),
            }
        }
        Ok(Self(mods.0, key.context("请选择一个主键")?))
    }

    pub fn is_unmodified(self) -> bool {
        self.0 == 0
    }

    pub fn wait_for_release(self) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let pressed = |key| unsafe { GetAsyncKeyState(key) < 0 };
        let modifiers = [
            (MOD_CONTROL, VK_CONTROL),
            (MOD_ALT, VK_MENU),
            (MOD_SHIFT, VK_SHIFT),
            (MOD_WIN, VK_LWIN),
            (MOD_WIN, VK_RWIN),
        ];
        while pressed(self.1 as i32)
            || modifiers
                .iter()
                .any(|(m, k)| self.0 & m.0 != 0 && pressed(k.0 as i32))
        {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        true
    }
}

pub type Binding = (Chord, String);

enum Update {
    Set(Vec<Binding>, bool, mpsc::Sender<Result<()>>),
    EndReplay,
}

static THREAD: OnceLock<u32> = OnceLock::new();
static UPDATES: Mutex<Vec<Update>> = Mutex::new(Vec::new());
static REPLAYS: AtomicUsize = AtomicUsize::new(0);
static FAILURES: Mutex<Vec<(Chord, String)>> = Mutex::new(Vec::new());

pub struct ReplayGuard;

pub fn suppress_during_replay() -> ReplayGuard {
    REPLAYS.fetch_add(1, Relaxed);
    ReplayGuard
}

impl Drop for ReplayGuard {
    fn drop(&mut self) {
        // Process queued WM_HOTKEY messages before ending suppression.
        if submit(Update::EndReplay).is_err() {
            REPLAYS.fetch_sub(1, Relaxed);
        }
    }
}

pub fn start(on_hotkey: impl Fn(Chord) + Send + 'static) {
    let (ready, initialized) = mpsc::sync_channel(0);
    std::thread::Builder::new()
        .name("hotkey".into())
        .spawn(move || unsafe {
            let mut msg = MSG::default();
            let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
            let _ = ready.send(GetCurrentThreadId());
            let mut active = HashMap::new();
            let mut next_id = 1;
            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                match msg.message {
                    WM_HOTKEY if REPLAYS.load(Relaxed) == 0 => {
                        let data = msg.lParam.0 as u32;
                        let chord = Chord(data & 0xffff & !MOD_NOREPEAT.0, data >> 16);
                        if active.get(&chord) == Some(&(msg.wParam.0 as i32)) {
                            on_hotkey(chord);
                        }
                    }
                    WM_UPDATE => {
                        // Each message is a queue barrier, including replay completion.
                        let update = {
                            let mut pending = UPDATES.lock().unwrap();
                            if pending.is_empty() {
                                None
                            } else {
                                Some(pending.remove(0))
                            }
                        };
                        match update {
                            Some(Update::Set(bindings, strict, reply)) => {
                                let _ =
                                    reply.send(apply(&mut active, &mut next_id, bindings, strict));
                            }
                            Some(Update::EndReplay) => {
                                REPLAYS.fetch_sub(1, Relaxed);
                            }
                            None => {}
                        }
                    }
                    _ => {}
                }
            }
        })
        .expect("spawn hotkey thread");
    let _ = THREAD.set(initialized.recv().expect("initialize hotkey thread"));
}

fn submit(update: Update) -> Result<()> {
    let tid = *THREAD.get().context("快捷键服务尚未启动")?;
    let mut pending = UPDATES.lock().unwrap();
    pending.push(update);
    if let Err(error) = unsafe { PostThreadMessageW(tid, WM_UPDATE, WPARAM(0), LPARAM(0)) } {
        pending.pop();
        return Err(error.into());
    }
    Ok(())
}

pub fn set(bindings: Vec<Binding>, strict: bool) -> Result<()> {
    let (reply, result) = mpsc::channel();
    submit(Update::Set(bindings, strict, reply))?;
    result.recv().context("快捷键服务已退出")?
}

fn apply(
    active: &mut HashMap<Chord, i32>,
    next_id: &mut i32,
    bindings: Vec<Binding>,
    strict: bool,
) -> Result<()> {
    let mut added = Vec::new();
    let mut failed = Vec::new();
    for (chord, label) in &bindings {
        if active.contains_key(chord) {
            continue;
        }
        while active.values().any(|id| id == next_id) {
            *next_id = *next_id % 0xbfff + 1;
        }
        let id = *next_id;
        *next_id = id % 0xbfff + 1;
        let result =
            unsafe { RegisterHotKey(None, id, HOT_KEY_MODIFIERS(chord.0) | MOD_NOREPEAT, chord.1) };
        if let Err(error) = result {
            let message = format!("快捷键 {label} 注册失败，可能已被系统或其他程序占用（{error}）");
            // 之前就注册不上的组合键已经在界面上提示过，不阻止用户修改别的快捷键
            let known = FAILURES.lock().unwrap().iter().any(|(c, _)| c == chord);
            if strict && !known {
                for added_chord in added {
                    if let Some(id) = active.remove(&added_chord) {
                        unregister(id);
                    }
                }
                bail!("{message}");
            }
            log::warn!("{message}");
            failed.push((*chord, label.clone()));
        } else {
            active.insert(*chord, id);
            added.push(*chord);
        }
    }
    active.retain(|chord, id| {
        if bindings.iter().any(|(desired, _)| chord == desired) {
            return true;
        }
        unregister(*id);
        false
    });
    *FAILURES.lock().unwrap() = failed;
    Ok(())
}

/// 最近一次注册时被系统或其他程序占用的快捷键（显示文本）
pub fn failures() -> Vec<String> {
    FAILURES.lock().unwrap().iter().map(|(_, label)| label.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "registers temporary hotkeys on the Windows desktop"]
    fn desktop_registration_dispatch_and_conflict_rollback() {
        let first = Chord(MOD_CONTROL.0 | MOD_ALT.0 | MOD_SHIFT.0, VK_F23.0.into());
        let second = Chord(first.0, VK_F24.0.into());
        let blocked = Chord(first.0, VK_F22.0.into());
        let (sender, received) = mpsc::channel();
        start(move |chord| {
            let _ = sender.send(chord);
        });
        set(vec![(first, "first".into())], true).unwrap();

        unsafe { RegisterHotKey(None, 13, HOT_KEY_MODIFIERS(blocked.0), blocked.1) }.unwrap();
        let result = set(
            vec![(second, "second".into()), (blocked, "blocked".into())],
            true,
        );
        unregister(13);
        assert!(result.unwrap_err().to_string().contains("blocked"));
        // The failed update releases its new registration and preserves the original.
        unsafe { RegisterHotKey(None, 13, HOT_KEY_MODIFIERS(second.0), second.1) }.unwrap();
        unregister(13);
        let data = LPARAM((first.0 | (first.1 << 16)) as isize);
        unsafe { PostThreadMessageW(*THREAD.get().unwrap(), WM_HOTKEY, WPARAM(1), data) }.unwrap();
        assert_eq!(
            received
                .recv_timeout(std::time::Duration::from_secs(2))
                .unwrap(),
            first
        );
        set(vec![], true).unwrap();
        unsafe { RegisterHotKey(None, 13, HOT_KEY_MODIFIERS(first.0), first.1) }.unwrap();
        unregister(13);
    }

    #[test]
    fn shortcut_aliases_and_modifier_order_share_a_registration() {
        let parse = |keys: &[&str]| {
            Chord::parse(&keys.iter().map(|key| key.to_string()).collect::<Vec<_>>())
        };
        assert_eq!(
            parse(&["ctrl", "alt", "s"]).unwrap(),
            parse(&["lalt", "control", "S"]).unwrap()
        );
        assert!(parse(&["ctrl"]).is_err());
        assert!(parse(&["ctrl", "s", "t"]).is_err());
        assert!(parse(&["f1"]).unwrap().is_unmodified());
    }
}

fn unregister(id: i32) {
    let _ = unsafe { UnregisterHotKey(None, id) };
}
