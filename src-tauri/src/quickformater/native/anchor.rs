//! 确定"选中文字"在屏幕上的位置：UI Automation 选区矩形 > 系统插入符 > 鼠标位置。

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Ole::*;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::Interface;

pub fn rect_valid(r: &RECT) -> bool {
    r.bottom > r.top && r.right >= r.left
}

fn union(a: Option<RECT>, b: RECT) -> RECT {
    match a {
        None => b,
        Some(a) => RECT {
            left: a.left.min(b.left),
            top: a.top.min(b.top),
            right: a.right.max(b.right),
            bottom: a.bottom.max(b.bottom),
        },
    }
}

pub fn selected_rect() -> Option<RECT> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().ok()?;
        struct Com;
        impl Drop for Com {
            fn drop(&mut self) {
                unsafe { CoUninitialize() }
            }
        }
        let _com = Com;
        let uia: IUIAutomation =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
        let mut element = uia.GetFocusedElement().ok()?;
        let walker = uia.ControlViewWalker().ok();
        for _ in 0..4 {
            if let Some(rect) = text_pattern_rect(&element) {
                return Some(rect);
            }
            element = walker.as_ref()?.GetParentElement(&element).ok()?;
        }
        None
    }
}

unsafe fn text_pattern_rect(el: &IUIAutomationElement) -> Option<RECT> {
    let unk = el.GetCurrentPattern(UIA_TextPatternId).ok()?;
    let tp: IUIAutomationTextPattern = unk.cast().ok()?;
    let ranges = tp.GetSelection().ok()?;
    let n = ranges.Length().ok()?;
    let mut acc: Option<RECT> = None;
    for i in 0..n {
        let Ok(range) = ranges.GetElement(i) else {
            continue;
        };
        let Ok(sa) = range.GetBoundingRectangles() else {
            continue;
        };
        if sa.is_null() {
            continue;
        }
        for q in read_f64_array(sa).chunks_exact(4) {
            if q[3] <= 0.0 {
                continue;
            }
            let r = RECT {
                left: q[0].round() as i32,
                top: q[1].round() as i32,
                right: (q[0] + q[2]).round() as i32,
                bottom: (q[1] + q[3]).round() as i32,
            };
            acc = Some(union(acc, r));
        }
        let _ = SafeArrayDestroy(sa);
    }
    acc
}

unsafe fn read_f64_array(sa: *mut SAFEARRAY) -> Vec<f64> {
    let (Ok(lb), Ok(ub)) = (SafeArrayGetLBound(sa, 1), SafeArrayGetUBound(sa, 1)) else {
        return Vec::new();
    };
    if ub < lb {
        return Vec::new();
    }
    let mut p = std::ptr::null_mut();
    if SafeArrayAccessData(sa, &mut p).is_err() || p.is_null() {
        return Vec::new();
    }
    let v = std::slice::from_raw_parts(p as *const f64, (ub - lb + 1) as usize).to_vec();
    let _ = SafeArrayUnaccessData(sa);
    v
}

/// 系统插入符位置（Chrome 等自绘控件通常没有）
pub fn caret_rect(fg: HWND) -> Option<RECT> {
    unsafe {
        let tid = GetWindowThreadProcessId(fg, None);
        let mut gti = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        GetGUIThreadInfo(tid, &mut gti).ok()?;
        if gti.hwndCaret.0.is_null() {
            return None;
        }
        let r = gti.rcCaret;
        if r.bottom <= r.top {
            return None;
        }
        let mut a = POINT {
            x: r.left,
            y: r.top,
        };
        let mut b = POINT {
            x: r.right,
            y: r.bottom,
        };
        let _ = ClientToScreen(gti.hwndCaret, &mut a);
        let _ = ClientToScreen(gti.hwndCaret, &mut b);
        log::debug!("caret rect {:?}", (a.x, a.y, b.x, b.y));
        Some(RECT {
            left: a.x,
            top: a.y,
            right: b.x.max(a.x + 1),
            bottom: b.y,
        })
    }
}

pub fn cursor_pos() -> POINT {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    p
}

/// 以鼠标为锚点：近似光标本身的高度
pub fn mouse_rect((x, y): (i32, i32)) -> RECT {
    let p = POINT { x, y };
    let h = (10.0
        * scale_for(&RECT {
            left: p.x,
            top: p.y,
            right: p.x + 1,
            bottom: p.y + 1,
        })) as i32;
    RECT {
        left: p.x - 1,
        top: p.y - h,
        right: p.x + 1,
        bottom: p.y + h,
    }
}

fn monitor_info(r: &RECT) -> MONITORINFO {
    unsafe {
        let mon = MonitorFromRect(r, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(mon, &mut mi);
        mi
    }
}

pub fn work_area(r: &RECT) -> RECT {
    monitor_info(r).rcWork
}

pub fn scale_for(r: &RECT) -> f32 {
    unsafe {
        let mon = MonitorFromRect(r, MONITOR_DEFAULTTONEAREST);
        let (mut x, mut y) = (96u32, 96u32);
        if GetDpiForMonitor(mon, MDT_EFFECTIVE_DPI, &mut x, &mut y).is_ok() && x > 0 {
            x as f32 / 96.0
        } else {
            1.0
        }
    }
}

/// 把锚点裁剪到所在显示器内；完全在屏幕外（比如选区被滚出视野）则返回 None
pub fn clip_to_monitor(r: RECT) -> Option<RECT> {
    let m = monitor_info(&r).rcMonitor;
    let c = RECT {
        left: r.left.max(m.left),
        top: r.top.max(m.top),
        right: r.right.min(m.right),
        bottom: r.bottom.min(m.bottom),
    };
    rect_valid(&c).then_some(c)
}
