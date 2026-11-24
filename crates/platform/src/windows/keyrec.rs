//! 快捷键录制。
//!
//! 录制期间装一个低级键盘钩子把按键全部吞掉：低级钩子先于 RegisterHotKey 和系统
//! 快捷键（Win+方向键等）执行，后装的钩子又先于别的软件的钩子，所以这里能录到
//! 已经被其它程序占用的组合键，而且不会触发它们。Win+L、Ctrl+Alt+Del 系统不让拦。

use std::cell::RefCell;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    VIRTUAL_KEY, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RCONTROL, VK_RMENU, VK_RSHIFT,
    VK_RWIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, MSG, PM_NOREMOVE, PeekMessageW,
    PostQuitMessage, PostThreadMessageW, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL,
    WM_APP, WM_KEYDOWN, WM_QUIT, WM_SYSKEYDOWN,
};

use super::hook::MAGIC;
use super::input::key_name;

pub mod sequence;

const WM_MODS: u32 = WM_APP + 1;
const WM_DONE: u32 = WM_APP + 2;
/// 录完后继续吞键直到全部松开；这么久没有任何按键事件就认为抬起事件丢了，直接收工
const DRAIN_IDLE: Duration = Duration::from_secs(2);

const CTRL: u8 = 1;
const ALT: u8 = 2;
const SHIFT: u8 = 4;
const WIN: u8 = 8;
/// 输出顺序，与前端展示一致
const MOD_NAMES: [(u8, &str); 4] = [(CTRL, "ctrl"), (ALT, "alt"), (SHIFT, "shift"), (WIN, "win")];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyRecEvent {
    /// 当前按住的修饰键，用于实时显示
    Mods(Vec<String>),
    /// 录到一个完整组合键（修饰键 + 主键）
    Done(Vec<String>),
}

/// 当前可被 cancel 的录制会话：(代数, 钩子线程 id)。线程 id 为 0 表示还没起来或已脱离
static ACTIVE: Mutex<(u64, u32)> = Mutex::new((0, 0));

/// 开始录制，回调在钩子线程的消息循环里执行（不在钩子回调内）。
/// 录到组合键后会话自行结束；同一时刻只有一个会话，重复调用会顶掉上一个。
pub fn begin(cb: impl Fn(KeyRecEvent) + Send + 'static) {
    cancel();
    let generation = ACTIVE.lock().unwrap().0;
    std::thread::Builder::new()
        .name("key-recorder".into())
        .spawn(move || unsafe { run(generation, cb) })
        .expect("spawn key-recorder thread");
}

/// 取消录制。已经录到键、正在等按键松开的会话不受影响，它会自己退出
pub fn cancel() {
    let mut active = ACTIVE.lock().unwrap();
    active.0 += 1;
    let tid = std::mem::take(&mut active.1);
    if tid != 0 {
        unsafe {
            let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }
}

/// 让当前线程的会话脱离 ACTIVE，之后的 cancel 不会再打断它
fn detach(tid: u32) {
    let mut active = ACTIVE.lock().unwrap();
    if active.1 == tid {
        active.1 = 0;
    }
}

unsafe fn run(generation: u64, cb: impl Fn(KeyRecEvent)) {
    unsafe {
        let mut msg = MSG::default();
        // 先建好消息队列，cancel 的 PostThreadMessage 才投得进来
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        let tid = GetCurrentThreadId();
        {
            let mut active = ACTIVE.lock().unwrap();
            if active.0 != generation {
                return;
            }
            active.1 = tid;
        }
        let hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) {
            Ok(h) => h,
            Err(e) => {
                log::error!("安装键盘钩子失败: {e}");
                detach(tid);
                return;
            }
        };
        STATE.with(|s| *s.borrow_mut() = State::new());
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            match msg.message {
                WM_MODS => cb(KeyRecEvent::Mods(mod_names(msg.wParam.0 as u8))),
                WM_DONE => {
                    if let Some(key) = key_name(VIRTUAL_KEY(msg.lParam.0 as u16)) {
                        let mut keys = mod_names(msg.wParam.0 as u8);
                        keys.push(key);
                        cb(KeyRecEvent::Done(keys));
                    }
                }
                _ => {}
            }
        }
        let _ = UnhookWindowsHookEx(hook);
        detach(tid);
    }
}

fn mod_names(mods: u8) -> Vec<String> {
    MOD_NAMES.iter().filter(|(bit, _)| mods & bit != 0).map(|(_, n)| n.to_string()).collect()
}

