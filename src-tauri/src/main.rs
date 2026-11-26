#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod action;
mod autostart;
mod commands;
mod engine;
mod feedback;
mod overlay;
mod pin;
mod quickformater;
mod recorder;
mod settings_window;
mod state;

use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{App, AppHandle, Emitter, Manager, RunEvent, WindowEvent, Wry};

use gesture_platform::{hook, hotkey};
use state::AppState;

struct TrayToggles {
    gestures: CheckMenuItem<Wry>,
    shortcuts_paused: CheckMenuItem<Wry>,
}

/// 统一的手势总开关入口：钩子状态 + 托盘勾选 + 通知前端
pub fn set_enabled(app: &AppHandle, on: bool) {
    hook::set_enabled(on);
    if let Some(toggles) = app.try_state::<TrayToggles>() {
        let _ = toggles.gestures.set_checked(on);
    }
    let _ = app.emit("enabled:changed", on);
    engine::toast(app, if on { "手势已启用" } else { "手势已停用" });
}

fn toggle_shortcuts(app: &AppHandle) {
    let state = app.state::<AppState>();
    let message = match state.toggle_shortcuts_paused() {
        Ok(true) => "快捷键已暂停".into(),
        Ok(false) => {
            let failed = hotkey::failures();
            if failed.is_empty() {
                "快捷键已恢复".into()
            } else {
                format!("快捷键已恢复，以下组合键被占用：{}", failed.join("、"))
            }
        }
        Err(error) => {
            log::warn!("切换快捷键暂停状态失败: {error:#}");
            "切换快捷键暂停状态失败".into()
        }
    };
    let _ = app
        .state::<TrayToggles>()
        .shortcuts_paused
        .set_checked(state.shortcuts_paused());
    engine::toast(app, &message);
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let tray_only = std::env::args_os().skip(1).any(|arg| arg == settings_window::TRAY_ARG);

    // 必须在 Builder 构建窗口之前注入，否则前端首帧 invoke 会拿不到 State
    let dir = std::path::PathBuf::from(std::env::var("APPDATA").expect("缺少 APPDATA"))
        .join("com.nextgestures.app");
    let state = AppState::load(dir.join("config.toml")).expect("加载配置失败");

    tauri::Builder::default()
        // 默认情况下窗口聚焦时会注册 Raw Input，此时系统不再回调本进程的低级键盘钩子，
        // 设置窗口在前台时快捷键录制就一个键都收不到。我们不用 DeviceEvent，直接关掉
        .device_event_filter(tauri::DeviceEventFilter::Always)
        // 只允许一个后台实例运行；重复启动时唤醒首实例的设置窗口。
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            if !args.iter().skip(1).any(|arg| arg == settings_window::TRAY_ARG) {
                settings_window::show(app);
            }
        }))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(state)
        .manage(settings_window::Lifecycle::default())
        .manage(overlay::Overlay::default())
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::config_path,
            commands::save_settings,
            commands::remove_ai_action,
            commands::upsert_gesture,
            commands::remove_gesture,
            commands::set_gesture_enabled,
            commands::upsert_profile,
            commands::remove_profile,
            commands::set_profile_passthrough,
            commands::hotkey_failures,
            commands::start_recording,
            commands::cancel_recording,
            commands::begin_pick,
            commands::cancel_pick,
            commands::begin_key_recording,
            commands::cancel_key_recording,
            commands::begin_key_sequence,
            commands::stop_key_sequence,
            commands::cancel_key_sequence,
            commands::set_enabled,
            commands::is_enabled,
            commands::foreground_exe,
            commands::virtual_screen_size,
            commands::test_action,
            commands::list_installed_apps,
            commands::show_gesture_hint_preview,
            commands::hide_gesture_hint_preview,
            overlay::overlay_ready,
            commands::hide_settings,
            settings_window::release_settings,
            commands::is_autostart_enabled,
            commands::set_autostart_enabled,
            commands::feedback_chat,
            commands::feedback_submit,
            commands::feedback_ack_unread,
        ])
        .setup(move |app| {
            engine::start(app.handle());
            build_tray(app)?;
            app.manage(feedback::Backend::new(app.package_info().version.to_string()));
            feedback::start(app.handle());
            let handle = app.handle().clone();
            hotkey::start(move |chord| dispatch_shortcut(&handle, chord));
            app.state::<AppState>().init_shortcuts()?;
            overlay::prewarm(app.handle());
            if !tray_only {
                settings_window::show(app.handle());
            }
            Ok(())
        })
        .on_window_event(|win, event| {
            if win.label() != settings_window::LABEL {
                return;
            }
            match event {
                WindowEvent::Focused(false) => gesture_platform::keyrec::sequence::cancel(None),
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    settings_window::hide(win.app_handle());
                }
                WindowEvent::Resized(_) => {
                    let shown = win.is_visible().unwrap_or(true) && !win.is_minimized().unwrap_or(false);
                    notify_main_visibility(win.app_handle(), shown);
                }
                _ => {}
            }
        })
        .build(tauri::generate_context!())
        .expect("启动 NextGestures 失败")
        .run(|app, event| match event {
            // 最后一个 WebView 被释放后，托盘、手势和快捷键仍需运行。
            RunEvent::ExitRequested { code: None, api, .. } => api.prevent_exit(),
            RunEvent::WindowEvent { label, event: WindowEvent::Destroyed, .. } => {
                if label == settings_window::LABEL {
                    settings_window::on_destroyed(app);
                } else if label == overlay::LABEL {
                    overlay::on_destroyed(app);
                }
            }
            _ => {}
        });
}

