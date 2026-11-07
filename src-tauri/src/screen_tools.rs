use std::collections::HashMap;
use std::sync::{Mutex, atomic::{AtomicBool, AtomicU64, Ordering::Relaxed}};

use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use gesture_core::ScreenTool;
use gesture_platform::{clipboard, hook, pip, screen::{self, Rect}, window};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

#[derive(Default)]
pub struct ScreenState {
    sessions: Mutex<HashMap<String, Session>>,
    busy: AtomicBool,
    next: AtomicU64,
}

struct Session {
    meta: SessionInfo,
    png: Vec<u8>,
    restore_main: bool,
}

#[derive(Clone, Serialize)]
pub struct SessionInfo {
    pub label: String,
    pub kind: String,
    pub tool: ScreenTool,
    pub bounds: Rect,
    pub click_through: bool,
}

#[derive(Default, Serialize)]
pub struct Output {
    pub text: Option<String>,
    pub path: Option<String>,
    pub label: Option<String>,
    pub canceled: bool,
}

pub fn image(app: &AppHandle, label: &str) -> Option<Vec<u8>> {
    app.state::<ScreenState>().sessions.lock().unwrap().get(label).map(|s| s.png.clone())
}

pub fn start(app: &AppHandle, tool: ScreenTool) -> Result<String> {
    screen::ensure_interactive_desktop()?;
    let state = app.state::<ScreenState>();
    ensure!(!state.busy.swap(true, Relaxed), "已有截图选区，请先完成或按 Esc 取消");
    let label = format!("capture-{}", state.next.fetch_add(1, Relaxed));
    let main = app.get_webview_window("main");
    let restore_main = main.as_ref().is_some_and(|w| w.is_focused().unwrap_or(false));
    let result = (|| -> Result<String> {
        hook::set_screen_selecting(true);
        if restore_main { if let Some(w) = &main { w.hide()?; } }
        if let Some(w) = app.get_webview_window(crate::overlay::LABEL) { w.hide()?; }
        // 等窗口隐藏提交到桌面合成器；这段等待只发生在截图工作线程。
        std::thread::sleep(std::time::Duration::from_millis(100));
        let bounds = screen::desktop_bounds();
        let png = screen::capture(bounds)?.png()?;
        let meta = SessionInfo { label: label.clone(), kind: "capture".into(), tool, bounds, click_through: false };
        state.sessions.lock().unwrap().insert(label.clone(), Session { meta, png, restore_main });
        let url = format!("tools.html?mode=capture");
        let win = WebviewWindowBuilder::new(app, &label, WebviewUrl::App(url.into()))
            .title("框选截图 · NextGestures").decorations(false).shadow(false).always_on_top(true)
            .skip_taskbar(true).resizable(false).visible(false).build()?;
        win.set_position(PhysicalPosition::new(bounds.x, bounds.y))?;
        win.set_size(PhysicalSize::new(bounds.width, bounds.height))?;
        win.show()?;
        win.set_focus()?;
        Ok(label.clone())
    })();
    if result.is_err() {
        state.sessions.lock().unwrap().remove(&label);
        state.busy.store(false, Relaxed);
        hook::set_screen_selecting(false);
        if let Some(win) = app.get_webview_window(&label) { let _ = win.destroy(); }
        if restore_main { if let Some(win) = main { let _ = win.show(); } }
        let _ = crate::overlay::ensure(app);
    }
    result
}

pub fn closed(app: &AppHandle, label: &str) {
    let state = app.state::<ScreenState>();
    let session = state.sessions.lock().unwrap().remove(label);
    if let Some(session) = session {
        if session.meta.kind == "capture" {
            state.busy.store(false, Relaxed);
            hook::set_screen_selecting(false);
            let _ = crate::overlay::ensure(app);
            if session.restore_main {
                if let Some(main) = app.get_webview_window("main") { let _ = main.show(); let _ = main.set_focus(); }
            }
        }
        let _ = app.emit("screen:windows-changed", ());
    }
}

#[tauri::command]
pub async fn start_screen_tool(app: AppHandle, tool: ScreenTool) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || start(&app, tool).map_err(|e| format!("{e:#}"))).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn screen_session(app: AppHandle, window: WebviewWindow) -> Result<SessionInfo, String> {
    app.state::<ScreenState>().sessions.lock().unwrap().get(window.label()).map(|s| s.meta.clone()).ok_or_else(|| "截图会话已结束".into())
}

#[tauri::command]
pub fn screen_windows(app: AppHandle) -> Vec<SessionInfo> {
    let mut out: Vec<_> = app.state::<ScreenState>().sessions.lock().unwrap().values().filter(|s| s.meta.kind == "pin").map(|s| s.meta.clone()).collect();
    for info in pip::list() {
        out.push(SessionInfo { label: format!("pip-{}", info.hwnd), kind: if info.paused { "pip_paused" } else { "pip" }.into(), tool: ScreenTool::Pip, bounds: Rect { x: 0, y: 0, width: 0, height: 0 }, click_through: false });
    }
    out
}

fn decode(data: &str) -> Result<Vec<u8>> {
    ensure!(data.len() <= 180 * 1024 * 1024, "图片文件过大");
    let data = data.strip_prefix("data:image/png;base64,").context("请使用 PNG 图片")?;
    let png = base64::engine::general_purpose::STANDARD.decode(data)?;
    screen::Frame::from_png(&png)?;
    Ok(png)
}

