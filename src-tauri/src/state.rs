use std::path::PathBuf;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use gesture_core::Config;
use gesture_platform::{hook, hotkey};

pub struct AppState {
    pub path: PathBuf,
    pub config: RwLock<Config>,
    /// 手势配置换版时递增，读取方持有 config 的读锁。
    config_revision: AtomicU64,
    /// 上次由本进程写入时的 mtime，用来区分外部编辑
    mtime: RwLock<Option<SystemTime>>,
    pub recording: AtomicBool,
    shortcuts_paused: AtomicBool,
}

impl AppState {
    pub fn load(path: PathBuf) -> Result<Self> {
        let config = Config::load(&path)?;
        let state = Self {
            mtime: RwLock::new(mtime_of(&path)),
            path,
            config: RwLock::new(config),
            config_revision: AtomicU64::new(0),
            recording: AtomicBool::new(false),
            shortcuts_paused: AtomicBool::new(false),
        };
        state.push_to_hook();
        Ok(state)
    }

    pub fn config_revision(&self) -> u64 {
        self.config_revision.load(Relaxed)
    }

    pub fn save(&self) -> Result<()> {
        self.config.read().unwrap().save(&self.path)?;
        *self.mtime.write().unwrap() = mtime_of(&self.path);
        self.push_to_hook();
        Ok(())
    }

    /// 外部编辑了 toml 就重新加载
    pub fn reload_if_changed(&self) -> Result<bool> {
        let current = mtime_of(&self.path);
        if current == *self.mtime.read().unwrap() {
            return Ok(false);
        }
        let mut config = self.config.write().unwrap();
        if current != mtime_of(&self.path) || current == *self.mtime.read().unwrap() {
            return Ok(false);
        }
        // 旧版配置会在加载时迁移并写回，记录写回后的 mtime，免得下一轮又当成外部修改
        let fresh = Config::load(&self.path)?;
        self.sync_shortcuts(&fresh, false)?;
        *config = fresh;
        self.config_revision.fetch_add(1, Relaxed);
        *self.mtime.write().unwrap() = mtime_of(&self.path);
        drop(config);
        self.push_to_hook();
        Ok(true)
    }

    /// 把触发键掩码和距离阈值同步给钩子线程
    pub fn push_to_hook(&self) {
        let cfg = self.config.read().unwrap();
        hook::set_blacklist(cfg.passthrough_exes());
        if !self.recording.load(Relaxed) {
            hook::set_trigger_mask(cfg.trigger_mask());
            hook::set_region_mask(cfg.region_mask());
            hook::set_click_mask(cfg.edge_click_mask());
        }
        hook::set_hover(cfg.has_dwell());
        hook::set_move_threshold(cfg.settings.move_threshold);
    }

    pub fn init_shortcuts(&self) -> Result<()> {
        let config = self.config.read().unwrap();
        self.sync_shortcuts(&config, false)
    }

    pub fn shortcuts_paused(&self) -> bool {
        self.shortcuts_paused.load(Relaxed)
    }

    pub fn toggle_shortcuts_paused(&self) -> Result<bool> {
        // 和配置写入串行化，确保恢复时注册的是最新配置。
        let config = self.config.write().unwrap();
        let paused = !self.shortcuts_paused();
        if paused {
            hotkey::set(Vec::new(), false)?;
        } else {
            hotkey::set(shortcut_bindings(&config, false)?, false)?;
        }
        self.shortcuts_paused.store(paused, Relaxed);
        Ok(paused)
    }

    fn sync_shortcuts(&self, config: &Config, strict: bool) -> Result<()> {
        // 暂停期间仍校验组合键和重复项，系统占用情况留待恢复时检查。
        let bindings = shortcut_bindings(config, strict)?;
        if !self.shortcuts_paused() {
            hotkey::set(bindings, strict)?;
        }
        Ok(())
    }

    /// 修改配置并落盘。先校验快捷键，运行时则一并重新注册；落盘失败时恢复原来的注册。
    pub fn update(&self, update: impl FnOnce(&mut Config) -> Result<()>) -> Result<()> {
        let mut config = self.config.write().unwrap();
        let mut next = config.clone();
        update(&mut next)?;
        let changed = hotkey_spec(&next) != hotkey_spec(&config);
        if changed {
            self.sync_shortcuts(&next, true)?;
        }
        if let Err(error) = next.save(&self.path) {
            if changed {
                if let Err(restore_error) = self.sync_shortcuts(&config, false) {
                    log::warn!("恢复快捷键失败: {restore_error:#}");
                }
            }
            return Err(error);
        }
        *self.mtime.write().unwrap() = mtime_of(&self.path);
        *config = next;
        self.config_revision.fetch_add(1, Relaxed);
        drop(config);
        self.push_to_hook();
        Ok(())
    }
}