fn mod_bit(vk: u32) -> Option<u8> {
    Some(match VIRTUAL_KEY(vk as u16) {
        VK_LCONTROL | VK_RCONTROL => CTRL,
        VK_LMENU | VK_RMENU => ALT,
        VK_LSHIFT | VK_RSHIFT => SHIFT,
        VK_LWIN | VK_RWIN => WIN,
        _ => return None,
    })
}

struct State {
    /// 按下被我们吞掉、还没松开的键
    held: Vec<u32>,
    mods: u8,
    /// 已经录到主键，只剩等所有键松开
    done: bool,
    last: Instant,
}

impl State {
    fn new() -> Self {
        Self { held: Vec::new(), mods: 0, done: false, last: Instant::now() }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Out {
    Mods(u8),
    Done(u8, u32),
    Quit,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::new());
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let k = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        // 我们自己合成的按键（手势动作）照常放行
        if k.dwExtraInfo != MAGIC {
            let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            let (swallow, out) = STATE.with(|s| on_key(&mut s.borrow_mut(), k.vkCode, down));
            let tid = unsafe { GetCurrentThreadId() };
            let post = |msg, w: u8, l: u32| unsafe {
                let _ = PostThreadMessageW(tid, msg, WPARAM(w as usize), LPARAM(l as isize));
            };
            match out {
                Some(Out::Mods(mods)) => post(WM_MODS, mods, 0),
                Some(Out::Done(mods, vk)) => {
                    // 前端收到结果后会调 cancel，不能让它在按键松开前把钩子拆掉，
                    // 否则长按产生的重复按键会漏给占用这个快捷键的程序
                    detach(tid);
                    post(WM_DONE, mods, vk);
                }
                Some(Out::Quit) => unsafe { PostQuitMessage(0) },
                None => {}
            }
            if swallow {
                return LRESULT(1);
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// 返回 (是否吞掉, 需要通知的事情)
fn on_key(st: &mut State, vk: u32, down: bool) -> (bool, Option<Out>) {
    if st.done && st.last.elapsed() > DRAIN_IDLE {
        st.held.clear();
        return (false, Some(Out::Quit));
    }

    if !down {
        // 只吞我们吞过按下的键；录制开始前就按着的键，抬起必须正常送达，否则会卡键
        let Some(i) = st.held.iter().position(|&v| v == vk) else {
            return (false, None);
        };
        st.held.swap_remove(i);
        st.last = Instant::now();
        if st.done {
            return (true, st.held.is_empty().then_some(Out::Quit));
        }
        // 左右两个同类修饰键都松开才算松开
        if let Some(bit) = mod_bit(vk)
            && !st.held.iter().any(|&v| mod_bit(v) == Some(bit))
        {
            st.mods &= !bit;
            return (true, Some(Out::Mods(st.mods)));
        }
        return (true, None);
    }

    let hold = |st: &mut State| {
        if !st.held.contains(&vk) {
            st.held.push(vk);
        }
        st.last = Instant::now();
    };

    if st.done {
        hold(st);
        return (true, None);
    }
    if let Some(bit) = mod_bit(vk) {
        hold(st);
        let changed = st.mods & bit == 0;
        st.mods |= bit;
        return (true, changed.then_some(Out::Mods(st.mods)));
    }
    // 不认识的键（媒体键、小键盘等）不参与录制，原样放行
    if key_name(VIRTUAL_KEY(vk as u16)).is_none() {
        return (false, None);
    }
    hold(st);
    st.done = true;
    (true, Some(Out::Done(st.mods, vk)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_F1, VK_LEFT, VK_VOLUME_UP};

    fn vk(k: VIRTUAL_KEY) -> u32 {
        k.0 as u32
    }

    #[test]
    fn records_win_arrow_and_swallows_until_released() {
        let mut st = State::new();
        assert_eq!(on_key(&mut st, vk(VK_LWIN), true), (true, Some(Out::Mods(WIN))));
        // 修饰键长按重复不再通知
        assert_eq!(on_key(&mut st, vk(VK_LWIN), true), (true, None));
        assert_eq!(on_key(&mut st, vk(VK_LEFT), true), (true, Some(Out::Done(WIN, vk(VK_LEFT)))));
        // 录到之后长按重复、抬起都继续吞，全部松开才退出
        assert_eq!(on_key(&mut st, vk(VK_LEFT), true), (true, None));
        assert_eq!(on_key(&mut st, vk(VK_LEFT), false), (true, None));
        assert_eq!(on_key(&mut st, vk(VK_LWIN), false), (true, Some(Out::Quit)));
    }

    #[test]
    fn bare_key_is_recorded_without_modifiers() {
        let mut st = State::new();
        assert_eq!(on_key(&mut st, vk(VK_F1), true), (true, Some(Out::Done(0, vk(VK_F1)))));
        assert_eq!(on_key(&mut st, vk(VK_F1), false), (true, Some(Out::Quit)));
    }

    #[test]
    fn releasing_a_modifier_before_the_main_key_drops_it() {
        let mut st = State::new();
        on_key(&mut st, vk(VK_LCONTROL), true);
        assert_eq!(on_key(&mut st, vk(VK_RCONTROL), true), (true, None));
        assert_eq!(on_key(&mut st, vk(VK_LSHIFT), true), (true, Some(Out::Mods(CTRL | SHIFT))));
        // 另一侧的 ctrl 还按着
        assert_eq!(on_key(&mut st, vk(VK_LCONTROL), false), (true, None));
        assert_eq!(on_key(&mut st, vk(VK_RCONTROL), false), (true, Some(Out::Mods(SHIFT))));
        assert_eq!(on_key(&mut st, vk(VK_F1), true), (true, Some(Out::Done(SHIFT, vk(VK_F1)))));
    }

    #[test]
    fn keys_held_before_recording_and_unknown_keys_pass_through() {
        let mut st = State::new();
        assert_eq!(on_key(&mut st, vk(VK_LCONTROL), false), (false, None));
        assert_eq!(on_key(&mut st, vk(VK_VOLUME_UP), true), (false, None));
        assert_eq!(on_key(&mut st, vk(VK_VOLUME_UP), false), (false, None));
        assert!(!st.done);
    }

    #[test]
    fn stale_drain_gives_up_and_lets_keys_through() {
        let mut st = State::new();
        on_key(&mut st, vk(VK_F1), true);
        st.last = Instant::now() - DRAIN_IDLE - Duration::from_millis(1);
        assert_eq!(on_key(&mut st, vk(VK_F1), true), (false, Some(Out::Quit)));
    }

    /// 真实注入按键：先确认 Shift+F13 能触发别人注册的全局热键，
    /// 再确认录制期间它被我们录到，而那个热键不再触发
    #[test]
    #[ignore = "injects real keyboard input; run alone"]
    fn recording_beats_a_registered_global_hotkey() {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
            MOD_SHIFT, RegisterHotKey, SendInput, UnregisterHotKey, VK_F13,
        };
        use windows::Win32::UI::WindowsAndMessaging::{PM_REMOVE, WM_HOTKEY};

        fn press() {
            let key = |vk: VIRTUAL_KEY, up: bool| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: vk,
                        dwFlags: if up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) },
                        ..Default::default()
                    },
                },
            };
            let batch = [
                key(VK_LSHIFT, false),
                key(VK_F13, false),
                key(VK_F13, true),
                key(VK_LSHIFT, true),
            ];
            unsafe { SendInput(&batch, size_of::<INPUT>() as i32) };
        }
        fn hotkey_fired() -> bool {
            let deadline = Instant::now() + Duration::from_millis(500);
            let mut msg = MSG::default();
            while Instant::now() < deadline {
                if unsafe { PeekMessageW(&mut msg, None, WM_HOTKEY, WM_HOTKEY, PM_REMOVE) }.as_bool()
                {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            false
        }

        unsafe { RegisterHotKey(None, 99, MOD_SHIFT, VK_F13.0 as u32) }.unwrap();
        press();
        assert!(hotkey_fired(), "baseline: the hotkey should fire without the recorder");

        let (tx, rx) = std::sync::mpsc::channel();
        begin(move |ev| {
            let _ = tx.send(ev);
        });
        std::thread::sleep(Duration::from_millis(300));
        press();
        let mut events = Vec::new();
        while let Ok(ev) = rx.recv_timeout(Duration::from_millis(500)) {
            events.push(ev);
        }
        let fired = hotkey_fired();
        cancel();
        unsafe { UnregisterHotKey(None, 99) }.unwrap();

        assert!(events.contains(&KeyRecEvent::Done(vec!["shift".into(), "f13".into()])), "{events:?}");
        assert!(!fired, "the other hotkey must not fire while recording");
        // 全部松开后会话自行结束，不留钩子线程
        assert_eq!(ACTIVE.lock().unwrap().1, 0);
    }

    #[test]
    fn modifier_names_keep_display_order() {
        assert_eq!(mod_names(WIN | CTRL), ["ctrl", "win"]);
    }
}
