use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError};
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
    left_click: bool,
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
    left_click: bool,
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
    left_click: bool,
    exe: String,
    raw: Vec<(i32, i32)>,
    fired: bool,
    shown_name: Option<String>,
    /// 关闭轨迹显示时不往 overlay 推点
    show_trail: bool,
}

/// 两次向 overlay 推送轨迹点的最小间隔。
/// 鼠标回报率可达 1000Hz，每个点单独 emit 就是一次 WebView2 ExecuteScript；
/// overlay 每帧才重画一次，按 ~250Hz 批量推送观感不变
const TRAIL_FLUSH: Duration = Duration::from_millis(4);

/// 鼠标停在配置了停留手势的边角，到点执行一次
struct Hover {
    region: Region,
    exe: String,
    dwell_at: Instant,
}

/// 待推送给 overlay 的轨迹点。距上次推送已超过 TRAIL_FLUSH 时立即发出（低速移动零额外延迟），
/// 否则攒着等到点或下一个事件到来再一起发
struct TrailBuf {
    pts: Vec<[i32; 2]>,
    last: Instant,
}

impl TrailBuf {
    fn push(&mut self, app: &AppHandle, p: (i32, i32)) {
        self.pts.push([p.0, p.1]);
        if self.last.elapsed() >= TRAIL_FLUSH {
            self.flush(app);
        }
    }

    fn flush(&mut self, app: &AppHandle) {
        if self.pts.is_empty() {
            return;
        }
        let _ = app.emit_to(overlay::LABEL, "trail:points", &self.pts);
        self.pts.clear();
        self.last = Instant::now();
    }

    /// 有积压时，最迟何时必须发出
    fn deadline(&self) -> Option<Instant> {
        (!self.pts.is_empty()).then(|| self.last + TRAIL_FLUSH)
    }
}

pub fn spawn(app: AppHandle, rx: Receiver<HookEvent>) {
    std::thread::Builder::new()
        .name("gesture-engine".into())
        .spawn(move || run(app, rx))
        .expect("spawn gesture-engine thread");
}

