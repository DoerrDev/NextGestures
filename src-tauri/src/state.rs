use std::path::PathBuf;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::SystemTime;

use anyhow::Result;
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
        let fresh = Config::load(&self.path)?;
        *self.config.write().unwrap() = fresh;
        *self.mtime.write().unwrap() = current;
        self.push_to_hook();
        Ok(true)
    }

    /// 把触发键掩码和距离阈值同步给钩子线程
    pub fn push_to_hook(&self) {
        let cfg = self.config.read().unwrap();
        if !self.recording.load(Relaxed) {
            hook::set_trigger_mask(cfg.trigger_mask());
            hook::set_region_mask(cfg.region_mask());
        }
        hook::set_move_threshold(cfg.settings.move_threshold);
        hotkey::set(&cfg.settings.toggle_hotkey);
    }
}

fn mtime_of(path: &std::path::Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}
