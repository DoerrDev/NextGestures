use std::path::PathBuf;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use gesture_core::Config;
use gesture_platform::{hook, hotkey};

pub struct AppState {
    pub path: PathBuf,
    pub config: RwLock<Config>,
    /// 上次由本进程写入时的 mtime，用来区分外部编辑
    mtime: RwLock<Option<SystemTime>>,
    pub recording: AtomicBool,
}

impl AppState {
    pub fn load(path: PathBuf) -> Result<Self> {
        let config = Config::load(&path)?;
        let state = Self {
            mtime: RwLock::new(mtime_of(&path)),
            path,
            config: RwLock::new(config),
            recording: AtomicBool::new(false),
        };
        state.push_to_hook();
        Ok(state)
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
        hotkey::set(shortcut_bindings(&fresh, false)?, false)?;
        *config = fresh;
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
        hotkey::set(shortcut_bindings(&config, false)?, false)
    }

    /// 修改配置并落盘。会影响快捷键的改动先重新注册（严格模式），失败则整体不生效；
    /// 落盘失败时恢复原来的注册。
    pub fn update(&self, update: impl FnOnce(&mut Config) -> Result<()>) -> Result<()> {
        let mut config = self.config.write().unwrap();
        let mut next = config.clone();
        update(&mut next)?;
        let changed = hotkey_spec(&next) != hotkey_spec(&config);
        if changed {
            hotkey::set(shortcut_bindings(&next, true)?, true)?;
        }
        if let Err(error) = next.save(&self.path) {
            if changed {
                let restore =
                    shortcut_bindings(&config, false).and_then(|old| hotkey::set(old, false));
                if let Err(restore_error) = restore {
                    log::warn!("恢复快捷键失败: {restore_error:#}");
                }
            }
            return Err(error);
        }
        *self.mtime.write().unwrap() = mtime_of(&self.path);
        *config = next;
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
                .filter(|g| g.enabled && g.has_hotkey())
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
        for gesture in profile.gestures.iter().filter(|g| g.enabled && g.has_hotkey()) {
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