fn run(app: AppHandle, rx: Receiver<HookEvent>) {
    let mut session: Option<Session> = None;
    let mut trail = TrailBuf { pts: Vec::new(), last: Instant::now() };
    let mut hover: Option<Hover> = None;
    loop {
        let deadline = match (trail.deadline(), hover.as_ref().map(|h| h.dwell_at)) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let ev = match deadline {
            Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                Ok(ev) => ev,
                Err(RecvTimeoutError::Timeout) => {
                    if trail.deadline().is_some_and(|t| t <= Instant::now()) {
                        trail.flush(&app);
                    }
                    if hover.as_ref().is_some_and(|h| h.dwell_at <= Instant::now()) {
                        // 停留一次只触发一次，离开边角再回来才会重新计时
                        fire_dwell(&app, hover.take().unwrap());
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            },
            None => match rx.recv() {
                Ok(ev) => ev,
                Err(_) => break,
            },
        };
        // 其它事件（命中提示、结束等）必须排在已画出的点之后到达 overlay
        if !matches!(ev, HookEvent::Move { .. }) {
            trail.flush(&app);
        }
        match ev {
            HookEvent::PassThroughClick { button } => input::click(button),
            HookEvent::PassThroughPress { button } => input::press(button),
            HookEvent::EdgeHover { region } => {
                hover = region.and_then(|r| enter_edge(&app, r));
            }
            HookEvent::EdgeClick { button, region } => {
                if recording(&app) {
                    finish_recording(&app, Some(button), Some(region), &[], &[], vec![], false);
                    continue;
                }
                let exe = window::foreground_exe();
                let hit = lookup(&app, &exe, Some(button), Some(region), &[], false, &[]);
                match hit {
                    Some(hit) => {
                        let _ = overlay::ensure(&app);
                        fire(&app, &exe, hit, true);
                    }
                    None => input::click(button),
                }
            }
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
                    left_click: false,
                    exe: window::foreground_exe(),
                    raw: vec![point],
                    fired: false,
                    shown_name: None,
                    show_trail: begin_trail(&app),
                });
                emit_live(&app, session.as_ref().unwrap());
            }
            HookEvent::Wheel { dir, region } => {
                let s = session.get_or_insert_with(|| Session {
                    button: None,
                    region,
                    wheel: vec![],
                    left_click: false,
                    exe: window::foreground_exe(),
                    raw: vec![],
                    fired: false,
                    shown_name: None,
                    show_trail: false, // 纯滚轮手势没有轨迹
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
                if s.show_trail {
                    trail.push(&app, point);
                }
                if recording(&app) {
                    // 只有设置窗口的录制面板关心，不必广播给 overlay
                    let _ = app.emit_to("main", "gesture:live-point", [point.0, point.1]);
                } else if !s.fired && s.raw.len() >= 6 && s.raw.len() % 3 == 0 {
                    on_partial(&app, s);
                }
            }
            HookEvent::LeftClick { point } => {
                let Some(s) = session.as_mut() else { continue };
                s.left_click = true;
                s.raw.push(point);
                if s.show_trail {
                    trail.push(&app, point);
                    trail.flush(&app);
                }
                emit_live(&app, s);
                if !recording(&app) && !s.fired {
                    on_partial(&app, s);
                }
            }
            HookEvent::End { button, region, points, times, wheel, left_click } => {
                let s = session.take();
                if recording(&app) {
                    let _ = app.emit_to(overlay::LABEL, "trail:end", ());
                    finish_recording(&app, button, region, &points, &times, wheel, left_click);
                    continue;
                }
                if s.as_ref().is_some_and(|s| s.fired) {
                    let _ = app.emit_to(overlay::LABEL, "trail:end", ());
                    continue;
                }
                let exe = s.as_ref().map(|s| s.exe.clone()).unwrap_or_else(window::foreground_exe);
                match lookup(&app, &exe, button, region, &wheel, left_click, &points) {
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
                        nearest(&app, &exe, button, region, &wheel, left_click, &points)
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

/// 返回本次手势是否显示轨迹
fn begin_trail(app: &AppHandle) -> bool {
    let state = app.state::<AppState>();
    let mut style = {
        let cfg = state.config.read().unwrap();
        if cfg.settings.quiet_mode {
            return false;
        }
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
    let show_trail = style.show_trail;
    let _ = app.emit_to(overlay::LABEL, "trail:begin", style);
    show_trail
}

fn show_match_hint(app: &AppHandle) -> bool {
    !app.state::<AppState>().config.read().unwrap().settings.quiet_mode
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
            left_click: s.left_click,
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
    left_click: bool,
) {
    stop_recording(app);
    // 按键手势只在边角单击时带区域，录制的轨迹 / 滚轮不限边角
    let edge_click = button.is_some() && region.is_some() && points.is_empty() && wheel.is_empty();
    let region = if button.is_some() && !edge_click { None } else { region };
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
        Some((norm, times)) if !norm.is_empty() || !wheel.is_empty() || edge_click => {
            let _ = app.emit(
                "gesture:recorded",
                Recorded { trigger: button, region, wheel, left_click, points: norm, times, raw },
            );
        }
        _ => {
            let _ = app.emit("gesture:record-failed", "轨迹太短，请重新录制");
        }
    }
}

/// 画的过程中实时提示命中的手势名；Immediate 模式直接执行
fn on_partial(app: &AppHandle, s: &mut Session) {
    let hit = lookup(app, &s.exe, s.button, s.region, &s.wheel, s.left_click, &s.raw);
    let name = hit.as_ref().map(|h| h.name.clone());
    if name != s.shown_name {
        s.shown_name = name.clone();
        if show_match_hint(app) {
            let _ = app.emit_to(overlay::LABEL, "trail:match", name);
        }
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
    let Some(hit) = lookup(app, &s.exe, s.button, s.region, &s.wheel, s.left_click, &s.raw) else {
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
    left_click: bool,
    norm: Option<&'a [Point]>,
) -> matcher::Input<'a> {
    matcher::Input { button, region, wheel, left_click, dwell: false, stroke: norm }
}

/// 鼠标进入边角：有停留手势才开始计时；全屏、暂停、黑名单、录制时不触发
fn enter_edge(app: &AppHandle, region: Region) -> Option<Hover> {
    if recording(app) || !hook::is_enabled() || window::foreground_fullscreen() {
        return None;
    }
    let exe = window::foreground_exe();
    let state = app.state::<AppState>();
    let cfg = state.config.read().unwrap();
    if cfg.settings.is_blacklisted(&exe) {
        return None;
    }
    let ms = dwell_gesture(&cfg, &exe, region)?.gesture.dwell_ms;
    Some(Hover { region, exe, dwell_at: Instant::now() + Duration::from_millis(ms as u64) })
}

fn dwell_gesture<'a>(cfg: &'a gesture_core::Config, exe: &str, region: Region) -> Option<matcher::Matched<'a>> {
    let input = matcher::Input {
        button: None,
        region: Some(region),
        wheel: &[],
        left_click: false,
        dwell: true,
        stroke: None,
    };
    matcher::best(cfg, exe, input)
}

fn fire_dwell(app: &AppHandle, h: Hover) {
    let hit = {
        let state = app.state::<AppState>();
        let cfg = state.config.read().unwrap();
        dwell_gesture(&cfg, &h.exe, h.region).map(|m| Hit {
            profile_id: m.profile_id.to_string(),
            gesture_id: m.gesture.id.clone(),
            name: m.gesture.name.clone(),
            score: m.score,
            fire_mode: FireMode::Immediate,
            action: m.gesture.action.clone(),
        })
    };
    if let Some(hit) = hit {
        let _ = overlay::ensure(app);
        fire(app, &h.exe, hit, true);
    }
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
    left_click: bool,
    raw: &[(i32, i32)],
) -> Option<Hit> {
    let norm = norm_of(raw)?;
    let state = app.state::<AppState>();
    let cfg = state.config.read().unwrap();
    let m = matcher::best(&cfg, exe, to_input(button, region, wheel, left_click, norm.as_deref()))?;
    Some(Hit {
        profile_id: m.profile_id.to_string(),
        gesture_id: m.gesture.id.clone(),
        name: m.gesture.name.clone(),
        score: m.score,
        // 右键组合存在时保留追加左键的机会，统一在右键松开时执行。
        fire_mode: if m.gesture.action.is_ai()
            || matches!(m.gesture.action, Action::Screenshot | Action::RecordRegion)
            || left_click
            || matcher::requires_release(&cfg, exe, button)
        {
            FireMode::OnRelease
        } else {
            m.gesture.fire_mode
        },
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
    left_click: bool,
    raw: &[(i32, i32)],
) -> String {
    let Some(norm) = norm_of(raw) else {
        return "轨迹无效".into();
    };
    let state = app.state::<AppState>();
    let cfg = state.config.read().unwrap();
    match matcher::best_above(&cfg, exe, to_input(button, region, wheel, left_click, norm.as_deref()), 0.0) {
        Some(m) => format!("{} ({:.3})", m.gesture.name, m.score),
        None => "无候选手势".into(),
    }
}

fn fire(app: &AppHandle, exe: &str, hit: Hit, show_hint: bool) {
    log::info!("命中手势 {} (score {:.3}) @ {}", hit.name, hit.score, exe);
    if show_hint && show_match_hint(app) {
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
    crate::action::spawn(app.clone(), hit.action);
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
