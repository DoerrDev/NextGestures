use crate::config::{Config, Gesture, Profile, Region, TriggerButton, WheelDir};
use crate::recognizer::{self, Point};

#[derive(Debug)]
pub struct Matched<'a> {
    pub profile_id: &'a str,
    pub gesture: &'a Gesture,
    pub score: f32,
}

/// 一次手势输入的全部要素
#[derive(Debug, Clone, Copy)]
pub struct Input<'a> {
    pub button: Option<TriggerButton>,
    pub region: Option<Region>,
    pub wheel: &'a [WheelDir],
    pub left_click: bool,
    /// 边角停留：只匹配停留手势
    pub dwell: bool,
    /// 归一化后的轨迹；没有画轨迹时为 None
    pub stroke: Option<&'a [Point]>,
}

/// 进程专属手势优先于 Global；只有专属组里没有达到阈值的才回退到 Global。
pub fn best<'a>(cfg: &'a Config, exe: &str, input: Input<'_>) -> Option<Matched<'a>> {
    best_above(cfg, exe, input, cfg.settings.match_threshold)
}

/// 传 0.0 可以拿到不受阈值限制的最佳候选，用于调试和阈值调优
pub fn best_above<'a>(
    cfg: &'a Config,
    exe: &str,
    input: Input<'_>,
    threshold: f32,
) -> Option<Matched<'a>> {
    if cfg.is_passthrough(exe) {
        return None;
    }
    let app = cfg.profiles.iter().filter(|p| matches_exe(p, exe));
    let global = cfg.profiles.iter().filter(|p| p.is_global());

    best_of(app, input, threshold).or_else(|| best_of(global, input, threshold))
}

fn matches_exe(p: &Profile, exe: &str) -> bool {
    p.matches_exe(exe)
}

/// 快捷键对应的手势：进程专属配置组优先，其次 Global；直通程序里不响应。
/// `is_chord` 判断手势的按键是否就是按下的组合键（解析由平台层负责）。
pub fn hotkey_gesture<'a>(
    cfg: &'a Config,
    exe: &str,
    is_chord: impl Fn(&[String]) -> bool,
) -> Option<Matched<'a>> {
    if cfg.is_passthrough(exe) {
        return None;
    }
    let find = |global: bool| {
        cfg.profiles
            .iter()
            .filter(|p| if global { p.is_global() } else { matches_exe(p, exe) })
            .find_map(|p| {
                p.gestures
                    .iter()
                    .find(|g| g.enabled && g.has_hotkey() && is_chord(&g.hotkey))
                    .map(|g| Matched { profile_id: &p.id, gesture: g, score: 1.0 })
            })
    };
    find(false).or_else(|| find(true))
}

/// 在会话结束前，用户仍可把右键轨迹追加为左键组合，需延后立即执行。
pub fn requires_release(cfg: &Config, exe: &str, button: Option<TriggerButton>) -> bool {
    button == Some(TriggerButton::Right) && cfg.profiles.iter().any(|p| {
        (p.is_global() || matches_exe(p, exe)) && p.gestures.iter().any(|g| {
            g.enabled && g.trigger == Some(TriggerButton::Right) && g.left_click
        })
    })
}

