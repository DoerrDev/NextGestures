use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::recognizer::{self, Point};

const LEGACY_BUILTIN_PIN_ID: &str = "builtin-pin-screenshot";

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
    RecordRegion,
    AiJyutping,
    AiTranslate,
    AiPolish,
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
}

impl Action {
    pub fn is_ai(&self) -> bool {
        matches!(self, Self::AiJyutping | Self::AiTranslate | Self::AiPolish)
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
}

impl Profile {
    pub fn is_global(&self) -> bool {
        self.exe.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// 按进程名停用全部手势；旧配置默认为空。
    pub blacklist: Vec<String>,
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

impl Settings {
    pub fn set_blacklist(&mut self, names: Vec<String>) -> Result<()> {
        let mut normalized = Vec::new();
        for name in names {
            let name = name.trim().to_ascii_lowercase();
            anyhow::ensure!(
                name.ends_with(".exe") && name.len() > 4
                    && !name.chars().any(|c| c.is_control() || "\\/:*?\"<>|".contains(c)),
                "请输入进程文件名，例如 chrome.exe"
            );
            if !normalized.contains(&name) {
                normalized.push(name);
            }
        }
        self.blacklist = normalized;
        Ok(())
    }

    pub fn is_blacklisted(&self, exe: &str) -> bool {
        !exe.is_empty() && self.blacklist.iter().any(|name| name.trim().eq_ignore_ascii_case(exe))
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            blacklist: vec![],
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

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiSettings {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub show_polish_notes: bool,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            show_polish_notes: true,
        }
    }
}

impl std::fmt::Debug for AiSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AiSettings")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("show_polish_notes", &self.show_polish_notes)
            .finish_non_exhaustive()
    }
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
        Self {
            version: version(),
            settings: Settings::default(),
            profiles: vec![Profile {
                id: "global".into(),
                name: "Global".into(),
                exe: String::new(),
                gestures: vec![],
            }],
        }
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
        let mut cfg = Self::parse(&text)
            .with_context(|| format!("解析配置失败: {}", path.display()))?;
        if !cfg.profiles.iter().any(|p| p.is_global()) {
            cfg.profiles.insert(0, Profile { id: "global".into(), name: "Global".into(), exe: String::new(), gestures: vec![] });
        }
        cfg.migrate_legacy_builtins();
        Ok(cfg)
    }

    fn parse(text: &str) -> Result<Self> {
        let mut config: toml::Table = toml::from_str(text)?;
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
        Ok(config.try_into()?)
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
            .filter(|g| g.enabled)
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

fn yes() -> bool {
    true
}

fn version() -> u32 {
    1
}

fn default_fire_mode() -> FireMode {
    FireMode::OnRelease
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
        assert_eq!(back.settings.ai, AiSettings::default());
        assert_eq!(back.settings.move_threshold, 20);
    }

    #[test]
    fn polish_notes_default_on_and_can_be_disabled_persistently() {
        let old = Config::parse("[settings.ai]\nmodel = 'custom'").unwrap();
        assert!(old.settings.ai.show_polish_notes);
        let mut cfg = old;
        cfg.settings.ai.show_polish_notes = false;
        let restored = Config::parse(&toml::to_string(&cfg).unwrap()).unwrap();
        assert!(!restored.settings.ai.show_polish_notes);
        assert_eq!(restored.settings.ai.model, "custom");
    }

    #[test]
    fn blacklist_persists_and_old_settings_default_to_empty() {
        let mut settings = Settings::default();
        settings.blacklist = vec!["Chrome.EXE".into(), " msedge.exe ".into()];
        let text = toml::to_string_pretty(&settings).unwrap();
        let back: Settings = toml::from_str(&text).unwrap();
        assert!(back.is_blacklisted("chrome.exe"));
        assert!(back.is_blacklisted("MSEDGE.EXE"));
        assert!(!back.is_blacklisted("mychrome.exe"));
        assert!(!back.is_blacklisted(""));
        let mut old: toml::Value = toml::from_str(&text).unwrap();
        old.as_table_mut().unwrap().remove("blacklist");
        let back: Settings = old.try_into().unwrap();
        assert!(back.blacklist.is_empty());
    }

    #[test]
    fn blacklist_normalizes_and_rejects_paths_without_changing_previous_list() {
        let mut settings = Settings::default();
        settings.set_blacklist(vec![" Chrome.EXE ".into(), "chrome.exe".into(), "Some Browser.exe".into()]).unwrap();
        assert_eq!(settings.blacklist, vec!["chrome.exe", "some browser.exe"]);
        for name in ["C:\\chrome.exe", "chrome", "", ".exe", "chrome?.exe"] {
            assert!(settings.set_blacklist(vec![name.into()]).is_err());
            assert_eq!(settings.blacklist, vec!["chrome.exe", "some browser.exe"]);
        }
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
        assert_eq!(cfg.settings.ai, AiSettings::default());
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
