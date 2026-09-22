use anyhow::{Context, Result};
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize};

pub const LABEL: &str = "overlay";

/// 覆盖全部显示器的透明置顶穿透窗口。
/// 常驻显示、永不激活：手势执行快捷键时绝不能把焦点从目标程序抢走。
pub fn setup(app: &AppHandle) -> Result<()> {
    let win = app.get_webview_window(LABEL).context("找不到 overlay 窗口")?;
    let (origin, size) = virtual_bounds(app)?;
    win.set_position(PhysicalPosition::new(origin.0, origin.1))?;
    win.set_size(PhysicalSize::new(size.0, size.1))?;
    win.set_ignore_cursor_events(true)?;
    win.set_always_on_top(true)?;
    no_activate(&win)?;
    win.show()?;
    Ok(())
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
    // 无条件重设：这是把窗口重新提到 topmost 序列顶部、解开 WebView2 遮挡冻结的唯一可靠方式
    win.set_always_on_top(true)?;
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
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(
            hwnd,
            GWL_EXSTYLE,
            ex | WS_EX_NOACTIVATE.0 as isize | WS_EX_TOOLWINDOW.0 as isize,
        );
    }
    Ok(())
}

#[cfg(not(windows))]
fn no_activate(_win: &tauri::WebviewWindow) -> Result<()> {
    Ok(())
}
