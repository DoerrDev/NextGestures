use std::cell::RefCell;
use std::sync::{Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use anyhow::{Result, anyhow, bail};
use gesture_core::KeySequenceEvent;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use super::key_name;

const WM_CAPTURED: u32 = WM_APP + 1;

struct Session {
    id: String,
    tid: u32,
    thread: JoinHandle<Vec<KeySequenceEvent>>,
}

static ACTIVE: Mutex<Option<Session>> = Mutex::new(None);

pub fn begin(
    id: String,
    owner: usize,
    on_event: impl Fn(KeySequenceEvent) + Send + 'static,
) -> Result<()> {
    if unsafe { GetForegroundWindow() }.0 as usize != owner {
        bail!("请在设置窗口中开始录制");
    }
    let mut active = ACTIVE.lock().unwrap();
    if active.is_some() {
        bail!("快捷键序列正在录制");
    }
    let (ready, result) = mpsc::sync_channel(1);
    let thread = thread::Builder::new()
        .name("key-sequence-recorder".into())
        .spawn(move || run(owner, ready, on_event))?;
    let tid = result.recv()??;
    *active = Some(Session { id, tid, thread });
    Ok(())
}

pub fn stop(id: &str) -> Result<Vec<KeySequenceEvent>> {
    let session = end(Some(id)).ok_or_else(|| anyhow!("快捷键序列录制已结束"))?;
    session
        .thread
        .join()
        .map_err(|_| anyhow!("快捷键序列录制异常结束"))
}

pub fn cancel(id: Option<&str>) {
    drop(end(id));
}

fn end(id: Option<&str>) -> Option<Session> {
    let mut active = ACTIVE.lock().unwrap();
    if id.is_some_and(|id| active.as_ref().is_none_or(|session| session.id != id)) {
        return None;
    }
    let session = active.take()?;
    unsafe {
        let _ = PostThreadMessageW(session.tid, WM_QUIT, WPARAM(0), LPARAM(0));
    }
    Some(session)
}

thread_local! {
    static CAPTURE: RefCell<Capture> = RefCell::new(Capture::default());
}

fn run(
    owner: usize,
    ready: mpsc::SyncSender<Result<u32>>,
    on_event: impl Fn(KeySequenceEvent),
) -> Vec<KeySequenceEvent> {
    unsafe {
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        CAPTURE.with(|state| {
            let mut state = state.borrow_mut();
            state.owner = owner;
            state.already_down = (8..=254)
                .filter(|&vk| GetAsyncKeyState(i32::from(vk)) < 0)
                .collect();
        });
        let hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) {
            Ok(hook) => hook,
            Err(error) => {
                let _ = ready.send(Err(error.into()));
                return Vec::new();
            }
        };
        let _ = ready.send(Ok(GetCurrentThreadId()));
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            if msg.message == WM_CAPTURED {
                let event = CAPTURE.with(|state| state.borrow().events[msg.wParam.0].clone());
                on_event(event);
            }
        }
        let _ = UnhookWindowsHookEx(hook);
    }
    CAPTURE.with(|state| std::mem::take(&mut *state.borrow_mut()).finish(Instant::now()))
}

unsafe extern "system" fn hook_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let key = unsafe { &*(lp.0 as *const KBDLLHOOKSTRUCT) };
        if !key.flags.contains(LLKHF_INJECTED) {
            let swallowed = CAPTURE.with(|state| {
                let mut state = state.borrow_mut();
                if unsafe { GetForegroundWindow() }.0 as usize != state.owner {
                    return false;
                }
                let Some(event) = physical_key(key) else {
                    return false;
                };
                let index = state.events.len();
                let swallow = state.record(event, Instant::now());
                if index < state.events.len() {
                    unsafe {
                        let _ = PostThreadMessageW(
                            GetCurrentThreadId(),
                            WM_CAPTURED,
                            WPARAM(index),
                            LPARAM(0),
                        );
                    }
                }
                swallow
            });
            if swallowed {
                return LRESULT(1);
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wp, lp) }
}

fn physical_key(key: &KBDLLHOOKSTRUCT) -> Option<KeySequenceEvent> {
    let vk = VIRTUAL_KEY(key.vkCode as u16);
    if vk.0 == 0 || vk == VK_PACKET {
        return None;
    }
    let extended = key.flags.contains(LLKHF_EXTENDED);
    let label = match vk {
        VK_LCONTROL | VK_RCONTROL | VK_CONTROL => "ctrl".into(),
        VK_LMENU | VK_RMENU | VK_MENU => "alt".into(),
        VK_LSHIFT | VK_RSHIFT | VK_SHIFT => "shift".into(),
        VK_LWIN | VK_RWIN => "win".into(),
        VK_RETURN if extended => "num enter".into(),
        _ if (VK_NUMPAD0.0..=VK_NUMPAD9.0).contains(&vk.0) => {
            format!("num {}", vk.0 - VK_NUMPAD0.0)
        }
        VK_ADD => "num +".into(),
        VK_SUBTRACT => "num −".into(),
        VK_MULTIPLY => "num *".into(),
        VK_DIVIDE => "num /".into(),
        VK_DECIMAL => "num .".into(),
        _ => key_name(vk).unwrap_or_else(|| format!("VK {:02X}", vk.0)),
    };
    Some(KeySequenceEvent {
        vk: vk.0,
        scan: (key.scanCode & 0xff) as u16,
        extended,
        down: !key.flags.contains(LLKHF_UP),
        delay_ms: 0,
        label,
    })
}

