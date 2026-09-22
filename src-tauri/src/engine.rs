use std::sync::atomic::Ordering::Relaxed;

use crossbeam_channel::Receiver;
use gesture_core::{Action, FireMode, Point, Region, TriggerButton, WheelDir, matcher, recognizer};
use gesture_platform::hook::{self, HookEvent};
use gesture_platform::{input, window};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::overlay;
use crate::state::AppState;

#[derive(Serialize, Clone)]
struct TrailBegin {
    origin: [i32; 2],
    color: String,
    width: f32,
    show_trail: bool,
    glow_color: String,
    glow_blur: f32,
    miss_color: String,
    hint_font_size: f32,
    hint_color: String,
    hint_offset_x: i32,
    hint_offset_y: i32,
}

#[derive(Serialize, Clone)]
struct GestureHint {
    name: String,
    font_size: f32,
    color: String,
    offset_x: i32,
    offset_y: i32,
}

#[derive(Serialize, Clone)]
struct Recorded {
    trigger: Option<TriggerButton>,
    region: Option<Region>,
    wheel: Vec<WheelDir>,
    points: Vec<Point>,
    times: Vec<f32>,
    raw: Vec<[i32; 2]>,
}

/// 录制过程中实时回传给设置窗口
#[derive(Serialize, Clone)]
struct Live {
    trigger: Option<TriggerButton>,
    region: Option<Region>,
    wheel: Vec<WheelDir>,
    points: Vec<[i32; 2]>,
}

#[derive(Serialize, Clone)]
struct PickInfo {
    exe: String,
    path: String,
    title: String,
    icon: String,
}

impl From<window::WindowInfo> for PickInfo {
    fn from(w: window::WindowInfo) -> Self {
        PickInfo { exe: w.exe, path: w.path, title: w.title, icon: w.icon }
    }
}

#[derive(Serialize, Clone)]
struct Fired {
    profile_id: String,
    gesture_id: String,
    name: String,
    score: f32,
    exe: String,
}

struct Hit {
    profile_id: String,
    gesture_id: String,
    name: String,
    score: f32,
    fire_mode: FireMode,
    action: Action,
}

struct Session {
    button: Option<TriggerButton>,
    region: Option<Region>,
    wheel: Vec<WheelDir>,
    exe: String,
    raw: Vec<(i32, i32)>,
    fired: bool,
    shown_name: Option<String>,
}

pub fn spawn(app: AppHandle, rx: Receiver<HookEvent>) {
    std::thread::Builder::new()
        .name("gesture-engine".into())
        .spawn(move || run(app, rx))
        .expect("spawn gesture-engine thread");
}

fn run(app: AppHandle, rx: Receiver<HookEvent>) {
    let mut session: Option<Session> = None;
    while let Ok(ev) = rx.recv() {
        match ev {
            HookEvent::PassThroughClick { button } => input::click(button),
            HookEvent::Picked(info) => {
                let _ = app.emit("pick:result", info.map(PickInfo::from));
            }
            HookEvent::PickHover(info) => {
                let _ = app.emit("pick:hover", info.map(PickInfo::from));
            }
            HookEvent::Start { button, region, point } => {
                log::debug!("手势开始 {button:?} {region:?} @ {point:?}");
                let wheel = session.take().map(|s| s.wheel).unwrap_or_default();
                session = Some(Session {
                    button,
                    region,
                    wheel,
                    exe: window::foreground_exe(),
                    raw: vec![point],
                    fired: false,
                    shown_name: None,
                });
                begin_trail(&app);
                emit_live(&app, session.as_ref().unwrap());
            }
            HookEvent::Wheel { dir, region } => {
                let s = session.get_or_insert_with(|| Session {
                    button: None,
                    region,
                    wheel: vec![],
                    exe: window::foreground_exe(),
                    raw: vec![],
                    fired: false,
                    shown_name: None,
                });
                s.wheel.push(dir);
                emit_live(&app, s);
                if !recording(&app) && !s.fired {
                    on_wheel_partial(&app, s);
                }
            }
            HookEvent::Move { point } => {
                let Some(s) = session.as_mut() else { continue };
                s.raw.push(point);
                let _ = app.emit_to(overlay::LABEL, "trail:point", [point.0, point.1]);
                if recording(&app) {
                    let _ = app.emit("gesture:live-point", [point.0, point.1]);
                } else if !s.fired && s.raw.len() >= 6 && s.raw.len() % 3 == 0 {
                    on_partial(&app, s);
                }
            }
            HookEvent::End { button, region, points, times, wheel } => {
                let s = session.take();
                if recording(&app) {
                    let _ = app.emit_to(overlay::LABEL, "trail:end", ());
                    finish_recording(&app, button, region, &points, &times, wheel);
                    continue;
                }
                if s.as_ref().is_some_and(|s| s.fired) {
                    let _ = app.emit_to(overlay::LABEL, "trail:end", ());
                    continue;
                }
                let exe = s.as_ref().map(|s| s.exe.clone()).unwrap_or_else(window::foreground_exe);
                match lookup(&app, &exe, button, region, &wheel, &points) {
                    Some(hit) => {
                        let show_hint = s.as_ref().is_none_or(|s| {
                            s.shown_name.as_deref() != Some(hit.name.as_str())
                        });
                        fire(&app, &exe, hit, show_hint)
                    }
                    None => log::debug!(
                        "手势结束未命中 {button:?} {region:?} {wheel:?} 采样 {} 点 @ {} 最接近: {}",
                        points.len(),
                        exe,
                        nearest(&app, &exe, button, region, &wheel, &points)
                    ),
                }
                let _ = app.emit_to(overlay::LABEL, "trail:end", ());
            }
            HookEvent::Cancel => {
                let s = session.take();
                let _ = app.emit_to(overlay::LABEL, "trail:end", ());
                if s.is_none_or(|s| s.button.is_some()) {
                    stop_recording(&app);
                }
            }
        }
    }
}

