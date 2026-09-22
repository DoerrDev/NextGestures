use std::sync::atomic::Ordering::Relaxed;

use gesture_core::{Action, Config, Gesture, Profile, Settings};
use gesture_platform::{hook, window};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::state::AppState;

type R<T> = Result<T, String>;

#[derive(Clone, Deserialize, Serialize)]
pub struct GestureHintPreview {
    font_size: f32,
    color: String,
    offset_x: i32,
    offset_y: i32,
}

#[derive(Serialize)]
pub struct VirtualScreenSize {
    width: u32,
    height: u32,
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// 改完配置统一落盘并通知前端
fn commit(app: &AppHandle) -> R<()> {
    app.state::<AppState>().save().map_err(err)
}

#[tauri::command]
pub fn get_config(state: State<AppState>) -> Config {
    state.config.read().unwrap().clone()
}

#[tauri::command]
pub fn config_path(state: State<AppState>) -> String {
    state.path.display().to_string()
}

#[tauri::command]
pub fn save_settings(app: AppHandle, state: State<AppState>, settings: Settings) -> R<()> {
    state.config.write().unwrap().settings = settings;
    commit(&app)
}

#[tauri::command]
pub fn upsert_gesture(
    app: AppHandle,
    state: State<AppState>,
    profile_id: String,
    gesture: Gesture,
) -> R<()> {
    state.config.write().unwrap().upsert_gesture(&profile_id, gesture).map_err(err)?;
    commit(&app)
}

#[tauri::command]
pub fn remove_gesture(
    app: AppHandle,
    state: State<AppState>,
    profile_id: String,
    gesture_id: String,
) -> R<()> {
    state.config.write().unwrap().remove_gesture(&profile_id, &gesture_id).map_err(err)?;
    commit(&app)
}

#[tauri::command]
pub fn set_gesture_enabled(
    app: AppHandle,
    state: State<AppState>,
    profile_id: String,
    gesture_id: String,
    enabled: bool,
) -> R<()> {
    {
        let mut cfg = state.config.write().unwrap();
        let profile = cfg.profile_mut(&profile_id).ok_or("未找到配置组")?;
        let g = profile.gestures.iter_mut().find(|g| g.id == gesture_id).ok_or("未找到手势")?;
        g.enabled = enabled;
    }
    commit(&app)
}

#[tauri::command]
pub fn upsert_profile(app: AppHandle, state: State<AppState>, profile: Profile) -> R<()> {
    {
        let mut cfg = state.config.write().unwrap();
        match cfg.profiles.iter_mut().find(|p| p.id == profile.id) {
            Some(slot) => {
                slot.name = profile.name;
                slot.exe = profile.exe;
            }
            None => cfg.profiles.push(profile),
        }
    }
    commit(&app)
}

#[tauri::command]
pub fn remove_profile(app: AppHandle, state: State<AppState>, profile_id: String) -> R<()> {
    {
        let mut cfg = state.config.write().unwrap();
        if cfg.profiles.iter().any(|p| p.id == profile_id && p.is_global()) {
            return Err("Global 配置组不能删除".into());
        }
        cfg.profiles.retain(|p| p.id != profile_id);
    }
    commit(&app)
}

/// 录制期间放开全部触发键，方便用户用任意键录
#[tauri::command]
pub fn start_recording(state: State<AppState>) {
    state.recording.store(true, Relaxed);
    hook::set_trigger_mask(0b1111);
    hook::set_region_mask(0xff);
}

#[tauri::command]
pub fn cancel_recording(state: State<AppState>) {
    state.recording.store(false, Relaxed);
    state.push_to_hook();
}

/// 准心拾取：下一次左键松开时通过 `pick:result` 事件回报 exe
#[tauri::command]
pub fn begin_pick() {
    hook::begin_pick();
}

#[tauri::command]
pub fn set_enabled(app: AppHandle, enabled: bool) {
    crate::set_enabled(&app, enabled);
}

#[tauri::command]
pub fn is_enabled() -> bool {
    hook::is_enabled()
}

#[tauri::command]
pub fn foreground_exe() -> String {
    window::foreground_exe()
}

#[tauri::command]
pub fn virtual_screen_size(app: AppHandle) -> VirtualScreenSize {
    let (width, height) = crate::overlay::size(&app);
    VirtualScreenSize { width, height }
}

#[tauri::command]
pub fn test_action(action: Action) -> R<()> {
    crate::action::run(&action).map_err(|e| format!("{e:#}"))
}

/// 设置页打开「触发提示」时向实际桌面 overlay 投送预览，不写入配置。
#[tauri::command]
pub fn show_gesture_hint_preview(app: AppHandle, hint: GestureHintPreview) {
    let _ = app.emit_to(crate::overlay::LABEL, "gesture:hint-preview", hint);
}

#[tauri::command]
pub fn hide_gesture_hint_preview(app: AppHandle) {
    let _ = app.emit_to(crate::overlay::LABEL, "gesture:hint-preview:hide", ());
}

#[tauri::command]
pub fn hide_settings(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
}

#[tauri::command]
pub fn is_autostart_enabled() -> bool {
    crate::autostart::is_enabled()
}

#[tauri::command]
pub fn set_autostart_enabled(enabled: bool) -> R<()> {
    crate::autostart::set_enabled(enabled)
}

#[tauri::command]
pub fn feedback_chat(
    app: AppHandle,
    backend: State<crate::feedback::Backend>,
) -> R<Vec<crate::feedback::ChatItem>> {
    backend.chat(&app)
}

/// 提交反馈并把昵称写进配置，返回服务端 ack 文案
#[tauri::command]
pub fn feedback_submit(
    app: AppHandle,
    state: State<AppState>,
    backend: State<crate::feedback::Backend>,
    name: String,
    message: String,
) -> R<String> {
    let ack = backend.submit(&app, &name, &message)?;
    if state.config.read().unwrap().settings.feedback_nickname != name {
        state.config.write().unwrap().settings.feedback_nickname = name;
        commit(&app)?;
    }
    Ok(ack)
}

#[tauri::command]
pub fn feedback_ack_unread(app: AppHandle, backend: State<crate::feedback::Backend>) -> R<()> {
    backend.ack_unread(&app)
}
