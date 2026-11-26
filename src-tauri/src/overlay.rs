use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewWindow};

use crate::state::AppState;

pub const LABEL: &str = "overlay";
const LOAD_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_PENDING_EVENTS: usize = 4096;

#[derive(Default)]
pub struct Overlay(Mutex<Lifecycle>);

#[derive(Default)]
enum Phase {
    #[default]
    Absent,
    Loading {
        native_ready_at: Option<Instant>,
        listeners_ready: bool,
    },
    Ready,
    Closing,
}

#[derive(Serialize)]
struct Pending {
    event: &'static str,
    payload: serde_json::Value,
}

#[derive(Default)]
struct Lifecycle {
    phase: Phase,
    generation: u32,
    pending: Vec<Pending>,
    preview_open: bool,
    timer_running: bool,
}

impl Lifecycle {
    fn deadline(&self) -> Option<Instant> {
        match self.phase {
            Phase::Loading {
                native_ready_at, ..
            } => native_ready_at.map(|at| at + LOAD_TIMEOUT),
            _ => None,
        }
    }

    fn start_loading(&mut self) -> u32 {
        self.generation = self.generation.wrapping_add(1);
        self.phase = Phase::Loading {
            native_ready_at: None,
            listeners_ready: false,
        };
        self.generation
    }
}

/// 设置页仍按需创建，轨迹层随后台启动预热，避免第一笔等待 WebView2。
pub fn prewarm(app: &AppHandle) {
    let generation = {
        let overlay = app.state::<Overlay>();
        let mut state = overlay.0.lock().unwrap();
        if !matches!(state.phase, Phase::Absent) {
            return;
        }
        state.start_loading()
    };
    request_create(app, generation);
}

/// 启动事件会按需创建窗口，后续事件只向现有或已排队创建的窗口发送。
pub fn emit(app: &AppHandle, event: &'static str, payload: impl Serialize + Clone) {
    let opens = matches!(
        event,
        "trail:begin" | "gesture:hint-show" | "gesture:toast" | "gesture:hint-preview"
    );
    let (create, reposition) = {
        let overlay = app.state::<Overlay>();
        let mut state = overlay.0.lock().unwrap();
        if matches!(state.phase, Phase::Absent | Phase::Closing)
            && !opens
            && state.pending.is_empty()
        {
            return;
        }
        if event == "gesture:hint-preview:hide" && !state.preview_open {
            return;
        }
        match event {
            "gesture:hint-preview" => state.preview_open = true,
            "gesture:hint-preview:hide" => state.preview_open = false,
            _ => {}
        }
        if matches!(state.phase, Phase::Ready) {
            // 持锁发送，保证 ready 刷出的冷启动事件先于后续事件。
            let _ = app.emit_to(LABEL, event, payload);
            (None, opens)
        } else {
            if let Ok(payload) = serde_json::to_value(payload) {
                if event == "gesture:hint-preview"
                    && state.pending.last().is_some_and(|p| p.event == event)
                {
                    state.pending.pop();
                }
                if state.pending.len() >= MAX_PENDING_EVENTS {
                    if event == "trail:points" {
                        return;
                    }
                    if let Some(i) = state.pending.iter().position(|p| p.event == "trail:points") {
                        state.pending.remove(i);
                    } else {
                        state.pending.remove(0);
                    }
                }
                state.pending.push(Pending { event, payload });
            }
            let create = matches!(state.phase, Phase::Absent).then(|| state.start_loading());
            (create, false)
        }
    };
    if let Some(generation) = create {
        request_create(app, generation);
    } else if reposition {
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            if let Some(win) = handle.get_webview_window(LABEL) {
                let _ = position(&handle, &win).and_then(|_| apply_z_order(&handle));
            }
        });
    }
}

pub fn show_preview(app: &AppHandle, hint: impl Serialize + Clone) {
    emit(app, "gesture:hint-preview", hint);
}

pub fn hide_preview(app: &AppHandle) {
    emit(app, "gesture:hint-preview:hide", ());
}

fn request_create(app: &AppHandle, generation: u32) {
    let handle = app.clone();
    // WebView2 initialization pumps Windows messages. Build off the invoking event/IPC
    // stack, then apply native flags on the UI thread before making the window visible.
    std::thread::spawn(move || {
        let built = create(&handle, generation);
        let app = handle.clone();
        let _ = handle.run_on_main_thread(move || {
            let initialized = built.and_then(|win| {
                position(&app, &win)?;
                win.set_ignore_cursor_events(true)?;
                no_activate(&win)
            });
            if let Err(error) = initialized {
                log::warn!("创建手势轨迹层失败: {error:#}");
                close(&app);
                return;
            }
            let overlay = app.state::<Overlay>();
            if let Phase::Loading {
                native_ready_at, ..
            } = &mut overlay.0.lock().unwrap().phase
            {
                *native_ready_at = Some(Instant::now());
            }
            show_when_ready(&app, generation);
            arm_load_timeout(&app);
        });
    });
}

