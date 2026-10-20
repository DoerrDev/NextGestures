use anyhow::{Context, Result};
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize};

use crate::state::AppState;

pub const LABEL: &str = "overlay";

/// 覆盖全部显示器的透明穿透窗口（默认置顶，可在设置里关闭）。
/// 常驻显示、永不激活：手势执行快捷键时绝不能把焦点从目标程序抢走。
pub fn setup(app: &AppHandle) -> Result<()> {
    let win = app.get_webview_window(LABEL).context("找不到 overlay 窗口")?;
    let (origin, size) = virtual_bounds(app)?;
    win.set_position(PhysicalPosition::new(origin.0, origin.1))?;
    win.set_size(PhysicalSize::new(size.0, size.1))?;
    win.set_ignore_cursor_events(true)?;
    win.set_always_on_top(topmost_enabled(app))?;
    no_activate(&win)?;
    win.show()?;
    apply_z_order(app)
}

/// 每次手势开始前校正：锁屏/UAC/全屏程序会打掉置顶，显示器休眠或分辨率变化会挪动窗口
pub fn ensure(app: &AppHandle) -> Result<()> {
    let win = app.get_webview_window(LABEL).context("找不到 overlay 窗口")?;
    let (origin, size) = virtual_bounds(app)?;
    let want_pos = PhysicalPosition::new(origin.0, origin.1);
    let want_size = PhysicalSize::new(size.0, size.1);
    if win.outer_position().ok() != Some(want_pos) {
        win.set_position(want_pos)?;
    }
    if win.outer_size().ok() != Some(want_size) {
        win.set_size(want_size)?;
    }
    if !win.is_visible().unwrap_or(false) {
        win.show()?;
    }
    apply_z_order(app)
}

/// 「触发提示置顶显示」设置项，未加载配置时按默认开启处理
fn topmost_enabled(app: &AppHandle) -> bool {
    app.try_state::<AppState>()
        .map(|s| s.config.read().unwrap().settings.gesture_hint_topmost)
        .unwrap_or(true)
}

/// 按设置重新声明 overlay 的 Z 序；手势开始、弹提示、设置保存时调用。
///
/// 不能只靠 `set_always_on_top(true)`：tao 只在内部标志变化时才调用 SetWindowPos，
/// 标志一直是 true 时它是空操作。而 explorer、全屏程序、其它后创建的置顶窗口
/// 都会把 overlay 压到下面（甚至被 SetWindowPos(HWND_NOTOPMOST) 打掉置顶），
/// 之后提示就只能在桌面上看到。所以每次都直接 SetWindowPos(HWND_TOPMOST) 提到置顶序列最上面。
pub fn apply_z_order(app: &AppHandle) -> Result<()> {
    let win = app.get_webview_window(LABEL).context("找不到 overlay 窗口")?;
    let on_top = topmost_enabled(app);
    // 先同步 tao 的内部标志：它在任何标志变化时都会按标志重写 GWL_EXSTYLE，标志不对会把置顶改回去
    win.set_always_on_top(on_top)?;
    // tao 的窗口操作是投递到主线程执行的，这里同样投递过去，保证排在它后面
    let w = win.clone();
    win.run_on_main_thread(move || {
        let _ = no_activate(&w);
        let _ = raise(&w, on_top);
    })?;
    Ok(())
}

/// overlay 窗口左上角在虚拟屏幕中的物理坐标，前端用它换算 canvas 坐标
pub fn origin(app: &AppHandle) -> (i32, i32) {
    virtual_bounds(app).map(|(o, _)| o).unwrap_or((0, 0))
}

pub fn size(app: &AppHandle) -> (u32, u32) {
    virtual_bounds(app).map(|(_, s)| s).unwrap_or((1920, 1080))
}

fn virtual_bounds(app: &AppHandle) -> Result<((i32, i32), (u32, u32))> {
    let monitors = app.available_monitors()?;
    let mut bounds: Option<(i32, i32, i32, i32)> = None;
    for m in &monitors {
        let p = m.position();
        let s = m.size();
        let (l, t, r, b) = (p.x, p.y, p.x + s.width as i32, p.y + s.height as i32);
        bounds = Some(match bounds {
            None => (l, t, r, b),
            Some(o) => (o.0.min(l), o.1.min(t), o.2.max(r), o.3.max(b)),
        });
    }
    let (l, t, r, b) = bounds.context("没有可用显示器")?;
    Ok(((l, t), ((r - l) as u32, (b - t) as u32)))
}

#[cfg(windows)]
fn no_activate(win: &tauri::WebviewWindow) -> Result<()> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, SetWindowLongPtrW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    };
    let hwnd = HWND(win.hwnd()?.0 as *mut std::ffi::c_void);
    let want = WS_EX_NOACTIVATE.0 as isize | WS_EX_TOOLWINDOW.0 as isize;
    unsafe {
        // tao 改任何窗口标志时都会按自己的标志重写扩展样式，这两位会丢，需要补回
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if ex & want != want {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | want);
        }
    }
    Ok(())
}

/// 开启时提到置顶序列最上面；关闭时降为普通窗口（已是普通窗口则不改变 Z 序）。
/// SWP_NOACTIVATE 保证不抢焦点。
#[cfg(windows)]
fn raise(win: &tauri::WebviewWindow, on_top: bool) -> Result<()> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
        SetWindowPos,
    };
    let hwnd = HWND(win.hwnd()?.0 as *mut std::ffi::c_void);
    let after = if on_top { HWND_TOPMOST } else { HWND_NOTOPMOST };
    unsafe {
        SetWindowPos(
            hwnd,
            Some(after),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        )?;
    }
    Ok(())
}

#[cfg(not(windows))]
fn no_activate(_win: &tauri::WebviewWindow) -> Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn raise(_win: &tauri::WebviewWindow, _on_top: bool) -> Result<()> {
    Ok(())
}
