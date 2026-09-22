use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN, RegisterHotKey,
    UnregisterHotKey, VK_CONTROL, VK_LWIN, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, PostThreadMessageW, TranslateMessage, WM_APP, WM_HOTKEY,
};

use super::input::parse_key;

const ID: i32 = 1;
const WM_RELOAD: u32 = WM_APP + 1;

static THREAD: AtomicU32 = AtomicU32::new(0);
static PENDING: Mutex<Option<(HOT_KEY_MODIFIERS, u32)>> = Mutex::new(None);
static CALLBACK: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// 全局快捷键必须在带消息循环的线程上注册/接收
pub fn start(on_hotkey: impl Fn() + Send + Sync + 'static) {
    let _ = CALLBACK.set(Box::new(on_hotkey));
    std::thread::Builder::new()
        .name("hotkey".into())
        .spawn(|| unsafe {
            THREAD.store(GetCurrentThreadId(), Relaxed);
            apply();
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                match msg.message {
                    WM_HOTKEY => {
                        if let Some(cb) = CALLBACK.get() {
                            cb();
                        }
                    }
                    WM_RELOAD => apply(),
                    _ => {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            }
        })
        .expect("spawn hotkey thread");
}

/// 更新快捷键；空列表或解析失败则取消注册
pub fn set(keys: &[String]) {
    *PENDING.lock().unwrap() = parse(keys);
    let tid = THREAD.load(Relaxed);
    if tid != 0 {
        unsafe {
            let _ = PostThreadMessageW(tid, WM_RELOAD, WPARAM(0), LPARAM(0));
        }
    }
}

fn parse(keys: &[String]) -> Option<(HOT_KEY_MODIFIERS, u32)> {
    let mut mods = MOD_NOREPEAT;
    let mut vk = None;
    for k in keys {
        match parse_key(k)? {
            VK_CONTROL => mods |= MOD_CONTROL,
            VK_MENU => mods |= MOD_ALT,
            VK_SHIFT => mods |= MOD_SHIFT,
            VK_LWIN => mods |= MOD_WIN,
            v => vk = Some(v.0 as u32),
        }
    }
    vk.map(|v| (mods, v))
}

unsafe fn apply() {
    unsafe {
        let _ = UnregisterHotKey(None, ID);
        if let Some((mods, vk)) = *PENDING.lock().unwrap()
            && let Err(e) = RegisterHotKey(None, ID, mods, vk)
        {
            log::warn!("注册快捷键失败: {e}");
        }
    }
}
