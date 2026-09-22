use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::recognizer::{self, Point};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    None,
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
    /// 依次发送多组组合键
    KeySequence { steps: Vec<Vec<String>> },
    /// 用默认浏览器打开链接，`{selection}` 会替换成当前选中的文字
    OpenUrl { url: String },
    Lua { code: String },
}

impl Default for Action {
    fn default() -> Self {
        Action::None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
pub struct Settings {
    pub trigger_buttons: Vec<TriggerButton>,
    /// 进入手势态的最小移动距离（像素）
    pub move_threshold: i32,
    /// 相似度阈值 [0,1]
    pub match_threshold: f32,
    pub show_trail: bool,
    pub trail_color: String,
    pub trail_width: f32,
    /// 轨迹辉光颜色
    #[serde(default = "default_glow_color")]
    pub glow_color: String,
    /// 轨迹辉光扩散半径，0 表示关闭
    #[serde(default = "default_glow_blur")]
    pub glow_blur: f32,
    /// 未匹配到手势时的轨迹颜色
    #[serde(default = "default_miss_color")]
    pub miss_color: String,
    /// 手势名称提示的字号（CSS 像素）
    #[serde(default = "default_gesture_hint_font_size")]
    pub gesture_hint_font_size: f32,
    /// 手势名称提示的文字颜色
    #[serde(default = "default_gesture_hint_color")]
    pub gesture_hint_color: String,
    /// 相对屏幕中心的水平偏移（CSS 像素）
    #[serde(default)]
    pub gesture_hint_offset_x: i32,
    /// 相对屏幕中心的垂直偏移（CSS 像素）
    #[serde(default)]
    pub gesture_hint_offset_y: i32,
    /// 开关手势的全局快捷键，如 ["ctrl", "alt", "g"]；空表示未设置
    #[serde(default)]
    pub toggle_hotkey: Vec<String>,
    pub language: String,
    /// 统计 / 用户反馈后端地址
    #[serde(default = "default_api_base")]
    pub api_base: String,
    /// 提交反馈时使用的昵称
    #[serde(default)]
    pub feedback_nickname: String,
}

fn default_api_base() -> String {
    "https://software.sayhey.top".into()
}

fn default_glow_color() -> String {
    "#00caf2".into()
}

fn default_miss_color() -> String {
    "#a1a1a1".into()
}

fn default_glow_blur() -> f32 {
    8.0
}

fn default_gesture_hint_font_size() -> f32 {
    18.0
}

fn default_gesture_hint_color() -> String {
    "#ffffff".into()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            trigger_buttons: vec![TriggerButton::Right, TriggerButton::Middle],
            move_threshold: 20,
            match_threshold: 0.80,
            show_trail: true,
            trail_color: "#4d4dff".into(),
            trail_width: 3.0,
            glow_color: default_glow_color(),
            glow_blur: default_glow_blur(),
            miss_color: default_miss_color(),
            gesture_hint_font_size: default_gesture_hint_font_size(),
            gesture_hint_color: default_gesture_hint_color(),
            gesture_hint_offset_x: 0,
            gesture_hint_offset_y: 0,
            toggle_hotkey: vec![],
            language: "zh-CN".into(),
            api_base: default_api_base(),
            feedback_nickname: String::new(),
        }
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
        Self { version: version(), settings: Settings::default(), profiles: vec![seed_global()] }
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
        let mut cfg: Config = toml::from_str(&text)
            .with_context(|| format!("解析配置失败: {}", path.display()))?;
        if !cfg.profiles.iter().any(|p| p.is_global()) {
            cfg.profiles.insert(0, Profile { id: "global".into(), name: "Global".into(), exe: String::new(), gestures: vec![] });
        }
        Ok(cfg)
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

    /// 无按键边角手势用到的区域掩码，钩子只在这些区域进入无键手势态
    pub fn region_mask(&self) -> u8 {
        self.profiles
            .iter()
            .flat_map(|p| &p.gestures)
            .filter(|g| g.enabled && g.trigger.is_none())
            .filter_map(|g| g.region)
            .fold(0, |m, r| m | r.bit())
    }

    pub fn profile_mut(&mut self, profile_id: &str) -> Option<&mut Profile> {
        self.profiles.iter_mut().find(|p| p.id == profile_id)
    }

    /// 覆盖式写入：存在同 id 则更新，否则追加
    pub fn upsert_gesture(&mut self, profile_id: &str, gesture: Gesture) -> Result<()> {
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

fn seed_global() -> Profile {
    Profile {
        id: "global".into(),
        name: "Global".into(),
        exe: String::new(),
        gestures: vec![
            Gesture {
                id: "seed-e-notepad".into(),
                name: "画 e 打开记事本".into(),
                enabled: true,
                deletable: true,
                trigger: Some(TriggerButton::Right),
                region: None,
                wheel: vec![],
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
                fire_mode: FireMode::Immediate,
                points: recognizer::normalize(&shape_o()).unwrap_or_default(),
                times: vec![],
                action: Action::Hotkey { keys: vec!["ctrl".into(), "w".into()] },
            },
        ],
    }
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

    #[test]
    fn default_config_roundtrips_through_toml() {
        let cfg = Config::default();
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.profiles.len(), 1);
        assert_eq!(back.profiles[0].gestures.len(), 2);
        assert_eq!(back.profiles[0].gestures[0].points.len(), recognizer::NUM_POINTS);
        assert_eq!(back.settings.move_threshold, 20);
    }

    #[test]
    fn seeded_e_and_o_are_distinguishable() {
        let cfg = Config::default();
        let e = &cfg.profiles[0].gestures[0].points;
        let o = &cfg.profiles[0].gestures[1].points;
        assert!(recognizer::score(e, o) < cfg.settings.match_threshold);
    }

    #[test]
    fn trigger_mask_bits() {
        let cfg = Config::default();
        assert_eq!(cfg.trigger_mask(), TriggerButton::Right.bit() | TriggerButton::Middle.bit());
    }

    #[test]
    fn remove_respects_deletable_flag() {
        let mut cfg = Config::default();
        cfg.profiles[0].gestures[0].deletable = false;
        assert!(cfg.remove_gesture("global", "seed-e-notepad").is_err());
        assert!(cfg.remove_gesture("global", "seed-o-close").is_ok());
        assert_eq!(cfg.profiles[0].gestures.len(), 1);
    }
}