fn output(app: AppHandle, label: String, kind: String, data: String, rect: Option<Rect>, language: String) -> Result<Output> {
    let state = app.state::<ScreenState>();
    let meta = state.sessions.lock().unwrap().get(&label).map(|s| s.meta.clone()).context("截图会话已结束")?;
    if kind == "pip" {
        ensure!(meta.kind == "capture", "请重新框选画中画区域");
        let rect = rect.and_then(|r| r.intersection(meta.bounds)).context("选区不正确")?;
        ensure!(rect.width >= 2 && rect.height >= 2, "选区过小");
        let win = app.get_webview_window(&label).context("选区已关闭")?;
        win.hide()?;
        std::thread::sleep(std::time::Duration::from_millis(80));
        let source = window::root_hwnd_at((rect.x + (rect.width/2) as i32, rect.y + (rect.height/2) as i32));
        match pip::open(source, rect) {
            Ok(hwnd) => return Ok(Output { label: Some(format!("pip-{hwnd}")), ..Default::default() }),
            Err(e) => { let _ = win.show(); return Err(e); }
        }
    }
    let png = decode(&data)?;
    match kind.as_str() {
        "copy" => {
            let win = app.get_webview_window(&label).context("截图窗口已关闭")?;
            clipboard::set_image(&screen::Frame::from_png(&png)?, win.hwnd()?.0 as isize)?;
            Ok(Output::default())
        }
        "save" => {
            let win = app.get_webview_window(&label).context("截图窗口已关闭")?;
            let path = screen::save_png(win.hwnd()?.0 as isize, &png)?;
            Ok(Output { canceled: path.is_none(), path: path.map(|p| p.to_string_lossy().to_string()), ..Default::default() })
        }
        "ocr" => Ok(Output { text: Some(screen::ocr(&screen::Frame::from_png(&png)?, &language)?), ..Default::default() }),
        "pin" => {
            ensure!(state.sessions.lock().unwrap().values().filter(|s| s.meta.kind == "pin").count() < 20, "贴图数量已达 20 张，请先关闭部分贴图");
            let frame = screen::Frame::from_png(&png)?;
            let pin = format!("pin-{}", state.next.fetch_add(1, Relaxed));
            let scale = (720.0 / frame.width as f64).min(560.0 / frame.height as f64).min(1.0);
            let meta = SessionInfo { label: pin.clone(), kind: "pin".into(), tool: ScreenTool::Pin, bounds: Rect { x: 0, y: 0, width: frame.width, height: frame.height }, click_through: false };
            state.sessions.lock().unwrap().insert(pin.clone(), Session { meta, png, restore_main: false });
            let result = WebviewWindowBuilder::new(&app, &pin, WebviewUrl::App("tools.html?mode=pin".into()))
                .title("贴图 · NextGestures").decorations(false).transparent(true).always_on_top(true)
                .inner_size(frame.width as f64 * scale, frame.height as f64 * scale + 48.0)
                .min_inner_size(180.0, 100.0).center().build();
            if let Err(e) = result { state.sessions.lock().unwrap().remove(&pin); return Err(e.into()); }
            let _ = app.emit("screen:windows-changed", ());
            Ok(Output { label: Some(pin), ..Default::default() })
        }
        _ => bail!("未知图片操作"),
    }
}

#[tauri::command]
pub async fn screen_output(app: AppHandle, window: WebviewWindow, kind: String, data: String, rect: Option<Rect>, language: Option<String>) -> Result<Output, String> {
    let label = window.label().to_string();
    tauri::async_runtime::spawn_blocking(move || output(app, label, kind, data, rect, language.unwrap_or_default()).map_err(|e| format!("{e:#}"))).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn screen_ocr_languages() -> Result<Vec<screen::OcrLanguage>, String> {
    tauri::async_runtime::spawn_blocking(|| screen::ocr_languages().map_err(|e| format!("{e:#}"))).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn screen_copy_text(window: WebviewWindow, text: String) -> Result<(), String> {
    let owner = window.hwnd().map_err(|e| e.to_string())?.0 as isize;
    clipboard::set_text_for_window(&text, owner).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn screen_control(app: AppHandle, window: WebviewWindow, label: String, operation: String, value: Option<f64>) -> Result<(), String> {
    if window.label() != "main" && window.label() != label { return Err("窗口操作不匹配".into()); }
    if let Some(id) = label.strip_prefix("pip-") {
        if operation != "close" { return Err("请在画中画窗口调整选项".into()); }
        return pip::close(id.parse().map_err(|_| "窗口编号错误")?).map_err(|e| e.to_string());
    }
    let win = app.get_webview_window(&label).ok_or("窗口已关闭")?;
    match operation.as_str() {
        "close" => win.close().map_err(|e| e.to_string()),
        "interactive" => {
            let interactive = value.unwrap_or(1.0) != 0.0;
            win.set_ignore_cursor_events(!interactive).map_err(|e| e.to_string())?;
            if let Some(session) = app.state::<ScreenState>().sessions.lock().unwrap().get_mut(&label) { session.meta.click_through = !interactive; }
            let _ = app.emit("screen:windows-changed", ());
            Ok(())
        }
        "resize" => {
            let state = app.state::<ScreenState>();
            let meta = state.sessions.lock().unwrap().get(&label).map(|s| s.meta.clone()).ok_or("贴图已关闭")?;
            if meta.kind != "pin" { return Err("当前窗口不是贴图".into()); }
            let scale = value.unwrap_or(1.0).clamp(0.1, 3.0);
            win.set_size(tauri::LogicalSize::new((meta.bounds.width as f64 * scale).clamp(180.0, 4096.0), (meta.bounds.height as f64 * scale + 48.0).clamp(100.0, 4096.0))).map_err(|e| e.to_string())
        }
        _ => Err("未知窗口操作".into()),
    }
}
