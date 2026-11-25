use std::sync::atomic::Ordering::Relaxed;

use gesture_core::{Action, Config, Gesture, KeySequenceEvent, Profile, Settings};
use gesture_platform::keyrec::{self, KeyRecEvent};
use gesture_platform::{hook, shell, window};
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

#[derive(Serialize)]
pub struct InstalledApp {
    id: String,
    name: String,
    path: String,
    icon: Option<String>,
}

#[tauri::command]
pub async fn list_installed_apps() -> R<Vec<InstalledApp>> {
    tauri::async_runtime::spawn_blocking(|| {
        shell::installed_apps().map(|apps| {
            apps.into_iter()
                .map(|app| InstalledApp {
                    id: app.id,
                    name: app.name,
                    path: app.path,
                    icon: app.icon,
                })
                .collect()
        })
    })
    .await
    .map_err(err)?
    .map_err(err)
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
pub fn save_settings(app: AppHandle, state: State<AppState>, mut settings: Settings) -> R<()> {
    {
        let mut cfg = state.config.write().unwrap();
        // 黑名单独立即时保存，避免旧设置快照把名单覆盖回去。
        settings.blacklist = cfg.settings.blacklist.clone();
        cfg.settings = settings;
    }
    commit(&app)?;
    // 「触发提示置顶」立即生效，不必等下一次手势
    let _ = crate::overlay::apply_z_order(&app);
    Ok(())
}

#[tauri::command]
pub fn save_blacklist(app: AppHandle, state: State<AppState>, names: Vec<String>) -> R<()> {
    state.config.write().unwrap().settings.set_blacklist(names).map_err(err)?;
    commit(&app)?;
    app.emit("config:changed", ()).map_err(err)
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
    // 在任意边角原地单击任意键都录成「边角 + 单击」
    hook::set_click_mask(u32::MAX);
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
pub fn cancel_pick() {
    hook::cancel_pick();
}

/// 录制快捷键：由原生键盘钩子抢在系统和其它软件的热键之前截获按键，
/// 按住的修饰键通过 `hotkey:mods` 实时回报，录到的组合键通过 `hotkey:recorded` 回报
#[tauri::command]
pub fn begin_key_recording(app: AppHandle) {
    keyrec::begin(move |ev| {
        let _ = match ev {
            KeyRecEvent::Mods(mods) => app.emit("hotkey:mods", mods),
            KeyRecEvent::Done(keys) => app.emit("hotkey:recorded", keys),
        };
    });
}

#[tauri::command]
pub fn cancel_key_recording() {
    keyrec::cancel();
}

#[derive(Clone, Serialize)]
struct KeySequencePreview {
    id: String,
    event: KeySequenceEvent,
}

#[tauri::command]
pub fn begin_key_sequence(window: tauri::WebviewWindow, id: String) -> R<()> {
    let owner = window.hwnd().map_err(err)?.0 as usize;
    let preview_id = id.clone();
    keyrec::sequence::begin(id, owner, move |event| {
        let _ = window.emit("key-sequence:event", KeySequencePreview { id: preview_id.clone(), event });
    })
    .map_err(err)
}

#[tauri::command]
pub async fn stop_key_sequence(id: String) -> R<Vec<KeySequenceEvent>> {
    tauri::async_runtime::spawn_blocking(move || keyrec::sequence::stop(&id))
        .await
        .map_err(err)?
        .map_err(err)
}

#[tauri::command]
pub fn cancel_key_sequence(id: String) {
    keyrec::sequence::cancel(Some(&id));
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
pub async fn test_action(app: AppHandle, window: tauri::WebviewWindow, action: Action) -> R<()> {
    if matches!(action, Action::KeySequence { .. }) && window.is_focused().map_err(err)? {
        return Err("请在倒计时结束前切换到要测试的窗口".into());
    }
    tauri::async_runtime::spawn_blocking(move || crate::action::run(&app, &action))
        .await
        .map_err(err)?
        .map_err(|e| format!("{e:#}"))
}

/// 设置页打开「触发提示」时向实际桌面 overlay 投送预览，不写入配置。
#[tauri::command]
pub fn show_gesture_hint_preview(app: AppHandle, hint: GestureHintPreview) {
    let _ = crate::overlay::ensure(&app);
    let _ = app.emit_to(crate::overlay::LABEL, "gesture:hint-preview", hint);
}

#[tauri::command]
pub fn hide_gesture_hint_preview(app: AppHandle) {
    let _ = app.emit_to(crate::overlay::LABEL, "gesture:hint-preview:hide", ());
}

#[tauri::command]
pub fn hide_settings(app: AppHandle) {
    keyrec::sequence::cancel(None);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
    crate::notify_main_visibility(&app, false);
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