#[derive(Default)]
struct Capture {
    owner: usize,
    events: Vec<KeySequenceEvent>,
    already_down: Vec<u16>,
    // Indexes of the press events for the keys still held.
    held: Vec<usize>,
    start: Option<Instant>,
    last_ms: u32,
}

impl Capture {
    fn record(&mut self, event: KeySequenceEvent, now: Instant) -> bool {
        if let Some(index) = self.already_down.iter().position(|&vk| vk == event.vk) {
            if !event.down {
                self.already_down.swap_remove(index);
            }
            return false;
        }
        let held = self.held.iter().position(|&i| {
            let start = &self.events[i];
            (start.vk, start.scan, start.extended) == (event.vk, event.scan, event.extended)
        });
        match (event.down, held) {
            (true, Some(_)) => return true, // Keyboard autorepeat is not a new press.
            (true, None) => self.held.push(self.events.len()),
            (false, Some(i)) => {
                self.held.remove(i);
            }
            (false, None) => return false, // Pressed before recording started.
        }
        self.push(event, now);
        true
    }

    fn push(&mut self, mut event: KeySequenceEvent, now: Instant) {
        let start = *self.start.get_or_insert(now);
        let elapsed = now
            .saturating_duration_since(start)
            .as_millis()
            .min(u32::MAX as u128) as u32;
        event.delay_ms = elapsed.saturating_sub(self.last_ms);
        self.last_ms = elapsed;
        self.events.push(event);
    }

    fn finish(mut self, now: Instant) -> Vec<KeySequenceEvent> {
        while let Some(i) = self.held.pop() {
            let mut release = self.events[i].clone();
            release.down = false;
            self.push(release, now);
        }
        self.events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    #[ignore = "requires a Windows input desktop"]
    fn desktop_replay_preserves_event_order_and_intervals() {
        use crate::windows::{hook::MAGIC, input::replay_sequence};

        thread_local! {
            static OBSERVED: RefCell<Vec<(u16, bool, Instant)>> = const { RefCell::new(Vec::new()) };
        }

        unsafe extern "system" fn observe(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
            if code == HC_ACTION as i32 {
                let key = unsafe { &*(lp.0 as *const KBDLLHOOKSTRUCT) };
                let vk = key.vkCode as u16;
                if key.dwExtraInfo == MAGIC && [VK_F23.0, VK_F24.0].contains(&vk) {
                    OBSERVED.with(|events| {
                        events.borrow_mut().push((
                            vk,
                            !key.flags.contains(LLKHF_UP),
                            Instant::now(),
                        ));
                    });
                    return LRESULT(1);
                }
            }
            unsafe { CallNextHookEx(None, code, wp, lp) }
        }

        let script = [
            (VK_F23, true, 0),
            (VK_F24, true, 70),
            (VK_F24, false, 90),
            (VK_F23, false, 120),
        ];
        let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(observe), None, 0) }.unwrap();
        let replay = thread::spawn(move || {
            let events = script.map(|(vk, down, delay_ms)| KeySequenceEvent {
                vk: vk.0,
                scan: unsafe { MapVirtualKeyW(u32::from(vk.0), MAPVK_VK_TO_VSC) } as u16,
                extended: false,
                down,
                delay_ms,
                label: String::new(),
            });
            replay_sequence(&events)
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        while !replay.is_finished() && Instant::now() < deadline {
            unsafe {
                let _ = PeekMessageW(&mut MSG::default(), None, 0, 0, PM_NOREMOVE);
            }
            thread::sleep(Duration::from_millis(1));
        }
        unsafe { UnhookWindowsHookEx(hook) }.unwrap();
        replay.join().unwrap().unwrap();
        let observed = OBSERVED.with(|events| events.take());
        assert_eq!(
            observed
                .iter()
                .map(|&(vk, down, _)| (vk, down))
                .collect::<Vec<_>>(),
            script.map(|(vk, down, _)| (vk.0, down))
        );
        let gaps: Vec<_> = observed
            .windows(2)
            .map(|pair| (pair[1].2 - pair[0].2).as_millis())
            .collect();
        println!("Windows key event gaps: {gaps:?} ms");
        for (actual, (_, _, expected)) in gaps.into_iter().zip(script.into_iter().skip(1)) {
            assert!(
                actual.abs_diff(u128::from(expected)) < 40,
                "{actual} vs {expected} ms"
            );
        }
    }

    #[test]
    fn keeps_overlap_and_timing_and_releases_keys_still_held_at_stop() {
        let mut capture = Capture::default();
        let start = Instant::now();
        let mut key = |ms, vk: VIRTUAL_KEY, down| {
            capture.record(
                KeySequenceEvent {
                    vk: vk.0,
                    scan: 1,
                    extended: false,
                    down,
                    delay_ms: 0,
                    label: String::new(),
                },
                start + Duration::from_millis(ms),
            )
        };
        assert!(!key(0, VK_A, false));
        assert!(key(10, VK_LCONTROL, true));
        assert!(key(30, VK_A, true));
        assert!(key(50, VK_A, true));
        assert!(key(70, VK_A, false));
        assert!(key(500, VK_B, true));
        let events = capture.finish(start + Duration::from_millis(600));
        assert_eq!(
            events
                .iter()
                .map(|e| (e.vk, e.down, e.delay_ms))
                .collect::<Vec<_>>(),
            [
                (VK_LCONTROL.0, true, 0),
                (VK_A.0, true, 20),
                (VK_A.0, false, 40),
                (VK_B.0, true, 430),
                (VK_B.0, false, 100),
                (VK_LCONTROL.0, false, 0)
            ],
        );
    }
}
