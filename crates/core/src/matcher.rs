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
    let app = cfg.profiles.iter().filter(|p| matches_exe(p, exe));
    let global = cfg.profiles.iter().filter(|p| p.is_global());

    best_of(app, input, threshold).or_else(|| best_of(global, input, threshold))
}

fn matches_exe(p: &Profile, exe: &str) -> bool {
    !p.is_global() && p.exe.eq_ignore_ascii_case(exe)
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
            if !g.enabled || g.trigger != input.button || g.wheel != input.wheel {
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
            fire_mode: FireMode::OnRelease,
            points,
            times: vec![],
            action: Action::None,
        }
    }

    fn input(button: TriggerButton, stroke: &[Point]) -> Input<'_> {
        Input { button: Some(button), region: None, wheel: &[], stroke: Some(stroke) }
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
        let mut cfg = Config { version: 1, settings: Default::default(), profiles: vec![] };
        cfg.profiles.push(Profile {
            id: "global".into(),
            name: "Global".into(),
            exe: String::new(),
            gestures: vec![gesture("g-right", stroke.clone())],
        });
        cfg.profiles.push(Profile {
            id: "ps".into(),
            name: "Photoshop".into(),
            exe: "photoshop.exe".into(),
            gestures: vec![gesture("ps-right", stroke.clone())],
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
    fn trigger_button_must_match() {
        let (cfg, stroke) = cfg_with_app();
        assert!(best(&cfg, "explorer.exe", input(TriggerButton::Middle, &stroke)).is_none());
    }

    #[test]
    fn below_threshold_is_no_match() {
        let (cfg, _) = cfg_with_app();
        let down = recognizer::normalize(&line([0.0, 0.0], [0.0, 200.0])).unwrap();
        assert!(best(&cfg, "explorer.exe", input(TriggerButton::Right, &down)).is_none());
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
            stroke: None,
        };
        assert_eq!(best(&cfg, "explorer.exe", wheel).unwrap().gesture.id, "g-wheel");
        let short = Input { wheel: &[WheelDir::Up], ..wheel };
        assert!(best(&cfg, "explorer.exe", short).is_none());
    }
}
