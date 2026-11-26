use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager, WebviewWindow, WebviewWindowBuilder};

pub const LABEL: &str = "main";
pub const TRAY_ARG: &str = "--tray";

#[derive(Default)]
pub struct Lifecycle {
    inner: Mutex<State>,
}

#[derive(Default)]
struct State {
    phase: Phase,
    close_token: u32,
    release_requested: bool,
}

#[derive(Default, PartialEq, Eq)]
enum Phase {
    #[default]
    Ready,
    Opening,
    Closing {
        reopen: bool,
    },
}

pub fn show(app: &AppHandle) {
    {
        let state = app.state::<Lifecycle>();
        let mut state = state.inner.lock().unwrap();
        match &mut state.phase {
            Phase::Opening => return,
            Phase::Closing { reopen } => {
                *reopen = true;
                return;
            }
            Phase::Ready => {}
        }
        // 托盘快速点回设置时，作废旧页面尚未答复的关闭请求。
        state.release_requested = false;
        if let Some(win) = app.get_webview_window(LABEL) {
            drop(state);
            reveal(&win);
            return;
        }
        state.phase = Phase::Opening;
    }

    let handle = app.clone();
    // WebView2 初始化会泵送 Windows 消息。从 worker 发起创建，避免卡住窗口事件回调。
    let worker = std::thread::Builder::new()
        .name("settings-webview".into())
        .spawn(move || {
            let result = create(&handle);
            let _ = handle.run_on_main_thread({
                let handle = handle.clone();
                move || {
                    handle.state::<Lifecycle>().inner.lock().unwrap().phase = Phase::Ready;
                    match result {
                        Ok(win) => {
                            size_window(&win);
                            reveal(&win);
                        }
                        Err(e) => log::error!("创建设置窗口失败: {e}"),
                    }
                }
            });
        });
    if let Err(e) = worker {
        app.state::<Lifecycle>().inner.lock().unwrap().phase = Phase::Ready;
        log::error!("启动设置窗口线程失败: {e}");
    }
}

fn create(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == LABEL)
        .ok_or_else(|| tauri::Error::WindowNotFound)?;
    WebviewWindowBuilder::from_config(app, config)?
        .visible(false)
        .build()
}

fn reveal(win: &WebviewWindow) {
    let _ = win.show();
    let _ = win.unminimize();
    let _ = win.set_focus();
    super::notify_main_visibility(win.app_handle(), true);
}

/// 主窗口默认占当前显示器 65% 宽、75% 高。
fn size_window(win: &WebviewWindow) {
    let Ok(Some(monitor)) = win.current_monitor() else {
        return;
    };
    let screen = monitor.size().to_logical::<f64>(monitor.scale_factor());
    let size = tauri::LogicalSize::new(
        (screen.width * 0.65).max(880.0),
        (screen.height * 0.75).max(560.0),
    );
    let _ = win.set_size(size);
    let _ = win.center();
}

pub fn hide(app: &AppHandle) {
    gesture_platform::keyrec::sequence::cancel(None);
    crate::overlay::hide_preview(app);
    let Some(win) = app.get_webview_window(LABEL) else {
        return;
    };
    let token = {
        let state = app.state::<Lifecycle>();
        let mut state = state.inner.lock().unwrap();
        if state.phase != Phase::Ready {
            return;
        }
        state.close_token = state.close_token.wrapping_add(1);
        state.release_requested = true;
        state.close_token
    };
    let _ = win.hide();
    super::notify_main_visibility(app, false);
    // 前端确认没有草稿或保存操作后才销毁 WebView。
    let _ = win.emit_to(LABEL, "settings:release-requested", token);
}

#[tauri::command]
pub fn release_settings(win: WebviewWindow, token: u32) {
    if win.label() != LABEL {
        return;
    }
    let app = win.app_handle().clone();
    let _ = app.run_on_main_thread(move || {
        let state = win.app_handle().state::<Lifecycle>();
        {
            let mut state = state.inner.lock().unwrap();
            if state.phase != Phase::Ready || !state.release_requested || state.close_token != token
            {
                return;
            }
            state.release_requested = false;
            state.phase = Phase::Closing { reopen: false };
        }
        if let Err(e) = win.destroy() {
            state.inner.lock().unwrap().phase = Phase::Ready;
            log::warn!("释放设置窗口失败: {e}");
        }
    });
}

pub fn on_destroyed(app: &AppHandle) {
    let reopen = {
        let state = app.state::<Lifecycle>();
        let mut state = state.inner.lock().unwrap();
        matches!(
            std::mem::take(&mut state.phase),
            Phase::Closing { reopen: true }
        )
    };
    if reopen {
        show(app);
    }
}
