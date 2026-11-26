use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::recognizer::{self, Point};

const LEGACY_BUILTIN_PIN_ID: &str = "builtin-pin-screenshot";
/// 2：快捷键并入手势（Gesture.hotkey），黑名单并入配置组（Profile.passthrough）
const CONFIG_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriggerButton {
    Right,
    Middle,
    X1,
    X2,
}

impl TriggerButton {
    pub const ALL: [TriggerButton; 4] =
        [TriggerButton::Right, TriggerButton::Middle, TriggerButton::X1, TriggerButton::X2];

    pub fn bit(self) -> u8 {
        match self {
            TriggerButton::Right => 1,
            TriggerButton::Middle => 2,
            TriggerButton::X1 => 4,
            TriggerButton::X2 => 8,
        }
    }
}

/// 屏幕四角四边
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Region {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

impl Region {
    pub fn bit(self) -> u8 {
        1 << (self as u8)
    }

    /// 边角单击掩码里的位：每个区域占 4 位，对应 4 个触发键
    pub fn click_bit(self, button: TriggerButton) -> u32 {
        let idx = match button {
            TriggerButton::Right => 0,
            TriggerButton::Middle => 1,
            TriggerButton::X1 => 2,
            TriggerButton::X2 => 3,
        };
        1 << (self as u32 * 4 + idx)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WheelDir {
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FireMode {
    /// 松开触发键后执行
    OnRelease,
    /// 画到匹配上立刻执行
    Immediate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeySequenceEvent {
    pub vk: u16,
    pub scan: u16,
    pub extended: bool,
    pub down: bool,
    pub delay_ms: u32,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    /// 已移除的 lua 动作读旧配置时降级为 None
    #[serde(alias = "lua")]
    None,
    #[serde(alias = "pin_screenshot")]
    Screenshot,
    /// 贴图：截图中贴出选区；否则贴出剪贴板（图片或文字），剪贴板未变时依次恢复关闭过的贴图
    Pin,
    RecordRegion,
    /// 「我的动作」里的 AI 动作；旧版内置的翻译 / 润色 / 粤拼在读取配置时迁移为对应预设
    AiCustom { id: String },
    /// 通过 Windows AppsFolder 打开已登记的应用
    LaunchApp { app_id: String, name: String },
    /// 执行程序 / CMD 命令
    Command {
        program: String,
        #[serde(default)]
        args: Vec<String>,
        /// true 用 `cmd /c` 包一层并隐藏窗口
        #[serde(default)]
        shell: bool,
    },
    /// 组合快捷键，同时按下再逆序释放，例如 ["ctrl", "w"]
    Hotkey { keys: Vec<String> },
    /// 录制的按键事件，或旧版保存的多组组合键。
    KeySequence {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        steps: Vec<Vec<String>>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        events: Vec<KeySequenceEvent>,
    },
    /// 用默认浏览器打开链接，`{selection}` 会替换成当前选中的文字
    OpenUrl { url: String },
}

impl Default for Action {
    fn default() -> Self {
        Action::None
    }
}

impl Gesture {
    /// 边角 + 单击：在 region 内按下并松开触发键，不画轨迹也不滚轮
    pub fn is_edge_click(&self) -> bool {
        self.trigger.is_some() && self.region.is_some() && self.points.is_empty() && self.wheel.is_empty()
    }

    /// 边角 + 停留
    pub fn is_dwell(&self) -> bool {
        self.dwell_ms > 0
    }

    /// 能被鼠标输入触发：轨迹、滚轮、边角单击或边角停留
    pub fn has_mouse_trigger(&self) -> bool {
        !self.points.is_empty() || !self.wheel.is_empty() || self.is_edge_click()
            || (self.is_dwell() && self.region.is_some())
    }

    pub fn has_hotkey(&self) -> bool {
        self.hotkey.iter().any(|key| !key.trim().is_empty())
    }
}

impl Action {
    pub fn is_ai(&self) -> bool {
        matches!(self, Self::AiCustom { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Gesture {
    pub id: String,
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// 是否允许在列表里删除，新增弹窗默认勾选
    #[serde(default = "yes")]
    pub deletable: bool,
    /// None 表示不按键，仅靠边角触发
    #[serde(default)]
    pub trigger: Option<TriggerButton>,
    /// 手势开始时鼠标所在的屏幕边角；None 表示不限
    #[serde(default)]
    pub region: Option<Region>,
    /// 滚轮序列，可与轨迹组合，也可单独成为手势
    #[serde(default)]
    pub wheel: Vec<WheelDir>,
    /// 右键轨迹终点按下左键，松开右键后识别；旧配置默认关闭
    #[serde(default)]
    pub left_click: bool,
    /// 大于 0 表示边角停留手势：鼠标在 region 停留这么久（ms）后触发
    #[serde(default)]
    pub dwell_ms: u32,
    #[serde(default = "default_fire_mode")]
    pub fire_mode: FireMode,
    /// 归一化后的 64 个模板点；纯滚轮手势为空
    #[serde(default)]
    pub points: Vec<Point>,
    /// 与 points 对应的录制时间（ms，相对起点），用于回放动画；旧配置为空
    #[serde(default)]
    pub times: Vec<f32>,
    #[serde(default)]
    pub action: Action,
    /// 键盘快捷键，如 ["Ctrl","Alt","A"]；空表示没有快捷键
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hotkey: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub name: String,
    /// 空字符串表示 Global（所有程序生效）
    #[serde(default)]
    pub exe: String,
    #[serde(default)]
    pub gestures: Vec<Gesture>,
    /// true = 在这个程序里交给程序自带手势（原黑名单），NextGestures 完全不拦截
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub passthrough: bool,
}

impl Profile {
    pub fn is_global(&self) -> bool {
        self.exe.is_empty()
    }

    /// 专属于该进程的配置组（不含 Global）
    pub fn matches_exe(&self, exe: &str) -> bool {
        !self.is_global() && self.exe.trim().eq_ignore_ascii_case(exe.trim())
    }

    fn global() -> Self {
        Self { id: "global".into(), name: "Global".into(), exe: String::new(), gestures: vec![], passthrough: false }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub trigger_buttons: Vec<TriggerButton>,
    /// 进入手势态的最小移动距离（像素）
    pub move_threshold: i32,
    /// 相似度阈值 [0,1]
    pub match_threshold: f32,
    pub show_trail: bool,
    pub quiet_mode: bool,
    pub trail_color: String,
    pub trail_width: f32,
    /// 轨迹辉光颜色
    pub glow_color: String,
    /// 轨迹辉光扩散半径，0 表示关闭
    pub glow_blur: f32,
    /// 未匹配到手势时的轨迹颜色
    pub miss_color: String,
    /// 手势名称提示的字号（CSS 像素）
    pub gesture_hint_font_size: f32,
    /// 手势名称提示的文字颜色
    pub gesture_hint_color: String,
    /// 相对屏幕中心的水平偏移（CSS 像素）
    pub gesture_hint_offset_x: i32,
    /// 相对屏幕中心的垂直偏移（CSS 像素）
    pub gesture_hint_offset_y: i32,
    /// 触发提示（轨迹 / 手势名称）是否置顶显示在所有窗口之上
    pub gesture_hint_topmost: bool,
    /// 开关手势的全局快捷键，如 ["ctrl", "alt", "g"]；空表示未设置
    pub toggle_hotkey: Vec<String>,
    pub language: String,
    /// 统计 / 用户反馈后端地址
    pub api_base: String,
    /// 提交反馈时使用的昵称
    pub feedback_nickname: String,
    pub ai: AiSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            trigger_buttons: vec![TriggerButton::Right, TriggerButton::Middle],
            move_threshold: 20,
            match_threshold: 0.80,
            show_trail: true,
            quiet_mode: false,
            trail_color: "#4d4dff".into(),
            trail_width: 3.0,
            glow_color: "#00caf2".into(),
            glow_blur: 8.0,
            miss_color: "#a1a1a1".into(),
            gesture_hint_font_size: 18.0,
            gesture_hint_color: "#ffffff".into(),
            gesture_hint_offset_x: 0,
            gesture_hint_offset_y: 0,
            gesture_hint_topmost: true,
            toggle_hotkey: vec![],
            language: "zh-CN".into(),
            api_base: "https://software.sayhey.top".into(),
            feedback_nickname: String::new(),
            ai: AiSettings::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultMode { Bubble, Replace }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiAction {
    pub id: String,
    pub name: String,
    pub prompt: String,
    pub mode: ResultMode,
    /// lucide 图标名（PascalCase），空串表示使用默认图标
    #[serde(default)]
    pub icon: String,
    /// 解析回复里 `<notes>` 行之后的内容，单独作为说明卡片显示（替换时不粘贴）
    #[serde(default)]
    pub notes: bool,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiSettings {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub actions: Vec<AiAction>,
    /// 预设已写入过「我的动作」；之后用户删掉的预设不再自动补回
    pub presets_seeded: bool,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            actions: Vec::new(),
            presets_seeded: false,
        }
    }
}

impl std::fmt::Debug for AiSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiSettings")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("actions", &self.actions.len())
            .finish_non_exhaustive()
    }
}

/// 版本 1 的全局快捷键列表，读取时迁移为 Global 里只有快捷键的手势
#[derive(Debug, Clone, PartialEq, Deserialize)]
struct LegacyShortcut {
    keys: Vec<String>,
    action: Action,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "version")]
    pub version: u32,
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub profiles: Vec<Profile>,
}

impl Default for Config {
    fn default() -> Self {
        let mut cfg = Self {
            version: CONFIG_VERSION,
            settings: Settings::default(),
            profiles: vec![Profile::global()],
        };
        cfg.seed_ai_presets(true, &[]);
        cfg
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            let cfg = Config::default();
            cfg.save(path)?;
            return Ok(cfg);
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("读取配置失败: {}", path.display()))?;
        let (cfg, migrated) = Self::parse_migrating(&text)
            .with_context(|| format!("解析配置失败: {}", path.display()))?;
        // 一次性迁移后立即落盘，之后不再重复迁移
        if migrated {
            cfg.save(path)?;
        }
        Ok(cfg)
    }

    #[cfg(test)]
    fn parse(text: &str) -> Result<Self> {
        Ok(Self::parse_migrating(text)?.0)
    }

    /// 解析并迁移旧版字段；第二个返回值表示内容因迁移而变化，需要写回文件
    fn parse_migrating(text: &str) -> Result<(Self, bool)> {
        let mut config: toml::Table = toml::from_str(text)?;
        let (legacy_ai, polish_notes) = migrate_legacy_ai_actions(&mut config);
        let shortcuts: Vec<LegacyShortcut> = match config.remove("shortcuts") {
            Some(value) => value.try_into().context("旧版快捷键格式错误")?,
            None => vec![],
        };
        let blacklist: Vec<String> = config
            .get_mut("settings")
            .and_then(|settings| settings.as_table_mut()?.remove("blacklist"))
            .and_then(|value| value.try_into().ok())
            .unwrap_or_default();
        // Keep AI credentials from older configs while dropping obsolete plugin switches.
        if let Some(ai) = config
            .remove("plugins")
            .and_then(|mut plugins| plugins.as_table_mut()?.remove("ai"))
        {
            let settings = config
                .entry("settings")
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let Some(settings) = settings.as_table_mut() {
                settings.entry("ai").or_insert(ai);
            }
        }
        let mut cfg: Self = config.try_into()?;
        if !cfg.profiles.iter().any(|p| p.is_global()) {
            cfg.profiles.insert(0, Profile::global());
        }
        // Global 不能直通（手改配置时纠正）
        cfg.profiles.iter_mut().filter(|p| p.is_global()).for_each(|p| p.passthrough = false);
        cfg.migrate_legacy_builtins();
        let seeded = cfg.seed_ai_presets(polish_notes, &legacy_ai);
        let migrated = cfg.version < CONFIG_VERSION || !shortcuts.is_empty() || !blacklist.is_empty()
            || !legacy_ai.is_empty() || seeded;
        cfg.migrate_shortcuts(shortcuts);
        cfg.migrate_blacklist(blacklist);
        cfg.version = cfg.version.max(CONFIG_VERSION);
        Ok((cfg, migrated))
    }

    /// 首次运行把预设写入「我的动作」；`required` 是旧手势迁移后引用到的预设，缺了也要补上。
    /// 返回配置是否有变化
    fn seed_ai_presets(&mut self, polish_notes: bool, required: &[&str]) -> bool {
        let ai = &mut self.settings.ai;
        let mut changed = !ai.presets_seeded;
        for preset in crate::ai_presets::ai_presets(polish_notes) {
            let wanted = !ai.presets_seeded || required.contains(&preset.id.as_str());
            if wanted && !ai.actions.iter().any(|a| a.id == preset.id) {
                ai.actions.push(preset);
                changed = true;
            }
        }
        ai.presets_seeded = true;
        changed
    }

    /// 旧版全局快捷键 → Global 配置组里只有快捷键的手势
    fn migrate_shortcuts(&mut self, shortcuts: Vec<LegacyShortcut>) {
        let mut n = 0;
        for shortcut in shortcuts {
            let id = loop {
                n += 1;
                let id = format!("shortcut-{n}");
                if !self.profiles.iter().flat_map(|p| &p.gestures).any(|g| g.id == id) {
                    break id;
                }
            };
            let gesture = Gesture {
                id,
                name: describe_action(&shortcut.action),
                enabled: true,
                deletable: true,
                trigger: None,
                region: None,
                wheel: vec![],
                left_click: false,
                dwell_ms: 0,
                fire_mode: FireMode::OnRelease,
                points: vec![],
                times: vec![],
                action: shortcut.action,
                hotkey: shortcut.keys,
            };
            let global = self.profiles.iter_mut().find(|p| p.is_global()).expect("global profile exists");
            global.gestures.push(gesture);
        }
    }

    /// 旧版黑名单 → 配置组的「交给程序自带手势」
    fn migrate_blacklist(&mut self, names: Vec<String>) {
        let mut n = 0;
        for name in names {
            let exe = name.trim();
            if exe.is_empty() {
                continue;
            }
            if let Some(profile) = self.profiles.iter_mut().find(|p| p.matches_exe(exe)) {
                profile.passthrough = true;
                continue;
            }
            let id = loop {
                n += 1;
                let id = format!("passthrough-{n}");
                if !self.profiles.iter().any(|p| p.id == id) {
                    break id;
                }
            };
            self.profiles.push(Profile {
                id,
                name: exe.to_string(),
                exe: exe.to_string(),
                gestures: vec![],
                passthrough: true,
            });
        }
    }

    /// 前台程序属于「交给程序自带手势」的配置组时，NextGestures 完全不拦截
    pub fn is_passthrough(&self, exe: &str) -> bool {
        !exe.trim().is_empty() && self.profiles.iter().any(|p| p.passthrough && p.matches_exe(exe))
    }

    /// 交给钩子的直通进程名单
    pub fn passthrough_exes(&self) -> Vec<String> {
        self.profiles
            .iter()
            .filter(|p| p.passthrough && !p.is_global())
            .map(|p| p.exe.trim().to_ascii_lowercase())
            .collect()
    }

    fn migrate_legacy_builtins(&mut self) {
        let legacy_seeds = legacy_seed_gestures();
        for profile in &mut self.profiles {
            let is_global = profile.is_global();
            profile.gestures.retain(|gesture| {
                if gesture.id == LEGACY_BUILTIN_PIN_ID {
                    return false;
                }
                // Keep the legacy tutorial rows if the user customized them.
                !(is_global && legacy_seeds.iter().any(|seed| {
                    seed.id == gesture.id && *gesture == Gesture {
                        enabled: gesture.enabled,
                        ..seed.clone()
                    }
                }))
            });
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // 非 pretty 会把点集数组内联成一行，配置文件才有可读性
        let text = toml::to_string(self)?;
        std::fs::write(path, text)
            .with_context(|| format!("写入配置失败: {}", path.display()))?;
        Ok(())
    }

    pub fn trigger_mask(&self) -> u8 {
        self.settings.trigger_buttons.iter().fold(0, |m, b| m | b.bit())
    }

    fn active_gestures(&self) -> impl Iterator<Item = &Gesture> {
        self.profiles
            .iter()
            .flat_map(|p| &p.gestures)
            .filter(|g| g.enabled && g.has_mouse_trigger())
    }

    /// 边角滚轮手势用到的区域掩码，钩子只在这些区域接管滚轮
    pub fn region_mask(&self) -> u8 {
        self.active_gestures()
            .filter(|g| g.trigger.is_none() && !g.wheel.is_empty())
            .filter_map(|g| g.region)
            .fold(0, |m, r| m | r.bit())
    }

    /// 边角单击手势掩码（Region::click_bit），钩子只在这些区域接管对应按键的单击
    pub fn edge_click_mask(&self) -> u32 {
        self.active_gestures()
            .filter(|g| g.is_edge_click())
            .fold(0, |m, g| m | g.region.unwrap().click_bit(g.trigger.unwrap()))
    }

    /// 有边角停留手势时钩子才需要回报鼠标进出边角
    pub fn has_dwell(&self) -> bool {
        self.active_gestures().any(|g| g.is_dwell() && g.region.is_some())
    }

    pub fn profile_mut(&mut self, profile_id: &str) -> Option<&mut Profile> {
        self.profiles.iter_mut().find(|p| p.id == profile_id)
    }

    /// 覆盖式写入：存在同 id 则更新，否则追加
    pub fn upsert_gesture(&mut self, profile_id: &str, gesture: Gesture) -> Result<()> {
        if let Action::LaunchApp { app_id, .. } = &gesture.action {
            anyhow::ensure!(!app_id.trim().is_empty(), "请先选择要启动的程序");
        }
        anyhow::ensure!(
            !gesture.left_click || (gesture.trigger == Some(TriggerButton::Right) && !gesture.points.is_empty()),
            "终点左键点击需要右键轨迹"
        );
        if gesture.is_dwell() {
            anyhow::ensure!(
                gesture.region.is_some() && gesture.trigger.is_none() && gesture.points.is_empty()
                    && gesture.wheel.is_empty(),
                "停留手势只能单独配置在屏幕边角"
            );
            anyhow::ensure!((100..=10_000).contains(&gesture.dwell_ms), "停留时长需在 100 ~ 10000 毫秒之间");
        }
        anyhow::ensure!(gesture.has_mouse_trigger() || gesture.has_hotkey(), "请设置至少一种触发方式");
        let profile = self
            .profile_mut(profile_id)
            .with_context(|| format!("未找到配置组: {profile_id}"))?;
        match profile.gestures.iter_mut().find(|g| g.id == gesture.id) {
            Some(slot) => *slot = gesture,
            None => profile.gestures.push(gesture),
        }
        Ok(())
    }

    pub fn remove_gesture(&mut self, profile_id: &str, gesture_id: &str) -> Result<()> {
        let profile = self
            .profile_mut(profile_id)
            .with_context(|| format!("未找到配置组: {profile_id}"))?;
        let Some(idx) = profile.gestures.iter().position(|g| g.id == gesture_id) else {
            return Ok(());
        };
        if !profile.gestures[idx].deletable {
            anyhow::bail!("该手势不允许删除");
        }
        profile.gestures.remove(idx);
        Ok(())
    }
}

/// 旧版内置 AI 动作（ai_translate / ai_polish / ai_jyutping）→ 引用对应预设的 ai_custom。
/// 同时取出旧的 `show_polish_notes`。返回（被引用的预设 id，润色是否附带说明）
fn migrate_legacy_ai_actions(config: &mut toml::Table) -> (Vec<&'static str>, bool) {
    let polish_notes = config
        .get_mut("settings")
        .and_then(|s| s.as_table_mut()?.get_mut("ai")?.as_table_mut()?.remove("show_polish_notes"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let mut used = Vec::new();
    let mut rewrite = |action: &mut toml::Value| {
        let Some(action) = action.as_table_mut() else { return };
        let Some(id) = action.get("kind").and_then(|k| k.as_str()).and_then(crate::ai_presets::legacy_kind_to_preset)
        else {
            return;
        };
        action.insert("kind".into(), "ai_custom".into());
        action.insert("id".into(), id.into());
        if !used.contains(&id) {
            used.push(id);
        }
    };
    let gestures = config
        .get_mut("profiles")
        .and_then(|p| p.as_array_mut())
        .into_iter()
        .flatten()
        .filter_map(|p| p.get_mut("gestures")?.as_array_mut())
        .flatten();
    for gesture in gestures {
        if let Some(action) = gesture.get_mut("action") {
            rewrite(action);
        }
    }
    let shortcuts = config.get_mut("shortcuts").and_then(|s| s.as_array_mut()).into_iter().flatten();
    for shortcut in shortcuts {
        if let Some(action) = shortcut.get_mut("action") {
            rewrite(action);
        }
    }
    (used, polish_notes)
}

fn yes() -> bool {
    true
}

fn version() -> u32 {
    1
}

fn default_fire_mode() -> FireMode {
    FireMode::OnRelease
}

/// 迁移旧快捷键时用动作生成的手势名称
fn describe_action(action: &Action) -> String {
    fn key_label(key: &str) -> String {
        let mut chars = key.trim().chars();
        chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
    }
    match action {
        Action::None => "快捷键".into(),
        Action::Screenshot => "截图".into(),
        Action::Pin => "贴图".into(),
        Action::RecordRegion => "区域录屏".into(),
        Action::AiCustom { id } => match id.as_str() {
            crate::ai_presets::TRANSLATE_ID => "AI 翻译".into(),
            crate::ai_presets::POLISH_ID => "AI 润色".into(),
            crate::ai_presets::JYUTPING_ID => "粤拼".into(),
            _ => "AI 动作".into(),
        },
        Action::LaunchApp { name, .. } => format!("启动 {name}"),
        Action::Command { program, .. } => format!("运行命令 {program}"),
        Action::Hotkey { keys } => {
            format!("发送按键 {}", keys.iter().map(|k| key_label(k)).collect::<Vec<_>>().join("+"))
        }
        Action::KeySequence { .. } => "按键序列".into(),
        Action::OpenUrl { url } => format!("打开网址 {url}"),
    }
}

fn legacy_seed_gestures() -> [Gesture; 2] {
    [
        Gesture {
            id: "seed-e-notepad".into(),
            name: "画 e 打开记事本".into(),
            enabled: true,
            deletable: true,
            trigger: Some(TriggerButton::Right),
            region: None,
            wheel: vec![],
            left_click: false,
            dwell_ms: 0,
            fire_mode: FireMode::OnRelease,
            points: recognizer::normalize(&shape_e()).unwrap_or_default(),
            times: vec![],
            action: Action::Command { program: "notepad.exe".into(), args: vec![], shell: false },
            hotkey: vec![],
        },
        Gesture {
            id: "seed-o-close".into(),
            name: "画 O 关闭标签 (Ctrl+W)".into(),
            enabled: true,
            deletable: true,
            trigger: Some(TriggerButton::Middle),
            region: None,
            wheel: vec![],
            left_click: false,
            dwell_ms: 0,
            fire_mode: FireMode::Immediate,
            points: recognizer::normalize(&shape_o()).unwrap_or_default(),
            times: vec![],
            action: Action::Hotkey { keys: vec!["ctrl".into(), "w".into()] },
            hotkey: vec![],
        },
    ]
}

/// 顺时针整圈（屏幕坐标 y 向下）
fn shape_o() -> Vec<Point> {
    arc([0.0, 0.0], 100.0, 0.0, -360.0, 48)
}

/// 手写体 e：先向右画横杠，再逆时针绕一圈到右下收笔
fn shape_e() -> Vec<Point> {
    let mut pts = vec![[0.0, 60.0], [45.0, 60.0], [90.0, 55.0]];
    pts.extend(arc([45.0, 55.0], 45.0, 0.0, -300.0, 40));
    pts
}

fn arc(center: Point, r: f32, from_deg: f32, to_deg: f32, n: usize) -> Vec<Point> {
    (0..n)
        .map(|i| {
            let d = from_deg + (to_deg - from_deg) * i as f32 / (n - 1) as f32;
            let a = d.to_radians();
            [center[0] + r * a.cos(), center[1] + r * a.sin()]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hotkey_only(id: &str, keys: &[&str]) -> Gesture {
        Gesture {
            id: id.into(),
            name: id.into(),
            enabled: true,
            deletable: true,
            trigger: None,
            region: None,
            wheel: vec![],
            left_click: false,
            dwell_ms: 0,
            fire_mode: FireMode::OnRelease,
            points: vec![],
            times: vec![],
            action: Action::Screenshot,
            hotkey: keys.iter().map(|k| k.to_string()).collect(),
        }
    }

    #[test]
    fn legacy_shortcuts_migrate_to_hotkey_gestures_in_global() {
        let (cfg, migrated) = Config::parse_migrating(r#"
            version = 1
            [[shortcuts]]
            keys = ["ctrl", "alt", "s"]
            action = { kind = "screenshot" }
            [[shortcuts]]
            keys = ["ctrl", "shift", "w"]
            action = { kind = "hotkey", keys = ["ctrl", "w"] }
            [[shortcuts]]
            keys = ["alt", "u"]
            action = { kind = "open_url", url = "https://x.test/?q={selection}" }
            [[profiles]]
            id = "global"
            name = "Global"
            [[profiles.gestures]]
            id = "shortcut-1"
            name = "existing"
            wheel = ["Up"]
            action = { kind = "none" }
        "#).unwrap();
        assert!(migrated);
        assert_eq!(cfg.version, CONFIG_VERSION);
        let global = &cfg.profiles[0];
        let migrated: Vec<_> = global.gestures[1..].iter().collect();
        assert_eq!(migrated.iter().map(|g| g.id.as_str()).collect::<Vec<_>>(), ["shortcut-2", "shortcut-3", "shortcut-4"]);
        assert_eq!(migrated.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(),
            ["截图", "发送按键 Ctrl+W", "打开网址 https://x.test/?q={selection}"]);
        assert_eq!(migrated[0].hotkey, ["ctrl", "alt", "s"]);
        assert_eq!(migrated[1].action, Action::Hotkey { keys: vec!["ctrl".into(), "w".into()] });
        for g in &migrated {
            assert!(g.enabled && g.deletable && g.trigger.is_none() && g.region.is_none());
            assert!(g.points.is_empty() && g.wheel.is_empty() && g.fire_mode == FireMode::OnRelease);
            assert!(!g.has_mouse_trigger() && g.has_hotkey());
        }
        // 迁移后写回的文件不再含旧字段，再读不会重复迁移
        let text = toml::to_string(&cfg).unwrap();
        assert!(!text.contains("shortcuts"));
        let (again, migrated) = Config::parse_migrating(&text).unwrap();
        assert!(!migrated);
        assert_eq!(again.profiles[0].gestures, cfg.profiles[0].gestures);
    }

    #[test]
    fn legacy_blacklist_migrates_to_passthrough_profiles() {
        let (cfg, migrated) = Config::parse_migrating(r#"
            version = 1
            [settings]
            blacklist = ["photoshop.exe", " Chrome.EXE ", "chrome.exe", ""]
            [[profiles]]
            id = "global"
            name = "Global"
            [[profiles]]
            id = "ps"
            name = "Photoshop"
            exe = "PhotoShop.exe"
        "#).unwrap();
        assert!(migrated);
        assert_eq!(cfg.profiles.len(), 3);
        assert!(!cfg.profiles[0].passthrough);
        assert!(cfg.profiles[1].passthrough);
        let chrome = &cfg.profiles[2];
        assert_eq!((chrome.id.as_str(), chrome.name.as_str(), chrome.exe.as_str()),
            ("passthrough-1", "Chrome.EXE", "Chrome.EXE"));
        assert!(chrome.passthrough && chrome.gestures.is_empty());
        assert!(cfg.is_passthrough("photoshop.exe"));
        assert!(cfg.is_passthrough("CHROME.exe"));
        assert!(!cfg.is_passthrough("mychrome.exe"));
        assert!(!cfg.is_passthrough(""));
        assert_eq!(cfg.passthrough_exes(), ["photoshop.exe", "chrome.exe"]);
        let text = toml::to_string(&cfg).unwrap();
        assert!(!text.contains("blacklist"));
        let (again, migrated) = Config::parse_migrating(&text).unwrap();
        assert!(!migrated);
        assert_eq!(again.passthrough_exes(), cfg.passthrough_exes());
    }

    #[test]
    fn current_configs_without_legacy_fields_do_not_migrate() {
        let text = toml::to_string(&Config::default()).unwrap();
        assert!(!Config::parse_migrating(&text).unwrap().1);
        // 版本 1 的空配置仍会升级一次版本号
        assert!(Config::parse_migrating("version = 1").unwrap().1);
    }

    #[test]
    fn hotkey_and_passthrough_round_trip_and_are_omitted_when_unset() {
        let mut cfg = config_with_gestures();
        let text = toml::to_string(&cfg).unwrap();
        assert!(!text.contains("\nhotkey =") && !text.contains("passthrough"));
        cfg.profiles[0].gestures[0].hotkey = vec!["Ctrl".into(), "Alt".into(), "A".into()];
        cfg.profiles[0].gestures.push(hotkey_only("only-key", &["Ctrl", "B"]));
        cfg.profiles.push(Profile {
            id: "app".into(), name: "App".into(), exe: "app.exe".into(), gestures: vec![], passthrough: true,
        });
        let back: Config = toml::from_str(&toml::to_string(&cfg).unwrap()).unwrap();
        assert_eq!(back.profiles[0].gestures, cfg.profiles[0].gestures);
        assert!(back.profiles[1].passthrough);
        // 设置里不再有黑名单字段，前端发来的设置也不会带回它
        let settings: Settings = toml::from_str("blacklist = ['x.exe']").unwrap();
        assert!(!toml::to_string(&settings).unwrap().contains("blacklist"));
    }

    #[test]
    fn gesture_needs_a_mouse_or_keyboard_trigger() {
        let mut cfg = Config::default();
        let mut g = hotkey_only("k", &["Ctrl", "K"]);
        assert!(cfg.upsert_gesture("global", g.clone()).is_ok());
        g.hotkey = vec![" ".into()];
        let error = cfg.upsert_gesture("global", g.clone()).unwrap_err();
        assert_eq!(error.to_string(), "请设置至少一种触发方式");
        g.wheel = vec![WheelDir::Up];
        assert!(cfg.upsert_gesture("global", g.clone()).is_ok());
        g.wheel.clear();
        g.trigger = Some(TriggerButton::Right);
        g.region = Some(Region::Top);
        assert!(cfg.upsert_gesture("global", g).is_ok());
    }

    #[test]
    fn hotkey_only_gestures_do_not_touch_hook_masks() {
        let mut cfg = Config::default();
        let mut g = hotkey_only("k", &["Ctrl", "K"]);
        // 即便残留了按键或边角也不算鼠标手势
        g.trigger = Some(TriggerButton::Right);
        cfg.profiles[0].gestures.push(g);
        assert_eq!(cfg.region_mask(), 0);
        assert_eq!(cfg.edge_click_mask(), 0);
        assert!(!cfg.has_dwell());
        assert_eq!(cfg.trigger_mask(), Config::default().trigger_mask());
    }

    fn config_with_gestures() -> Config {
        let mut cfg = Config::default();
        cfg.profiles[0].gestures = legacy_seed_gestures().into();
        cfg
    }

    #[test]
    fn default_config_roundtrips_through_toml() {
        let cfg = Config::default();
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back = Config::parse(&text).unwrap();
        assert_eq!(back.profiles.len(), 1);
        assert!(back.profiles[0].gestures.is_empty());
        assert_eq!(back.settings.ai, cfg.settings.ai);
        assert!(back.settings.ai.presets_seeded);
        assert_eq!(back.settings.ai.actions.len(), 3);
        assert_eq!(back.settings.move_threshold, 20);
    }

    #[test]
    fn presets_are_seeded_once_and_stay_deleted() {
        let (old, migrated) = Config::parse_migrating("[settings.ai]\nmodel = 'custom'").unwrap();
        assert!(migrated);
        let ids: Vec<_> = old.settings.ai.actions.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, [crate::ai_presets::TRANSLATE_ID, crate::ai_presets::POLISH_ID, crate::ai_presets::JYUTPING_ID]);
        assert!(old.settings.ai.actions[1].notes);
        assert_eq!(old.settings.ai.model, "custom");

        let mut cfg = old;
        cfg.settings.ai.actions.retain(|a| a.id != crate::ai_presets::JYUTPING_ID);
        let (restored, migrated) = Config::parse_migrating(&toml::to_string(&cfg).unwrap()).unwrap();
        assert!(!migrated);
        assert_eq!(restored.settings.ai.actions.len(), 2);
    }

    #[test]
    fn legacy_builtin_ai_gestures_migrate_to_presets() {
        let old = r#"
version = 2
[settings.ai]
show_polish_notes = false
presets_seeded = true
[[settings.ai.actions]]
id = "mine"
name = "我的"
prompt = "{{selection}}"
mode = "bubble"
[[profiles]]
id = "global"
name = "Global"
exe = ""
[[profiles.gestures]]
id = "g1"
name = "润色"
points = []
times = []
hotkey = ["ctrl", "p"]
[profiles.gestures.action]
kind = "ai_polish"
"#;
        let (cfg, migrated) = Config::parse_migrating(old).unwrap();
        assert!(migrated);
        assert_eq!(cfg.profiles[0].gestures[0].action, Action::AiCustom { id: crate::ai_presets::POLISH_ID.into() });
        // 已经写入过预设：只补回被旧手势引用的润色，并沿用「关闭说明」的旧设置
        let ids: Vec<_> = cfg.settings.ai.actions.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["mine", crate::ai_presets::POLISH_ID]);
        let polish = &cfg.settings.ai.actions[1];
        assert!(!polish.notes);
        assert!(polish.prompt.contains("Do not include notes"));
        assert!(!toml::to_string(&cfg).unwrap().contains("show_polish_notes"));
    }

    #[test]
    fn endpoint_click_persists_and_old_configs_default_to_plain() {
        let mut cfg = config_with_gestures();
        cfg.profiles[0].gestures[0].left_click = true;
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert!(back.profiles[0].gestures[0].left_click);
        let old = text.lines().filter(|line| !line.starts_with("left_click ="))
            .collect::<Vec<_>>().join("\n");
        let back: Config = toml::from_str(&old).unwrap();
        assert!(back.profiles[0].gestures.iter().all(|g| !g.left_click));
    }

    #[test]
    fn endpoint_click_requires_right_stroke_when_saving() {
        let mut cfg = config_with_gestures();
        let mut g = cfg.profiles[0].gestures[0].clone();
        g.left_click = true;
        assert!(cfg.upsert_gesture("global", g.clone()).is_ok());
        g.trigger = Some(TriggerButton::Middle);
        assert!(cfg.upsert_gesture("global", g.clone()).is_err());
        g.trigger = Some(TriggerButton::Right);
        g.points.clear();
        assert!(cfg.upsert_gesture("global", g).is_err());
    }

    #[test]
    fn removed_lua_action_degrades_to_none() {
        let mut cfg = config_with_gestures();
        cfg.profiles[0].gestures[0].action = Action::OpenUrl { url: "x".into() };
        let text = toml::to_string(&cfg).unwrap()
            .replace("kind = \"open_url\"\nurl = \"x\"", "kind = \"lua\"\ncode = \"ng.sleep(1)\"");
        assert!(text.contains("kind = \"lua\""));
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.profiles[0].gestures[0].action, Action::None);
        assert_eq!(back.profiles[0].gestures.len(), 2);
    }

    #[test]
    fn trigger_mask_bits() {
        let cfg = Config::default();
        assert_eq!(cfg.trigger_mask(), TriggerButton::Right.bit() | TriggerButton::Middle.bit());
    }

    #[test]
    fn remove_respects_deletable_flag() {
        let mut cfg = config_with_gestures();
        cfg.profiles[0].gestures[0].deletable = false;
        assert!(cfg.remove_gesture("global", "seed-e-notepad").is_err());
        assert!(cfg.remove_gesture("global", "seed-o-close").is_ok());
        assert_eq!(cfg.profiles[0].gestures.len(), 1);
    }

    #[test]
    fn migration_removes_legacy_defaults_and_preserves_user_mappings() {
        let mut cfg = config_with_gestures();
        let gestures = &mut cfg.profiles[0].gestures;
        gestures[0].enabled = false;
        gestures[1].action = Action::Hotkey { keys: vec!["ctrl".into(), "q".into()] };
        let mut pin = gestures[0].clone();
        pin.id = LEGACY_BUILTIN_PIN_ID.into();
        pin.action = Action::Screenshot;
        gestures.push(pin.clone());
        pin.id = "my-pin".into();
        gestures.push(pin);
        let old_toml = toml::to_string(&cfg).unwrap().replace("kind = \"screenshot\"", "kind = \"pin_screenshot\"");
        let mut cfg: Config = toml::from_str(&old_toml).unwrap();
        cfg.migrate_legacy_builtins();
        cfg.migrate_legacy_builtins();
        let gestures = &cfg.profiles[0].gestures;
        assert_eq!(gestures.iter().map(|g| g.id.as_str()).collect::<Vec<_>>(), ["seed-o-close", "my-pin"]);
        assert!(matches!(gestures[1].action, Action::Screenshot));
    }

    #[test]
    fn legacy_plugin_settings_keep_ai_credentials_and_drop_switches() {
        let cfg = Config::parse(r#"
            [plugins]
            screenshot = false
            recorder = false
            quickformater = false
            [plugins.ai]
            base_url = "https://example.test/v1"
            api_key = "test-key"
            model = "custom"
        "#).unwrap();
        assert_eq!(cfg.settings.ai.base_url, "https://example.test/v1");
        assert_eq!(cfg.settings.ai.api_key, "test-key");
        assert_eq!(cfg.settings.ai.model, "custom");
        let text = toml::to_string(&cfg).unwrap();
        assert!(!text.contains("[plugins"));
        assert_eq!(Config::parse(&text).unwrap().settings.ai, cfg.settings.ai);
    }

    #[test]
    fn explicit_ai_settings_take_precedence_over_legacy_settings() {
        let cfg = Config::parse("[settings.ai]\n[plugins.ai]\napi_key = 'old-key'").unwrap();
        assert!(cfg.settings.ai.api_key.is_empty());
    }

    #[test]
    fn key_sequences_read_legacy_steps_and_round_trip_recorded_events() {
        let legacy: Action = toml::from_str("kind = 'key_sequence'\nsteps = [['ctrl', 'c']]").unwrap();
        assert_eq!(legacy, Action::KeySequence {
            steps: vec![vec!["ctrl".into(), "c".into()]], events: vec![],
        });

        let recorded = Action::KeySequence {
            steps: vec![],
            events: vec![
                KeySequenceEvent { vk: 65, scan: 30, extended: false, down: true, delay_ms: 0, label: "a".into() },
                KeySequenceEvent { vk: 65, scan: 30, extended: false, down: false, delay_ms: 125, label: "a".into() },
            ],
        };
        let mut config = config_with_gestures();
        config.profiles[0].gestures[0].action = recorded.clone();
        let serialized = toml::to_string_pretty(&config).unwrap();
        let restored: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(restored.profiles[0].gestures[0].action, recorded);
    }
}