fn begin_trail(app: &AppHandle) {
    let state = app.state::<AppState>();
    let mut style = {
        let cfg = state.config.read().unwrap();
        TrailBegin {
            origin: [0, 0],
            color: cfg.settings.trail_color.clone(),
            width: cfg.settings.trail_width,
            show_trail: cfg.settings.show_trail,
            glow_color: cfg.settings.glow_color.clone(),
            glow_blur: cfg.settings.glow_blur,
            miss_color: cfg.settings.miss_color.clone(),
            hint_font_size: cfg.settings.gesture_hint_font_size,
            hint_color: cfg.settings.gesture_hint_color.clone(),
            hint_offset_x: cfg.settings.gesture_hint_offset_x,
            hint_offset_y: cfg.settings.gesture_hint_offset_y,
        }
    };
    let _ = overlay::ensure(app);
    let o = overlay::origin(app);
    style.origin = [o.0, o.1];
    let _ = app.emit_to(overlay::LABEL, "trail:begin", style);
}

fn emit_live(app: &AppHandle, s: &Session) {
    if !recording(app) {
        return;
    }
    let _ = app.emit(
        "gesture:live",
        Live {
            trigger: s.button,
            region: s.region,
            wheel: s.wheel.clone(),
            points: s.raw.iter().map(|p| [p.0, p.1]).collect(),
        },
    );
}

fn recording(app: &AppHandle) -> bool {
    app.state::<AppState>().recording.load(Relaxed)
}

fn stop_recording(app: &AppHandle) {
    let state = app.state::<AppState>();
    if state.recording.swap(false, Relaxed) {
        state.push_to_hook();
    }
}

fn finish_recording(
    app: &AppHandle,
    button: Option<TriggerButton>,
    region: Option<Region>,
    points: &[(i32, i32)],
    times: &[u32],
    wheel: Vec<WheelDir>,
) {
    stop_recording(app);
    let raw: Vec<[i32; 2]> = points.iter().map(|p| [p.0, p.1]).collect();
    let pts = to_points(points);
    let stroke = if pts.is_empty() {
        Some((vec![], vec![]))
    } else {
        recognizer::normalize(&pts).map(|norm| {
            let times = recognizer::timeline(&pts, times);
            (norm, times)
        })
    };
    match stroke {
        Some((norm, times)) if !norm.is_empty() || !wheel.is_empty() => {
            let _ = app.emit(
                "gesture:recorded",
                Recorded { trigger: button, region, wheel, points: norm, times, raw },
            );
        }
        _ => {
            let _ = app.emit("gesture:record-failed", "轨迹太短，请重新录制");
        }
    }
}

/// 画的过程中实时提示命中的手势名；Immediate 模式直接执行
fn on_partial(app: &AppHandle, s: &mut Session) {
    let hit = lookup(app, &s.exe, s.button, s.region, &s.wheel, &s.raw);
    let name = hit.as_ref().map(|h| h.name.clone());
    if name != s.shown_name {
        s.shown_name = name.clone();
        let _ = app.emit_to(overlay::LABEL, "trail:match", name);
    }
    if let Some(hit) = hit
        && hit.fire_mode == FireMode::Immediate
    {
        s.fired = true;
        let exe = s.exe.clone();
        fire(app, &exe, hit, false);
    }
}

/// 滚轮手势的 Immediate 模式：攒到一格就立刻执行，不等滚轮停
fn on_wheel_partial(app: &AppHandle, s: &mut Session) {
    let Some(hit) = lookup(app, &s.exe, s.button, s.region, &s.wheel, &s.raw) else {
        return;
    };
    if hit.fire_mode != FireMode::Immediate {
        return;
    }
    s.fired = true;
    s.shown_name = Some(hit.name.clone());
    let exe = s.exe.clone();
    fire(app, &exe, hit, true);
}

