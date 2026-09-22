#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod action;
mod autostart;
mod commands;
mod engine;
mod feedback;
mod overlay;
mod state;

use tauri::image::Image;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{App, AppHandle, Emitter, Manager, Wry};

use gesture_platform::{hook, hotkey};
use state::AppState;

/// 托盘里的「启用手势」勾选项，供各处开关同步
struct TrayToggle(CheckMenuItem<Wry>);

/// 统一的手势总开关入口：钩子状态 + 托盘勾选 + 通知前端
pub fn set_enabled(app: &AppHandle, on: bool) {
    hook::set_enabled(on);
    if let Some(t) = app.try_state::<TrayToggle>() {
        let _ = t.0.set_checked(on);
    }
    let _ = app.emit("enabled:changed", on);
    engine::toast(app, if on { "手势已启用" } else { "手势已停用" });
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // 必须在 Builder 构建窗口之前注入，否则前端首帧 invoke 会拿不到 State
    let dir = std::path::PathBuf::from(std::env::var("APPDATA").expect("缺少 APPDATA"))
        .join("com.nextgestures.app");
    let state = AppState::load(dir.join("config.toml")).expect("加载配置失败");

    tauri::Builder::default()
        // 只允许一个后台实例运行；重复启动时唤醒首实例的设置窗口。
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_settings(app);
        }))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::config_path,
            commands::save_settings,
            commands::upsert_gesture,
            commands::remove_gesture,
            commands::set_gesture_enabled,
            commands::upsert_profile,
            commands::remove_profile,
            commands::start_recording,
            commands::cancel_recording,
            commands::begin_pick,
            commands::set_enabled,
            commands::is_enabled,
            commands::foreground_exe,
            commands::virtual_screen_size,
            commands::test_action,
            commands::show_gesture_hint_preview,
            commands::hide_gesture_hint_preview,
            commands::hide_settings,
            commands::is_autostart_enabled,
            commands::set_autostart_enabled,
            commands::feedback_chat,
            commands::feedback_submit,
            commands::feedback_ack_unread,
        ])
        .setup(|app| {
            size_main_window(app.handle());
            overlay::setup(app.handle())?;
            engine::start(app.handle());
            build_tray(app)?;
            app.manage(feedback::Backend::new(app.package_info().version.to_string()));
            feedback::start(app.handle());
            let handle = app.handle().clone();
            hotkey::start(move || set_enabled(&handle, !hook::is_enabled()));
            Ok(())
        })
        .on_window_event(|win, event| {
            // 主窗口关闭只是隐藏，程序常驻托盘
            if let tauri::WindowEvent::CloseRequested { api, .. } = event
                && win.label() == "main"
            {
                api.prevent_close();
                let _ = win.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("启动 NextGestures 失败");
}

/// 主窗口默认占当前显示器 65% 宽、75% 高
fn size_main_window(app: &AppHandle) {
    let Some(win) = app.get_webview_window("main") else { return };
    let Ok(Some(monitor)) = win.current_monitor() else { return };
    let scale = monitor.scale_factor();
    let screen = monitor.size().to_logical::<f64>(scale);
    let size = tauri::LogicalSize::new((screen.width * 0.65).max(880.0), (screen.height * 0.75).max(560.0));
    let _ = win.set_size(size);
    let _ = win.center();
}

fn show_settings(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn build_tray(app: &mut App) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "settings", "设置", true, None::<&str>)?;
    let toggle =
        CheckMenuItem::with_id(app, "enabled", "启用手势", true, hook::is_enabled(), None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu =
        Menu::with_items(app, &[&open, &toggle, &PredefinedMenuItem::separator(app)?, &quit])?;

    app.manage(TrayToggle(toggle));
    TrayIconBuilder::with_id("tray")
        .icon(tray_icon())
        .tooltip("NextGestures")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "settings" => show_settings(app),
            "enabled" => set_enabled(app, !hook::is_enabled()),
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
                show_settings(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// 32x32 蓝底白 "N"，省掉一个图标文件
fn tray_icon() -> Image<'static> {
    const N: i32 = 32;
    const STROKE: [(f32, f32); 4] = [(9.0, 23.0), (9.0, 9.0), (23.0, 23.0), (23.0, 9.0)];
    let mut rgba = vec![0u8; (N * N * 4) as usize];
    for y in 0..N {
        for x in 0..N {
            let i = ((y * N + x) * 4) as usize;
            let (fx, fy) = (x as f32, y as f32);
            if (fx - 15.5).powi(2) + (fy - 15.5).powi(2) > 15.5f32.powi(2) {
                continue;
            }
            let on_stroke = STROKE
                .windows(2)
                .any(|s| dist_to_segment((fx, fy), s[0], s[1]) < 1.8);
            let c: [u8; 4] =
                if on_stroke { [0xff, 0xff, 0xff, 0xff] } else { [0x2b, 0x6c, 0xb0, 0xff] };
            rgba[i..i + 4].copy_from_slice(&c);
        }
    }
    Image::new_owned(rgba, N as u32, N as u32)
}

fn dist_to_segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (vx, vy) = (b.0 - a.0, b.1 - a.1);
    let (wx, wy) = (p.0 - a.0, p.1 - a.1);
    let len2 = vx * vx + vy * vy;
    let t = if len2 <= f32::EPSILON { 0.0 } else { ((wx * vx + wy * vy) / len2).clamp(0.0, 1.0) };
    ((wx - t * vx).powi(2) + (wy - t * vy).powi(2)).sqrt()
}