fn create(app: &AppHandle, generation: u32) -> Result<WebviewWindow> {
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == LABEL)
        .context("找不到 overlay 窗口配置")?;
    let win = tauri::WebviewWindowBuilder::from_config(app, config)?
        .visible(false)
        .focused(false)
        .focusable(false)
        .initialization_script(format!(
            "window.__nextGesturesOverlayGeneration = {generation}"
        ))
        .build()?;
    Ok(win)
}

#[tauri::command]
pub fn overlay_ready(window: WebviewWindow, generation: u32) {
    if window.label() != LABEL {
        return;
    }
    let app = window.app_handle();
    let overlay = app.state::<Overlay>();
    {
        let mut state = overlay.0.lock().unwrap();
        if state.generation != generation {
            return;
        }
        if let Phase::Loading {
            listeners_ready, ..
        } = &mut state.phase
        {
            *listeners_ready = true;
        }
    }
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || show_when_ready(&handle, generation));
}

fn show_when_ready(app: &AppHandle, generation: u32) {
    let overlay = app.state::<Overlay>();
    let ready_to_show = |state: &Lifecycle| {
        state.generation == generation
            && matches!(
                state.phase,
                Phase::Loading {
                    native_ready_at: Some(_),
                    listeners_ready: true
                }
            )
    };
    if !ready_to_show(&overlay.0.lock().unwrap()) {
        return;
    }
    let Some(win) = app.get_webview_window(LABEL) else {
        return;
    };
    // Set Z order first because it may change native style flags.
    if let Err(error) = apply_z_order(app).and_then(|_| win.show().map_err(Into::into)) {
        log::warn!("显示手势轨迹层失败: {error:#}");
        close(app);
        return;
    }
    let mut state = overlay.0.lock().unwrap();
    if !ready_to_show(&state) {
        return;
    }
    state.phase = Phase::Ready;
    let pending = std::mem::take(&mut state.pending);
    let _ = app.emit_to(LABEL, "overlay:replay", &pending);
}

fn arm_load_timeout(app: &AppHandle) {
    let overlay = app.state::<Overlay>();
    let mut state = overlay.0.lock().unwrap();
    let Some(deadline) = state.deadline() else {
        return;
    };
    if state.timer_running {
        return;
    }
    state.timer_running = true;
    drop(state);
    let handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        let app = handle.clone();
        let _ = handle.run_on_main_thread(move || {
            let overlay = app.state::<Overlay>();
            let mut state = overlay.0.lock().unwrap();
            state.timer_running = false;
            let expired = state.deadline().is_some_and(|at| at <= Instant::now());
            if expired {
                state.phase = Phase::Closing;
                state.pending = Vec::new();
                state.preview_open = false;
            }
            drop(state);
            if expired {
                destroy_window(&app);
            } else {
                arm_load_timeout(&app);
            }
        });
    });
}

fn close(app: &AppHandle) {
    let overlay = app.state::<Overlay>();
    {
        let mut state = overlay.0.lock().unwrap();
        state.phase = Phase::Closing;
        state.pending = Vec::new();
        state.preview_open = false;
    }
    destroy_window(app);
}

fn destroy_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(LABEL) {
        let _ = win.hide();
        if let Err(error) = win.destroy() {
            log::warn!("关闭手势轨迹层失败: {error}");
        }
    } else {
        on_destroyed(app);
    }
}

/// 在 Tauri 清理旧标签之后执行，避免与同名的新 WebView 窗口冲突。
pub fn on_destroyed(app: &AppHandle) {
    let overlay = app.state::<Overlay>();
    let create = {
        let mut state = overlay.0.lock().unwrap();
        state.phase = Phase::Absent;
        if state.pending.is_empty() {
            state.preview_open = false;
            None
        } else {
            Some(state.start_loading())
        }
    };
    if let Some(generation) = create {
        request_create(app, generation);
    }
}

/// 每次手势开始前校正位置。显示器休眠或分辨率变化可能挪动窗口。
fn position(app: &AppHandle, win: &WebviewWindow) -> Result<()> {
    let (origin, size) = virtual_bounds(app)?;
    let want_pos = PhysicalPosition::new(origin.0, origin.1);
    let want_size = PhysicalSize::new(size.0, size.1);
    if win.outer_position().ok() != Some(want_pos) {
        win.set_position(want_pos)?;
    }
    if win.outer_size().ok() != Some(want_size) {
        win.set_size(want_size)?;
    }
    Ok(())
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
    let Some(win) = app.get_webview_window(LABEL) else {
        return Ok(());
    };
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
