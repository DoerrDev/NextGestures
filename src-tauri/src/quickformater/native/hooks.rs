//! 低级键盘/鼠标钩子：在不抢焦点的前提下实现 Esc 关闭、点击外部关闭、悬浮窗滚轮滚动。

use super::{WM_APP_ESC, WM_APP_OUTSIDE, WM_APP_WHEEL};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU32, Ordering};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::PtInRect;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE;
use windows::Win32::UI::WindowsAndMessaging::*;

static KB_HOOK: AtomicIsize = AtomicIsize::new(0);
static MS_HOOK: AtomicIsize = AtomicIsize::new(0);
static MAIN_TID: AtomicU32 = AtomicU32::new(0);
static POPUP: AtomicIsize = AtomicIsize::new(0);
static POPUP_INSET: AtomicI32 = AtomicI32::new(0);
static ESC_SWALLOWED: AtomicBool = AtomicBool::new(false);

pub fn install(main_tid: u32) {
    if KB_HOOK.load(Ordering::SeqCst) != 0 {
        return;
    }
    MAIN_TID.store(main_tid, Ordering::SeqCst);
    ESC_SWALLOWED.store(false, Ordering::SeqCst);
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
        if let Ok(h) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(kb_proc), Some(hinst), 0) {
            KB_HOOK.store(h.0 as isize, Ordering::SeqCst);
        }
        if let Ok(h) = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(hinst), 0) {
            MS_HOOK.store(h.0 as isize, Ordering::SeqCst);
        }
    }
}

/// 在执行粘贴等阻塞操作前必须卸载，否则本线程阻塞会拖慢全局输入
pub fn uninstall() {
    set_popup(HWND::default(), 0);
    for h in [&KB_HOOK, &MS_HOOK] {
        let v = h.swap(0, Ordering::SeqCst);
        if v != 0 {
            unsafe {
                let _ = UnhookWindowsHookEx(HHOOK(v as *mut _));
            }
        }
    }
}

pub fn set_popup(hwnd: HWND, inset: i32) {
    POPUP_INSET.store(inset, Ordering::SeqCst);
    POPUP.store(hwnd.0 as isize, Ordering::SeqCst);
}

fn post(msg: u32, wparam: usize) {
    unsafe {
        let _ = PostThreadMessageW(
            MAIN_TID.load(Ordering::SeqCst),
            msg,
            WPARAM(wparam),
            LPARAM(0),
        );
    }
}

unsafe extern "system" fn kb_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let k = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        let injected = k.flags.0 & LLKHF_INJECTED.0 != 0;
        if k.vkCode == VK_ESCAPE.0 as u32 && !injected {
            let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
            if down && !ESC_SWALLOWED.swap(true, Ordering::SeqCst) {
                post(WM_APP_ESC, 0);
            } else if !down {
                ESC_SWALLOWED.store(false, Ordering::SeqCst);
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let popup = POPUP.load(Ordering::SeqCst);
    if code == HC_ACTION as i32 && popup != 0 {
        let m = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        let hwnd = HWND(popup as *mut _);
        let mut r = RECT::default();
        let valid = GetWindowRect(hwnd, &mut r).is_ok();
        let inset = POPUP_INSET.load(Ordering::SeqCst);
        r.left += inset;
        r.top += inset;
        r.right -= inset;
        r.bottom -= inset;
        let inside = valid && PtInRect(&r, m.pt).as_bool();
        match wparam.0 as u32 {
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN if !inside => {
                post(WM_APP_OUTSIDE, 0);
            }
            WM_MOUSEWHEEL if inside => {
                let delta = (m.mouseData >> 16) as u16 as i16;
                let _ = PostMessageW(
                    Some(hwnd),
                    WM_APP_WHEEL,
                    WPARAM(delta as isize as usize),
                    LPARAM(0),
                );
                return LRESULT(1);
            }
            _ => {}
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}
