use tauri::{AppHandle, Manager, WebviewWindow};

/// 截图成功、失败或提前返回都会恢复之前可见的轨迹窗口。
struct RestoreOverlay(WebviewWindow);
impl Drop for RestoreOverlay {
    fn drop(&mut self) {
        let window = self.0.clone();
        let _ = self.0.run_on_main_thread(move || {
            let _ = window.show();
            let _ = crate::overlay::apply_z_order(window.app_handle());
        });
    }
}

pub fn spawn(app: AppHandle) {
    let owner = app.clone();
    // 在主线程完成隐藏后再截图，避开部分机器上捕获排除 API 导致的启动错误。
    if let Err(e) = app.run_on_main_thread(move || {
        let restore = if let Some(window) = owner.get_webview_window(crate::overlay::LABEL)
            && window.is_visible().unwrap_or(false)
        {
            if let Err(e) = window.hide() {
                log::error!("截图前隐藏轨迹层失败: {e}");
                crate::engine::toast(&owner, &format!("贴图失败：{e}"));
                return;
            }
            Some(RestoreOverlay(window))
        } else {
            None
        };
        std::thread::spawn(move || {
            // 等待隐藏提交到桌面合成器；冻结整屏后立即恢复轨迹层，截图继续独立运行。
            std::thread::sleep(std::time::Duration::from_millis(80));
            if let Err(e) = gesture_platform::pin::screenshot(move || drop(restore)) {
                log::error!("截图失败: {e:#}");
                crate::engine::toast(&owner, &format!("截图失败：{e}"));
            }
        });
    }) {
        log::error!("调度截图失败: {e}");
        crate::engine::toast(&app, &format!("截图失败：{e}"));
    }
}

/// 贴图键：贴出剪贴板或恢复关闭过的贴图；贴出的贴图在该线程上运行到关闭。
pub fn spawn_paste(app: AppHandle) {
    std::thread::spawn(move || {
        if let Err(e) = gesture_platform::pin::paste() {
            log::warn!("贴图失败: {e:#}");
            crate::engine::toast(&app, &format!("{e}"));
        }
    });
}