/// 指定了边角的手势优先于不限边角的同轨迹手势
fn best_of<'a, I: Iterator<Item = &'a Profile>>(
    profiles: I,
    input: Input<'_>,
    threshold: f32,
) -> Option<Matched<'a>> {
    let mut best: Option<(bool, Matched<'a>)> = None;
    for p in profiles {
        for g in &p.gestures {
            // 只有快捷键的手势不参与鼠标匹配
            if !g.enabled || !g.has_mouse_trigger() || g.trigger != input.button || g.wheel != input.wheel
                || g.left_click != input.left_click || g.is_dwell() != input.dwell {
                continue;
            }
            if g.region.is_some() && g.region != input.region {
                continue;
            }
            let s = match input.stroke {
                Some(norm) if !g.points.is_empty() => recognizer::score(&g.points, norm),
                None if g.points.is_empty() => 1.0,
                _ => continue,
            };
            if s < threshold {
                continue;
            }
            let key = (g.region.is_some(), s);
            if best.as_ref().is_none_or(|(sp, b)| key > (*sp, b.score)) {
                best = Some((key.0, Matched { profile_id: &p.id, gesture: g, score: s }));
            }
        }
    }
    best.map(|(_, m)| m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Action, FireMode};

    fn gesture(id: &str, points: Vec<Point>) -> Gesture {
        Gesture {
            id: id.into(),
            name: id.into(),
            enabled: true,
            deletable: true,
            trigger: Some(TriggerButton::Right),
            region: None,
            wheel: vec![],
            left_click: false,
            dwell_ms: 0,
            fire_mode: FireMode::OnRelease,
            points,
            times: vec![],
            action: Action::None,
            hotkey: vec![],
        }
    }

    fn keys(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|k| k.to_string()).collect()
    }

    fn hotkey_only(id: &str, chord: &[&str]) -> Gesture {
        Gesture { trigger: None, hotkey: keys(chord), ..gesture(id, vec![]) }
    }

    fn input(button: TriggerButton, stroke: &[Point]) -> Input<'_> {
        Input { button: Some(button), region: None, wheel: &[], left_click: false, dwell: false, stroke: Some(stroke) }
    }

    fn line(from: Point, to: Point) -> Vec<Point> {
        (0..20)
            .map(|i| {
                let t = i as f32 / 19.0;
                [from[0] + t * (to[0] - from[0]), from[1] + t * (to[1] - from[1])]
            })
            .collect()
    }

    fn cfg_with_app() -> (Config, Vec<Point>) {
        let stroke = recognizer::normalize(&line([0.0, 0.0], [200.0, 0.0])).unwrap();
        let mut cfg = Config { profiles: vec![], ..Config::default() };
        cfg.profiles.push(Profile {
            id: "global".into(),
            name: "Global".into(),
            exe: String::new(),
            gestures: vec![gesture("g-right", stroke.clone())],
            passthrough: false,
        });
        cfg.profiles.push(Profile {
            id: "ps".into(),
            name: "Photoshop".into(),
            exe: "photoshop.exe".into(),
            gestures: vec![gesture("ps-right", stroke.clone())],
            passthrough: false,
        });
        (cfg, stroke)
    }

    #[test]
    fn app_profile_wins_over_global() {
        let (cfg, stroke) = cfg_with_app();
        let m = best(&cfg, "PhotoShop.EXE", input(TriggerButton::Right, &stroke)).unwrap();
        assert_eq!(m.gesture.id, "ps-right");
    }

    #[test]
    fn passthrough_overrides_app_and_global_gestures() {
        let (mut cfg, stroke) = cfg_with_app();
        cfg.profile_mut("ps").unwrap().exe = "PHOTOSHOP.EXE".into();
        cfg.profile_mut("ps").unwrap().passthrough = true;
        cfg.profiles.push(Profile {
            id: "ex".into(), name: "Explorer".into(), exe: "explorer.exe".into(), gestures: vec![], passthrough: true,
        });
        let input = input(TriggerButton::Right, &stroke);
        assert!(best(&cfg, "photoshop.exe", input).is_none());
        assert!(best_above(&cfg, "explorer.exe", input, 0.0).is_none());
        assert!(best(&cfg, "other.exe", input).is_some());
        cfg.profile_mut("ps").unwrap().passthrough = false;
        assert_eq!(best(&cfg, "photoshop.exe", input).unwrap().gesture.id, "ps-right");
    }

    #[test]
    fn hotkey_only_gestures_never_match_mouse_input() {
        let (mut cfg, _) = cfg_with_app();
        cfg.profiles[0].gestures = vec![hotkey_only("k", &["ctrl", "k"])];
        cfg.profiles[1].gestures.clear();
        let none = Input { button: None, region: None, wheel: &[], left_click: false, dwell: false, stroke: None };
        for input in [
            none,
            Input { button: Some(TriggerButton::Right), ..none },
            Input { region: Some(Region::Top), ..none },
            Input { dwell: true, region: Some(Region::Left), ..none },
        ] {
            assert!(best_above(&cfg, "x.exe", input, 0.0).is_none());
        }
        // 残留了按键也一样
        cfg.profiles[0].gestures[0].trigger = Some(TriggerButton::Right);
        assert!(best_above(&cfg, "x.exe", Input { button: Some(TriggerButton::Right), ..none }, 0.0).is_none());
        assert!(!requires_release(&cfg, "x.exe", Some(TriggerButton::Right)));
    }

    #[test]
    fn hotkeys_prefer_app_profile_then_global_and_skip_passthrough() {
        let (mut cfg, stroke) = cfg_with_app();
        let ctrl_k = |k: &[String]| k.iter().map(|s| s.to_ascii_lowercase()).eq(["ctrl", "k"].map(String::from));
        // 有轨迹的手势也可以带快捷键
        cfg.profile_mut("global").unwrap().gestures[0].hotkey = keys(&["Ctrl", "K"]);
        cfg.profile_mut("ps").unwrap().gestures.push(hotkey_only("ps-k", &["ctrl", "k"]));
        assert_eq!(hotkey_gesture(&cfg, "PhotoShop.exe", ctrl_k).unwrap().gesture.id, "ps-k");
        assert_eq!(hotkey_gesture(&cfg, "other.exe", ctrl_k).unwrap().gesture.id, "g-right");
        // 鼠标侧不受快捷键影响
        assert_eq!(best(&cfg, "other.exe", input(TriggerButton::Right, &stroke)).unwrap().gesture.id, "g-right");

        cfg.profile_mut("ps").unwrap().gestures[1].enabled = false;
        assert_eq!(hotkey_gesture(&cfg, "photoshop.exe", ctrl_k).unwrap().profile_id, "global");
        cfg.profile_mut("ps").unwrap().passthrough = true;
        assert!(hotkey_gesture(&cfg, "photoshop.exe", ctrl_k).is_none());
        assert!(hotkey_gesture(&cfg, "other.exe", |_: &[String]| false).is_none());
        cfg.profile_mut("global").unwrap().gestures[0].enabled = false;
        assert!(hotkey_gesture(&cfg, "other.exe", ctrl_k).is_none());
    }

    #[test]
    fn falls_back_to_global_for_other_exe() {
        let (cfg, stroke) = cfg_with_app();
        let m = best(&cfg, "explorer.exe", input(TriggerButton::Right, &stroke)).unwrap();
        assert_eq!(m.gesture.id, "g-right");
    }

    #[test]
    fn falls_back_to_global_when_app_gesture_disabled() {
        let (mut cfg, stroke) = cfg_with_app();
        cfg.profile_mut("ps").unwrap().gestures[0].enabled = false;
        let m = best(&cfg, "photoshop.exe", input(TriggerButton::Right, &stroke)).unwrap();
        assert_eq!(m.gesture.id, "g-right");
    }

    #[test]
    fn built_in_feature_actions_match_without_extra_settings() {
        let (mut cfg, stroke) = cfg_with_app();
        let input = input(TriggerButton::Right, &stroke);
        for action in [
            Action::Screenshot,
            Action::RecordRegion,
            Action::AiCustom { id: crate::ai_presets::TRANSLATE_ID.into() },
        ] {
            cfg.profile_mut("ps").unwrap().gestures[0].action = action;
            assert_eq!(best(&cfg, "photoshop.exe", input).unwrap().gesture.id, "ps-right");
        }
    }

    #[test]
    fn trigger_button_must_match() {
        let (cfg, stroke) = cfg_with_app();
        assert!(best(&cfg, "explorer.exe", input(TriggerButton::Middle, &stroke)).is_none());
    }

    #[test]
    fn endpoint_click_is_an_independent_gesture() {
        let (mut cfg, stroke) = cfg_with_app();
        let plain = input(TriggerButton::Right, &stroke);
        let clicked = Input { left_click: true, ..plain };
        assert!(best(&cfg, "photoshop.exe", clicked).is_none());
        let mut combo = gesture("clicked", stroke.clone());
        combo.left_click = true;
        cfg.profile_mut("global").unwrap().gestures.push(combo);
        assert_eq!(best(&cfg, "photoshop.exe", plain).unwrap().gesture.id, "ps-right");
        assert_eq!(best(&cfg, "photoshop.exe", clicked).unwrap().gesture.id, "clicked");
        cfg.profile_mut("global").unwrap().gestures.retain(|g| g.left_click);
        assert!(best(&cfg, "explorer.exe", plain).is_none());
    }

    #[test]
    fn only_applicable_enabled_combinations_delay_right_gestures() {
        let (mut cfg, _) = cfg_with_app();
        cfg.profile_mut("ps").unwrap().gestures[0].left_click = true;
        assert!(requires_release(&cfg, "PhotoShop.EXE", Some(TriggerButton::Right)));
        assert!(!requires_release(&cfg, "explorer.exe", Some(TriggerButton::Right)));
        assert!(!requires_release(&cfg, "photoshop.exe", Some(TriggerButton::Middle)));
        cfg.profile_mut("ps").unwrap().gestures[0].enabled = false;
        assert!(!requires_release(&cfg, "photoshop.exe", Some(TriggerButton::Right)));
        cfg.profile_mut("global").unwrap().gestures[0].left_click = true;
        assert!(requires_release(&cfg, "explorer.exe", Some(TriggerButton::Right)));
    }

    #[test]
    fn below_threshold_is_no_match() {
        let (cfg, _) = cfg_with_app();
        let down = recognizer::normalize(&line([0.0, 0.0], [0.0, 200.0])).unwrap();
        assert!(best(&cfg, "explorer.exe", input(TriggerButton::Right, &down)).is_none());
    }

    #[test]
    fn edge_click_and_dwell_match_only_their_own_input() {
        let (mut cfg, stroke) = cfg_with_app();
        let mut click = gesture("top-right-click", vec![]);
        click.region = Some(Region::Top);
        let mut dwell = gesture("left-dwell", vec![]);
        dwell.trigger = None;
        dwell.region = Some(Region::Left);
        dwell.dwell_ms = 600;
        cfg.profile_mut("global").unwrap().gestures.extend([click, dwell]);

        let tap = |region| Input {
            button: Some(TriggerButton::Right), region, wheel: &[], left_click: false, dwell: false, stroke: None,
        };
        assert_eq!(best(&cfg, "x.exe", tap(Some(Region::Top))).unwrap().gesture.id, "top-right-click");
        assert!(best(&cfg, "x.exe", tap(Some(Region::Bottom))).is_none());
        assert!(best(&cfg, "x.exe", tap(None)).is_none());
        // 从边角开始画轨迹仍匹配不限边角的普通手势
        let drawn = Input { region: Some(Region::Top), ..input(TriggerButton::Right, &stroke) };
        assert_eq!(best(&cfg, "x.exe", drawn).unwrap().gesture.id, "g-right");

        let stay = |region| Input { button: None, region, wheel: &[], left_click: false, dwell: true, stroke: None };
        assert_eq!(best(&cfg, "x.exe", stay(Some(Region::Left))).unwrap().gesture.id, "left-dwell");
        assert!(best(&cfg, "x.exe", stay(Some(Region::Right))).is_none());
        // 停留手势不会被同区域的无键滚轮输入命中
        let wheel = Input { dwell: false, wheel: &[WheelDir::Up], ..stay(Some(Region::Left)) };
        assert!(best(&cfg, "x.exe", wheel).is_none());
        assert_eq!(cfg.edge_click_mask(), Region::Top.click_bit(TriggerButton::Right));
        assert_eq!(cfg.region_mask(), 0);
        assert!(cfg.has_dwell());
    }

    #[test]
    fn region_specific_wins_and_wheel_must_match() {
        let (mut cfg, stroke) = cfg_with_app();
        let mut g = gesture("g-corner", stroke.clone());
        g.region = Some(Region::TopLeft);
        cfg.profile_mut("global").unwrap().gestures.push(g);
        let mut w = gesture("g-wheel", vec![]);
        w.wheel = vec![WheelDir::Up, WheelDir::Down];
        cfg.profile_mut("global").unwrap().gestures.push(w);

        let corner = Input { region: Some(Region::TopLeft), ..input(TriggerButton::Right, &stroke) };
        assert_eq!(best(&cfg, "explorer.exe", corner).unwrap().gesture.id, "g-corner");
        let plain = Input { region: Some(Region::Bottom), ..input(TriggerButton::Right, &stroke) };
        assert_eq!(best(&cfg, "explorer.exe", plain).unwrap().gesture.id, "g-right");

        let wheel = Input {
            button: Some(TriggerButton::Right),
            region: None,
            wheel: &[WheelDir::Up, WheelDir::Down],
            left_click: false,
            dwell: false,
            stroke: None,
        };
        assert_eq!(best(&cfg, "explorer.exe", wheel).unwrap().gesture.id, "g-wheel");
        let short = Input { wheel: &[WheelDir::Up], ..wheel };
        assert!(best(&cfg, "explorer.exe", short).is_none());
    }
}
