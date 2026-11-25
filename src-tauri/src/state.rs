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
        let fresh = Config::load(&self.path)?;
        hotkey::set(shortcut_bindings(&fresh, false)?, false)?;
        *config = fresh;
        *self.mtime.write().unwrap() = current;
        drop(config);
        self.push_to_hook();
        Ok(true)
    }

    /// 把触发键掩码和距离阈值同步给钩子线程
    pub fn push_to_hook(&self) {
        let cfg = self.config.read().unwrap();
        hook::set_blacklist(cfg.settings.blacklist.clone());
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

    pub fn update_shortcuts(&self, update: impl FnOnce(&mut Config)) -> Result<()> {
        let mut config = self.config.write().unwrap();
        let mut next = config.clone();
        update(&mut next);
        let changed = next.shortcuts != config.shortcuts
            || next.settings.toggle_hotkey != config.settings.toggle_hotkey;
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

fn shortcut_bindings(config: &Config, strict: bool) -> Result<Vec<hotkey::Binding>> {
    let mut bindings = Vec::new();
    let toggle = &config.settings.toggle_hotkey;
    let entries = (!toggle.is_empty())
        .then_some((toggle, true))
        .into_iter()
        .chain(
            config
                .shortcuts
                .iter()
                .map(|shortcut| (&shortcut.keys, false)),
        );
    for (keys, is_toggle) in entries {
        let label = keys.join(" + ");
        let parsed = (|| {
            let chord = hotkey::Chord::parse(keys)?;
            if !is_toggle && chord.is_unmodified() {
                bail!("请同时按住 Ctrl、Alt、Shift 或 Win 中的至少一个修饰键");
            }
            if bindings.iter().any(|(other, _)| *other == chord) {
                bail!("组合键已被 NextGestures 中的另一项绑定使用");
            }
            Ok(chord)
        })()
        .with_context(|| format!("快捷键 [{label}]"));
        match parsed {
            Ok(chord) => bindings.push((chord, label)),
            Err(error) if strict => bail!("{error:#}"),
            Err(error) => log::warn!("{error:#}"),
        }
    }
    Ok(bindings)
}

fn mtime_of(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}