/// 决定快捷键注册结果的全部配置：总开关 + 各非直通配置组里启用手势的快捷键
fn hotkey_spec(config: &Config) -> (&[String], Vec<(&str, &[String])>) {
    let gestures = config
        .profiles
        .iter()
        .filter(|p| !p.passthrough)
        .flat_map(|p| {
            p.gestures
                .iter()
                .filter(|g| g.is_active() && g.has_hotkey())
                .map(|g| (p.id.as_str(), g.hotkey.as_slice()))
        })
        .collect();
    (&config.settings.toggle_hotkey, gestures)
}

/// 总开关 + 所有手势快捷键去重后的注册表。同一组合键可以出现在不同配置组里（按前台程序分派），
/// 但同一配置组内不能重复，也不能与总开关相同。
fn shortcut_bindings(config: &Config, strict: bool) -> Result<Vec<hotkey::Binding>> {
    let mut bindings: Vec<hotkey::Binding> = Vec::new();
    let mut toggle_chord = None;
    let toggle = &config.settings.toggle_hotkey;
    if !toggle.is_empty() {
        let label = toggle.join(" + ");
        match hotkey::Chord::parse(toggle).with_context(|| format!("总开关快捷键 [{label}]")) {
            Ok(chord) => {
                toggle_chord = Some(chord);
                bindings.push((chord, label));
            }
            Err(error) if strict => bail!("{error:#}"),
            Err(error) => log::warn!("{error:#}"),
        }
    }
    for profile in config.profiles.iter().filter(|p| !p.passthrough) {
        let mut seen = Vec::new();
        for gesture in profile.gestures.iter().filter(|g| g.is_active() && g.has_hotkey()) {
            let label = gesture.hotkey.join(" + ");
            let parsed = (|| {
                let chord = hotkey::Chord::parse(&gesture.hotkey)
                    .with_context(|| format!("快捷键 [{label}]"))?;
                if Some(chord) == toggle_chord {
                    bail!("快捷键 {label} 已被用作总开关快捷键，请换一个组合");
                }
                if seen.contains(&chord) {
                    bail!("同一配置组内已有手势使用 {label}");
                }
                Ok(chord)
            })();
            match parsed {
                Ok(chord) => {
                    seen.push(chord);
                    if !bindings.iter().any(|(other, _)| *other == chord) {
                        bindings.push((chord, label));
                    }
                }
                Err(error) if strict => bail!("{error:#}"),
                Err(error) => log::warn!("[{}] {}: {error:#}", profile.name, gesture.name),
            }
        }
    }
    Ok(bindings)
}

fn mtime_of(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        MOD_ALT, MOD_CONTROL, MOD_SHIFT, RegisterHotKey, UnregisterHotKey, VIRTUAL_KEY, VK_F21,
        VK_F22,
    };

    #[test]
    #[ignore = "checks temporary global hotkey registrations on the Windows desktop"]
    fn paused_shortcuts_release_keys_and_resume_with_latest_config() {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::load(dir.path().join("config.toml")).unwrap();
        let keys = |key: &str| ["ctrl", "alt", "shift", key].map(String::from).to_vec();
        let available = |key: VIRTUAL_KEY| unsafe {
            let result = RegisterHotKey(None, 1, MOD_CONTROL | MOD_ALT | MOD_SHIFT, key.0.into());
            if result.is_ok() {
                UnregisterHotKey(None, 1).unwrap();
            }
            result.is_ok()
        };
        assert!(available(VK_F21) && available(VK_F22));
        hotkey::start(|_| {});
        state.init_shortcuts().unwrap();
        state
            .update(|cfg| {
                cfg.settings.toggle_hotkey = keys("f21");
                Ok(())
            })
            .unwrap();
        assert!(!available(VK_F21));

        let gestures_enabled = hook::is_enabled();
        assert!(state.toggle_shortcuts_paused().unwrap());
        assert!(available(VK_F21));
        assert!(
            state
                .update(|cfg| {
                    cfg.settings.toggle_hotkey = vec!["ctrl".into()];
                    Ok(())
                })
                .is_err()
        );
        state
            .update(|cfg| {
                cfg.settings.toggle_hotkey = keys("f22");
                Ok(())
            })
            .unwrap();
        assert!(available(VK_F22));

        assert!(!state.toggle_shortcuts_paused().unwrap());
        assert!(available(VK_F21) && !available(VK_F22));
        assert_eq!(hook::is_enabled(), gestures_enabled);
        assert!(state.toggle_shortcuts_paused().unwrap());
    }
}