fn to_input<'a>(
    button: Option<TriggerButton>,
    region: Option<Region>,
    wheel: &'a [WheelDir],
    norm: Option<&'a [Point]>,
) -> matcher::Input<'a> {
    matcher::Input { button, region, wheel, stroke: norm }
}

/// 有轨迹但归一化失败（太短）返回 None
fn norm_of(raw: &[(i32, i32)]) -> Option<Option<Vec<Point>>> {
    if raw.is_empty() {
        return Some(None);
    }
    recognizer::normalize(&to_points(raw)).map(Some)
}

fn lookup(
    app: &AppHandle,
    exe: &str,
    button: Option<TriggerButton>,
    region: Option<Region>,
    wheel: &[WheelDir],
    raw: &[(i32, i32)],
) -> Option<Hit> {
    let norm = norm_of(raw)?;
    let state = app.state::<AppState>();
    let cfg = state.config.read().unwrap();
    let m = matcher::best(&cfg, exe, to_input(button, region, wheel, norm.as_deref()))?;
    Some(Hit {
        profile_id: m.profile_id.to_string(),
        gesture_id: m.gesture.id.clone(),
        name: m.gesture.name.clone(),
        score: m.score,
        fire_mode: m.gesture.fire_mode,
        action: m.gesture.action.clone(),
    })
}

/// 未命中时给出最接近的候选，方便调阈值
fn nearest(
    app: &AppHandle,
    exe: &str,
    button: Option<TriggerButton>,
    region: Option<Region>,
    wheel: &[WheelDir],
    raw: &[(i32, i32)],
) -> String {
    let Some(norm) = norm_of(raw) else {
        return "轨迹无效".into();
    };
    let state = app.state::<AppState>();
    let cfg = state.config.read().unwrap();
    match matcher::best_above(&cfg, exe, to_input(button, region, wheel, norm.as_deref()), 0.0) {
        Some(m) => format!("{} ({:.3})", m.gesture.name, m.score),
        None => "无候选手势".into(),
    }
}

fn fire(app: &AppHandle, exe: &str, hit: Hit, show_hint: bool) {
    log::info!("命中手势 {} (score {:.3}) @ {}", hit.name, hit.score, exe);
    if show_hint {
        let state = app.state::<AppState>();
        let cfg = state.config.read().unwrap();
        let _ = app.emit_to(
            overlay::LABEL,
            "gesture:hint-show",
            GestureHint {
                name: hit.name.clone(),
                font_size: cfg.settings.gesture_hint_font_size,
                color: cfg.settings.gesture_hint_color.clone(),
                offset_x: cfg.settings.gesture_hint_offset_x,
                offset_y: cfg.settings.gesture_hint_offset_y,
            },
        );
    }
    let _ = app.emit_to(overlay::LABEL, "gesture:execute", ());
    let _ = app.emit(
        "gesture:fired",
        Fired {
            profile_id: hit.profile_id,
            gesture_id: hit.gesture_id,
            name: hit.name,
            score: hit.score,
            exe: exe.to_string(),
        },
    );
    crate::action::spawn(hit.action);
}

/// 在桌面 overlay 上弹一条短提示，样式沿用手势提示的字体/颜色/偏移
pub fn toast(app: &AppHandle, text: &str) {
    let _ = overlay::ensure(app);
    let state = app.state::<AppState>();
    let cfg = state.config.read().unwrap();
    let _ = app.emit_to(
        overlay::LABEL,
        "gesture:toast",
        GestureHint {
            name: text.to_string(),
            font_size: cfg.settings.gesture_hint_font_size,
            color: cfg.settings.gesture_hint_color.clone(),
            offset_x: cfg.settings.gesture_hint_offset_x,
            offset_y: cfg.settings.gesture_hint_offset_y,
        },
    );
}

fn to_points(raw: &[(i32, i32)]) -> Vec<Point> {
    raw.iter().map(|p| [p.0 as f32, p.1 as f32]).collect()
}

/// 配置文件热加载轮询
pub fn spawn_config_watcher(app: AppHandle) {
    std::thread::Builder::new()
        .name("config-watcher".into())
        .spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(1000));
                let state = app.state::<AppState>();
                match state.reload_if_changed() {
                    Ok(true) => {
                        log::info!("检测到配置变更，已重新加载");
                        let _ = app.emit("config:changed", ());
                    }
                    Ok(false) => {}
                    Err(e) => log::warn!("重新加载配置失败: {e:#}"),
                }
            }
        })
        .expect("spawn config-watcher thread");
}

pub fn start(app: &AppHandle) {
    let (tx, rx) = crossbeam_channel::bounded(1024);
    hook::start(tx);
    spawn(app.clone(), rx);
    spawn_config_watcher(app.clone());
}