fn dispatch_shortcut(app: &AppHandle, chord: hotkey::Chord) {
    use gesture_platform::window;
    let state = app.state::<AppState>();
    // Registration updates wait for this thread, so never block on the configuration lock here.
    let Ok(config) = state.config.try_read() else { return };
    if state.shortcuts_paused() {
        return;
    }
    if hotkey::Chord::parse(&config.settings.toggle_hotkey).ok() == Some(chord) {
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            if !handle.state::<AppState>().shortcuts_paused() {
                set_enabled(&handle, !hook::is_enabled());
            }
        });
        return;
    }
    // 前台程序的配置组优先，其次 Global；「交给程序自带手势」的程序里不响应
    let exe = window::foreground_exe();
    let Some(hit) = gesture_core::matcher::hotkey_gesture(&config, &exe, |keys| {
        hotkey::Chord::parse(keys).ok() == Some(chord)
    }) else { return };
    let action = hit.gesture.action.clone();
    drop(config);
    action::spawn_from_shortcut(app.clone(), action, chord);
}

/// 隐藏到托盘时 WebView2 不会把页面标成 hidden，前端动画会继续空转吃 GPU，
/// 这里显式告诉前端主窗口是否在屏幕上
pub fn notify_main_visibility(app: &AppHandle, visible: bool) {
    let _ = app.emit_to(settings_window::LABEL, "main:visibility", visible);
}

fn build_tray(app: &mut App) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "settings", "设置", true, None::<&str>)?;
    let gestures =
        CheckMenuItem::with_id(app, "enabled", "启用手势", true, hook::is_enabled(), None::<&str>)?;
    let shortcuts_paused = CheckMenuItem::with_id(
        app,
        "shortcuts-paused",
        "暂停快捷键",
        true,
        app.state::<AppState>().shortcuts_paused(),
        None::<&str>,
    )?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &open,
            &gestures,
            &shortcuts_paused,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    app.manage(TrayToggles {
        gestures,
        shortcuts_paused,
    });
    TrayIconBuilder::with_id("tray")
        .icon(tray_icon())
        .tooltip("NextGestures")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "settings" => settings_window::show(app),
            "enabled" => set_enabled(app, !hook::is_enabled()),
            "shortcuts-paused" => toggle_shortcuts(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                settings_window::show(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// 托盘图标：与应用图标同源（icons/icon.svg 生成的 32x32）
fn tray_icon() -> Image<'static> {
    Image::from_bytes(include_bytes!("../icons/32x32.png")).expect("embedded tray icon is a valid PNG")
}
