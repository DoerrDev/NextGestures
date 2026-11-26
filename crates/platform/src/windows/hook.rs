use std::cell::{Cell, RefCell};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU8, AtomicU32, Ordering::Relaxed};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use gesture_core::{Region, TriggerButton, WheelDir};
use windows::Win32::Foundation::{LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTONULL, MONITORINFO,
    MonitorFromPoint,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, MSG, MSLLHOOKSTRUCT, PostThreadMessageW,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_MOUSE_LL, WM_APP,
    WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE,
    WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

/// 打在 dwExtraInfo 上，用来识别并放行我们自己合成的输入
pub const MAGIC: usize = 0x4E47_5354;

/// 贴边判定厚度（像素）：鼠标被屏幕边缘挡住时坐标正好落在最外一两个像素
const EDGE_SIZE: i32 = 3;
/// 角落沿两条边各延伸的长度（像素），贴着边滑进这段就算角
const CORNER_SIZE: i32 = 32;
/// 显示器矩形缓存有效期：鼠标移动频率很高，不必每次都查询显示器
const MONITOR_CACHE_MS: u64 = 1000;
/// 无按键手势：滚轮停这么久就算一次手势结束
const IDLE_MS: u64 = 400;
/// 会话进行中的空闲检测步长
const IDLE_TICK_MS: u64 = 50;
/// 一格滚轮的 delta，硬件/驱动可能拆成多条小 delta 发出来
const WHEEL_DELTA: i32 = 120;
const MAX_STROKE_POINTS: usize = 8192;
const MAX_STROKE_MS: u32 = 20_000;
/// 唤醒钩子线程做空闲检查的自定义消息。不能用 WM_TIMER：它是最低优先级消息，
/// 鼠标一动就会被钩子事件饿死，导致多次滚轮被并进同一个会话
const WM_IDLE_TICK: u32 = WM_APP + 1;

#[derive(Debug)]
pub enum HookEvent {
    /// 移动超过阈值，正式进入手势态
    Start { button: Option<TriggerButton>, region: Option<Region>, point: (i32, i32) },
    Move { point: (i32, i32) },
    Wheel { dir: WheelDir, region: Option<Region> },
    LeftClick { point: (i32, i32) },
    End {
        button: Option<TriggerButton>,
        region: Option<Region>,
        /// 没有画轨迹（纯滚轮）时为空
        points: Vec<(i32, i32)>,
        times: Vec<u32>,
        wheel: Vec<WheelDir>,
        left_click: bool,
    },
    Cancel,
    StrokeLimit,
    /// 候选态直接松手，需要补发一次真实点击
    PassThroughClick { button: TriggerButton },
    /// 鼠标进出屏幕边角（配置了停留手势时才回报）
    EdgeHover { region: Option<Region> },
    /// 在边角内单击了触发键；未命中手势时需补发真实点击
    EdgeClick { button: TriggerButton, region: Region },
    /// 只为边角单击接管的按键被拖动了，补发按下交还给系统
    PassThroughPress { button: TriggerButton },
    /// 准心拾取结果
    Picked(Option<super::window::WindowInfo>),
    /// 拾取拖动中，当前指向窗口变化
    PickHover(Option<super::window::WindowInfo>),
}

static TX: OnceLock<Sender<HookEvent>> = OnceLock::new();
static TRIGGER_MASK: AtomicU8 = AtomicU8::new(0);
static REGION_MASK: AtomicU8 = AtomicU8::new(0);
static CLICK_MASK: AtomicU32 = AtomicU32::new(0);
static HOVER: AtomicBool = AtomicBool::new(false);
static MOVE_THRESHOLD: AtomicI32 = AtomicI32::new(20);
static ENABLED: AtomicBool = AtomicBool::new(true);
static PICKING: AtomicBool = AtomicBool::new(false);
static WHEEL: AtomicI32 = AtomicI32::new(0);
static PICK_LAST_HWND: AtomicIsize = AtomicIsize::new(0);
static HOOK_TID: AtomicU32 = AtomicU32::new(0);
/// 有无按键手势会话在进行，只有这时才需要给钩子线程发空闲检查消息
static TICKING: AtomicBool = AtomicBool::new(false);

pub fn set_trigger_mask(mask: u8) {
    TRIGGER_MASK.store(mask, Relaxed);
}

/// 允许无按键触发的边角掩码（Region::bit）
pub fn set_region_mask(mask: u8) {
    REGION_MASK.store(mask, Relaxed);
}

/// 边角单击掩码（Region::click_bit）
pub fn set_click_mask(mask: u32) {
    CLICK_MASK.store(mask, Relaxed);
}

/// 是否回报鼠标进出边角（停留手势需要）
pub fn set_hover(on: bool) {
    HOVER.store(on, Relaxed);
}

pub fn set_move_threshold(px: i32) {
    MOVE_THRESHOLD.store(px.max(1), Relaxed);
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Relaxed);
}

pub fn set_blacklist(names: Vec<String>) {
    super::blacklist::replace(names);
}

pub fn is_enabled() -> bool {
    ENABLED.load(Relaxed)
}

/// 进入准心拾取模式：下一次左键松开时回报目标窗口的 exe
pub fn begin_pick() {
    PICKING.store(true, Relaxed);
    PICK_LAST_HWND.store(0, Relaxed);
    super::window::set_pick_cursor();
}

pub fn cancel_pick() {
    if PICKING.swap(false, Relaxed) {
        PICK_LAST_HWND.store(0, Relaxed);
        super::window::restore_cursor();
    }
}

pub fn take_wheel() -> i32 {
    WHEEL.swap(0, Relaxed)
}

/// 低级鼠标钩子必须跑在带消息循环的线程上
pub fn start(tx: Sender<HookEvent>) -> JoinHandle<()> {
    let _ = TX.set(tx);
    std::thread::Builder::new()
        .name("hook-idle-tick".into())
        .spawn(|| {
            loop {
                std::thread::sleep(Duration::from_millis(IDLE_TICK_MS));
                let tid = HOOK_TID.load(Relaxed);
                if tid != 0 && TICKING.load(Relaxed) {
                    let _ =
                        unsafe { PostThreadMessageW(tid, WM_IDLE_TICK, WPARAM(0), LPARAM(0)) };
                }
            }
        })
        .expect("spawn hook-idle-tick thread");
    std::thread::Builder::new()
        .name("mouse-hook".into())
        .spawn(|| unsafe {
            HOOK_TID.store(GetCurrentThreadId(), Relaxed);
            let handle = match SetWindowsHookExW(WH_MOUSE_LL, Some(hook_proc), None, 0) {
                Ok(h) => h,
                Err(e) => {
                    log::error!("安装鼠标钩子失败: {e}");
                    return;
                }
            };
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                if msg.message == WM_IDLE_TICK {
                    STATE.with(|s| on_idle(&mut s.borrow_mut()));
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let _ = UnhookWindowsHookEx(handle);
        })
        .expect("spawn mouse-hook thread")
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Phase {
    Idle,
    Candidate,
    Gesture,
}

struct HookState {
    /// 黑名单窗口中开始的按键序列始终原样放行，直到对应按键松开。
    bypass_button: Option<TriggerButton>,
    /// 超长手势取消后，继续吞掉尚未配对的触发键。
    canceled_button: Option<TriggerButton>,
    phase: Phase,
    /// None 表示无按键的边角手势
    button: Option<TriggerButton>,
    region: Option<Region>,
    start: (i32, i32),
    /// 是否已经移动超过阈值（画了轨迹）
    moved: bool,
    stroke_started: Option<u32>,
    points: Vec<(i32, i32)>,
    /// 与 points 一一对应的系统时间戳（ms）
    times: Vec<u32>,
    /// 折叠后的滚轮序列：同向连续滚动只算一格，方向反转才追加
    wheel: Vec<WheelDir>,
    /// 还没攒满一格的滚轮 delta
    wheel_acc: i32,
    /// 最后一次滚轮时间，用来判定"滚轮停了"
    last_wheel: Option<Instant>,
    left_click: bool,
    /// 吞掉按下后，即使会话已经结束也要吞掉对应的抬起。
    swallowed_left_down: bool,
    /// 鼠标当前所在的边角，变化时回报 EdgeHover
    hover: Option<Region>,
}

thread_local! {
    static STATE: RefCell<HookState> = const {
        RefCell::new(HookState {
            bypass_button: None,
            canceled_button: None,
            phase: Phase::Idle,
            button: None,
            region: None,
            start: (0, 0),
            moved: false,
            stroke_started: None,
            points: Vec::new(),
            times: Vec::new(),
            wheel: Vec::new(),
            wheel_acc: 0,
            last_wheel: None,
            left_click: false,
            swallowed_left_down: false,
            hover: None,
        })
    };
    /// 最近一次查询到的显示器矩形与查询时间
    static MONITOR: Cell<Option<(RECT, Instant)>> = const { Cell::new(None) };
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    let ms = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
    if ms.dwExtraInfo != MAGIC {
        let swallow = STATE.with(|s| handle(&mut s.borrow_mut(), wparam.0 as u32, ms));
        if swallow {
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn send(ev: HookEvent) {
    if let Some(tx) = TX.get() {
        // 给超长手势的取消消息保留一个位置，避免高频移动填满队列后留下一条旧轨迹。
        let has_room = tx.capacity().is_none_or(|cap| tx.len() < cap.saturating_sub(1));
        if matches!(ev, HookEvent::StrokeLimit) || has_room {
            let _ = tx.try_send(ev);
        }
    }
}

fn reset(st: &mut HookState) {
    st.phase = Phase::Idle;
    st.button = None;
    st.region = None;
    st.moved = false;
    st.stroke_started = None;
    st.points.clear();
    st.times.clear();
    st.wheel.clear();
    st.wheel_acc = 0;
    st.last_wheel = None;
    st.left_click = false;
    TICKING.store(false, Relaxed);
}

fn cancel_long_stroke(st: &mut HookState, trigger_released: bool) {
    let button = st.button;
    reset(st);
    if !trigger_released {
        st.canceled_button = button;
    }
    send(HookEvent::StrokeLimit);
}

fn begin(st: &mut HookState, button: Option<TriggerButton>, region: Option<Region>, pt: (i32, i32), time: u32) {
    reset(st);
    st.phase = Phase::Candidate;
    st.button = button;
    st.region = region;
    st.start = pt;
    st.points.push(pt);
    st.times.push(time);
}

fn finish(st: &mut HookState) {
    let points = if st.moved { std::mem::take(&mut st.points) } else { Vec::new() };
    let times = if st.moved { std::mem::take(&mut st.times) } else { Vec::new() };
    let wheel = std::mem::take(&mut st.wheel);
    let (button, region, left_click) = (st.button, st.region, st.left_click);
    reset(st);
    send(HookEvent::End { button, region, points, times, wheel, left_click });
}

/// 无按键手势：滚轮停止 IDLE_MS 就是会话边界
fn on_idle(st: &mut HookState) {
    if st.button.is_some() || st.phase == Phase::Idle {
        return;
    }
    let stopped = st.last_wheel.is_none_or(|t| t.elapsed() >= Duration::from_millis(IDLE_MS));
    if !stopped {
        return;
    }
    match st.phase {
        Phase::Gesture => finish(st),
        _ => reset(st),
    }
}

fn handle(st: &mut HookState, msg: u32, ms: &MSLLHOOKSTRUCT) -> bool {
    let pt = (ms.pt.x, ms.pt.y);
    handle_with_blacklist(st, msg, ms, || {
        // 贴图上的按键、滚轮交给原生窗口，右键关闭不会被全局手势吞掉。
        let native_input = msg == WM_MOUSEWHEEL || button_of(msg, ms).is_some_and(|(_, down)| down);
        if native_input && super::pin::is_at(pt) {
            return true;
        }
        // 普通滚动、未启用的触发键和总开关关闭时不需要查询目标窗口。
        ENABLED.load(Relaxed)
            && super::blacklist::active()
            && match button_of(msg, ms) {
                Some((button, true)) => {
                    TRIGGER_MASK.load(Relaxed) & button.bit() != 0 || CLICK_MASK.load(Relaxed) != 0
                }
                _ => msg == WM_MOUSEWHEEL && edge_region(pt).is_some(),
            }
            && super::blacklist::foreground()
    })
}

fn handle_with_blacklist(
    st: &mut HookState,
    msg: u32,
    ms: &MSLLHOOKSTRUCT,
    blacklisted: impl FnOnce() -> bool,
) -> bool {
    let pt = (ms.pt.x, ms.pt.y);

    if msg == WM_LBUTTONUP && st.swallowed_left_down {
        st.swallowed_left_down = false;
        return true;
    }

    let button_event = button_of(msg, ms);
    if let Some(button) = st.canceled_button {
        if let Some((current, down)) = button_event
            && current == button
        {
            if !down {
                st.canceled_button = None;
            }
            return true;
        }
        return false;
    }

    if st.stroke_started.is_some_and(|start| ms.time.wrapping_sub(start) >= MAX_STROKE_MS) {
        let trigger_released = button_event.is_some_and(|(button, down)| !down && Some(button) == st.button);
        cancel_long_stroke(st, trigger_released);
        return trigger_released;
    }

    if super::region::is_selecting() {
        return false;
    }

    if PICKING.load(Relaxed) {
        match msg {
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => return true,
            WM_MOUSEMOVE => {
                let hwnd = super::window::root_hwnd_at(pt);
                if hwnd != PICK_LAST_HWND.swap(hwnd, Relaxed) {
                    send(HookEvent::PickHover(super::window::window_info_from_point(pt)));
                }
                return false;
            }
            WM_LBUTTONUP => {
                PICKING.store(false, Relaxed);
                let info = super::window::window_info_from_point(pt);
                super::window::restore_cursor();
                PICK_LAST_HWND.store(0, Relaxed);
                send(HookEvent::Picked(info));
                return true;
            }
            _ => return false,
        }
    }

    if let Some(button) = st.bypass_button {
        if button_of(msg, ms) == Some((button, false)) {
            st.bypass_button = None;
        }
        return false;
    }

    // 在吞掉任何触发事件之前决定是否放行；整段原生手势保留给目标程序。
    if st.phase == Phase::Idle
        && (msg == WM_MOUSEWHEEL || button_of(msg, ms).is_some_and(|(_, down)| down))
        && blacklisted()
    {
        st.bypass_button = button_of(msg, ms).map(|(button, _)| button);
        return false;
    }

    if msg == WM_MOUSEWHEEL {
        if st.left_click {
            return true;
        }
        let delta = (ms.mouseData >> 16) as u16 as i16 as i32;
        WHEEL.fetch_add(delta / WHEEL_DELTA, Relaxed);
        if st.phase == Phase::Idle {
            if !ENABLED.load(Relaxed) {
                return false;
            }
            let Some(r) = edge_region(pt) else {
                return false;
            };
            begin(st, None, Some(r), pt, ms.time);
        }
        st.phase = Phase::Gesture;
        st.last_wheel = Some(Instant::now());
        TICKING.store(st.button.is_none(), Relaxed);
        // 攒满一格才算一次，再把同向连续的折叠掉：滚多久、几格都只算一格
        st.wheel_acc += delta;
        while st.wheel_acc.abs() >= WHEEL_DELTA {
            let dir = if st.wheel_acc > 0 { WheelDir::Up } else { WheelDir::Down };
            st.wheel_acc -= st.wheel_acc.signum() * WHEEL_DELTA;
            if st.wheel.last() != Some(&dir) {
                st.wheel.push(dir);
                send(HookEvent::Wheel { dir, region: st.region });
            }
        }
        return true;
    }

    if msg == WM_MOUSEMOVE {
        track_hover(st, pt);
        // 边角滚轮手势不记录轨迹
        if st.phase == Phase::Idle || st.button.is_none() || st.left_click {
            return false;
        }
        if !st.moved {
            let (dx, dy) = (pt.0 - st.start.0, pt.1 - st.start.1);
            let th = MOVE_THRESHOLD.load(Relaxed);
            if dx * dx + dy * dy >= th * th {
                // 这个键不是手势触发键，只是为边角单击接管的：拖动就原样交还
                if let Some(button) = st.button
                    && st.region.is_some()
                    && TRIGGER_MASK.load(Relaxed) & button.bit() == 0
                {
                    reset(st);
                    send(HookEvent::PassThroughPress { button });
                    return false;
                }
            } else {
                // 还在等待决定是否为点击，保持采样上限，松手仍按普通点击处理。
                if st.points.len() < MAX_STROKE_POINTS {
                    st.points.push(pt);
                    st.times.push(ms.time);
                }
                return false;
            }
        }
        if st.points.len() >= MAX_STROKE_POINTS {
            cancel_long_stroke(st, false);
            return false;
        }
        st.points.push(pt);
        st.times.push(ms.time);
        if !st.moved {
            st.moved = true;
            st.stroke_started = Some(ms.time);
            st.phase = Phase::Gesture;
            send(HookEvent::Start { button: st.button, region: st.region, point: st.start });
        }
        send(HookEvent::Move { point: pt });
        return false;
    }

    // 无按键手势遇到任何按键都让位，随后照常处理这次按键
    if st.phase != Phase::Idle
        && st.button.is_none()
        && matches!(
            msg,
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
        )
    {
        reset(st);
    }

    // 手势进行中屏蔽左键，避免误触下层程序
    if st.phase == Phase::Gesture
        && matches!(msg, WM_LBUTTONDOWN | WM_LBUTTONUP | WM_LBUTTONDBLCLK)
    {
        if matches!(msg, WM_LBUTTONDOWN | WM_LBUTTONDBLCLK) {
            st.swallowed_left_down = true;
            if st.button == Some(TriggerButton::Right) && st.moved && !st.left_click {
                if st.points.len() >= MAX_STROKE_POINTS {
                    cancel_long_stroke(st, false);
                    return true;
                }
                st.points.push(pt);
                st.times.push(ms.time);
                st.left_click = true;
                send(HookEvent::LeftClick { point: pt });
            }
        }
        return true;
    }

    let Some((btn, down)) = button_event else {
        return false;
    };

    if down {
        if st.phase != Phase::Idle {
            reset(st);
            send(HookEvent::Cancel);
            return true;
        }
        if !ENABLED.load(Relaxed) {
            return false;
        }
        let click_region = edge_click_region(pt, btn);
        if TRIGGER_MASK.load(Relaxed) & btn.bit() == 0 && click_region.is_none() {
            return false;
        }
        begin(st, Some(btn), click_region, pt, ms.time);
        return true;
    }

    if st.button != Some(btn) {
        return false;
    }
    match st.phase {
        Phase::Candidate => {
            let region = st.region;
            reset(st);
            send(match region {
                Some(region) => HookEvent::EdgeClick { button: btn, region },
                None => HookEvent::PassThroughClick { button: btn },
            });
            true
        }
        Phase::Gesture => {
            finish(st);
            true
        }
        Phase::Idle => false,
    }
}

fn button_of(msg: u32, ms: &MSLLHOOKSTRUCT) -> Option<(TriggerButton, bool)> {
    match msg {
        WM_RBUTTONDOWN => Some((TriggerButton::Right, true)),
        WM_RBUTTONUP => Some((TriggerButton::Right, false)),
        WM_MBUTTONDOWN => Some((TriggerButton::Middle, true)),
        WM_MBUTTONUP => Some((TriggerButton::Middle, false)),
        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            let btn = match (ms.mouseData >> 16) as u16 {
                1 => TriggerButton::X1,
                2 => TriggerButton::X2,
                _ => return None,
            };
            Some((btn, msg == WM_XBUTTONDOWN))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(button: TriggerButton) -> HookState {
        let mut st = HookState {
            bypass_button: None,
            canceled_button: None,
            phase: Phase::Idle, button: None, region: None, start: (0, 0),
            moved: false, stroke_started: None, points: vec![], times: vec![], wheel: vec![], wheel_acc: 0,
            last_wheel: None, left_click: false, swallowed_left_down: false, hover: None,
        };
        begin(&mut st, Some(button), None, (0, 0), 0);
        st
    }

    fn event(st: &mut HookState, msg: u32, x: i32) -> bool {
        handle(st, msg, &MSLLHOOKSTRUCT {
            pt: POINT { x, y: 0 }, time: x as u32,
            ..Default::default()
        })
    }

    #[test]
    fn stroke_point_limit_cancels_and_consumes_the_original_button_release() {
        let mut st = state(TriggerButton::Right);
        for i in 1..MAX_STROKE_POINTS {
            assert!(!event(&mut st, WM_MOUSEMOVE, 100 + i as i32));
        }
        assert_eq!(st.phase, Phase::Gesture);
        assert_eq!(st.points.len(), MAX_STROKE_POINTS);
        assert_eq!(st.times.len(), MAX_STROKE_POINTS);

        // Endpoint-click sampling also obeys the limit and still consumes its paired release.
        assert!(event(&mut st, WM_LBUTTONDOWN, 9000));
        assert_eq!(st.phase, Phase::Idle);
        assert!(st.points.is_empty() && st.times.is_empty());
        assert_eq!(st.canceled_button, Some(TriggerButton::Right));
        assert!(!event(&mut st, WM_MOUSEMOVE, 9001));
        assert!(!event(&mut st, WM_MBUTTONDOWN, 9001));
        assert!(!event(&mut st, WM_MBUTTONUP, 9001));
        assert!(event(&mut st, WM_LBUTTONUP, 9001));
        assert!(event(&mut st, WM_RBUTTONUP, 9001));
        assert!(!event(&mut st, WM_RBUTTONUP, 9001));
        assert!(st.canceled_button.is_none());
    }

    #[test]
    fn stroke_timeout_on_trigger_release_uses_wrapping_mouse_timestamp() {
        let mut st = state(TriggerButton::Right);
        let start = u32::MAX - 100;
        let msg = |time| MSLLHOOKSTRUCT { pt: POINT { x: 100, y: 0 }, time, ..Default::default() };
        assert!(!handle(&mut st, WM_MOUSEMOVE, &msg(start)));
        assert_eq!(st.stroke_started, Some(start));
        assert!(!handle(&mut st, WM_MOUSEMOVE, &msg(start.wrapping_add(MAX_STROKE_MS - 1))));
        assert_eq!(st.phase, Phase::Gesture);
        assert!(handle(&mut st, WM_RBUTTONUP, &msg(start.wrapping_add(MAX_STROKE_MS))));
        assert_eq!(st.phase, Phase::Idle);
        assert!(st.points.is_empty() && st.times.is_empty());
        assert!(st.canceled_button.is_none());
        assert!(!handle(&mut st, WM_RBUTTONUP, &msg(start.wrapping_add(MAX_STROKE_MS))));
    }

    #[test]
    fn elapsed_limit_starts_when_drawing_and_leaves_wheel_sequences_independent() {
        let mut st = state(TriggerButton::Right);
        assert!(!handle(&mut st, WM_MOUSEMOVE, &MSLLHOOKSTRUCT { time: MAX_STROKE_MS, ..Default::default() }));
        assert_eq!(st.phase, Phase::Candidate);
        assert!(event(&mut st, WM_RBUTTONUP, MAX_STROKE_MS as i32));
        assert!(st.canceled_button.is_none());

        begin(&mut st, None, Some(Region::Top), (0, 0), 0);
        let wheel = MSLLHOOKSTRUCT { mouseData: 120 << 16, time: MAX_STROKE_MS * 2, ..Default::default() };
        assert!(handle(&mut st, WM_MOUSEWHEEL, &wheel));
        assert_eq!(st.phase, Phase::Gesture);
        assert!(st.stroke_started.is_none());
        assert_eq!(st.wheel, [WheelDir::Up]);
        reset(&mut st);
    }

    #[test]
    #[ignore = "opens a real desktop pin window; run alone"]
    fn desktop_pin_right_button_bypasses_gesture_hook() {
        use windows::core::w;
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, HTCAPTION, IsWindowVisible, SendMessageW, WM_NCRBUTTONUP};
        let worker = std::thread::spawn(|| super::super::pin::open(super::super::pin::Bounds {
            x: 100, y: 100, width: 240, height: 160,
        }));
        let deadline = Instant::now() + Duration::from_secs(5);
        let hwnd = loop {
            if let Ok(hwnd) = unsafe { FindWindowW(w!("NextGesturesQuickPin"), None) } {
                if unsafe { IsWindowVisible(hwnd) }.as_bool() { break hwnd; }
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(25));
        };
        let previous = TRIGGER_MASK.swap(TriggerButton::Right.bit(), Relaxed);
        let mut st = state(TriggerButton::Right);
        reset(&mut st);
        let ms = MSLLHOOKSTRUCT { pt: POINT { x: 150, y: 150 }, ..Default::default() };
        let down = handle(&mut st, WM_RBUTTONDOWN, &ms);
        let bypass = st.bypass_button;
        let up = handle(&mut st, WM_RBUTTONUP, &ms);
        TRIGGER_MASK.store(previous, Relaxed);
        unsafe { SendMessageW(hwnd, WM_NCRBUTTONUP, Some(WPARAM(HTCAPTION as usize)), None); }
        worker.join().unwrap().unwrap();
        assert!(!down && !up);
        assert_eq!(bypass, Some(TriggerButton::Right));
        assert_eq!(st.phase, Phase::Idle);
        assert!(st.bypass_button.is_none());
        println!("Pin right down/up passed the gesture hook with Right trigger enabled");
    }

    #[test]
    fn blacklisted_buttons_pass_entire_native_sequence_even_after_focus_changes() {
        for (down, up, data) in [
            (WM_RBUTTONDOWN, WM_RBUTTONUP, 0),
            (WM_MBUTTONDOWN, WM_MBUTTONUP, 0),
            (WM_XBUTTONDOWN, WM_XBUTTONUP, 1 << 16),
            (WM_XBUTTONDOWN, WM_XBUTTONUP, 2 << 16),
        ] {
            let mut st = state(TriggerButton::Right);
            reset(&mut st);
            let ms = MSLLHOOKSTRUCT { mouseData: data, ..Default::default() };
            assert!(!handle_with_blacklist(&mut st, down, &ms, || true));
            assert!(st.bypass_button.is_some());
            for msg in [WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_LBUTTONDOWN, WM_LBUTTONUP, up] {
                assert!(!handle_with_blacklist(&mut st, msg, &ms, || panic!("sequence must stay native")));
                assert_eq!(st.phase, Phase::Idle);
            }
            assert!(st.bypass_button.is_none());
        }
    }

    #[test]
    fn blacklisted_corner_wheel_passes_without_starting_a_session() {
        let mut st = state(TriggerButton::Right);
        reset(&mut st);
        let ms = MSLLHOOKSTRUCT { mouseData: 120 << 16, ..Default::default() };
        for _ in 0..3 {
            assert!(!handle_with_blacklist(&mut st, WM_MOUSEWHEEL, &ms, || true));
            assert_eq!(st.phase, Phase::Idle);
            assert!(st.wheel.is_empty());
        }
    }

    #[test]
    fn endpoint_click_freezes_path_until_right_release() {
        let mut st = state(TriggerButton::Right);
        event(&mut st, WM_MOUSEMOVE, 100);
        assert!(event(&mut st, WM_LBUTTONDOWN, 110));
        assert!(st.left_click);
        assert_eq!(st.phase, Phase::Gesture);
        assert_eq!(st.points.last(), Some(&(110, 0)));
        let points = st.points.clone();
        let times = st.times.clone();
        event(&mut st, WM_MOUSEMOVE, 200);
        assert!(event(&mut st, WM_MOUSEWHEEL, 200));
        assert_eq!(st.points, points);
        assert_eq!(st.times, times);
        assert!(st.wheel.is_empty());
        assert!(event(&mut st, WM_LBUTTONUP, 200));
        assert_eq!(st.phase, Phase::Gesture);
        assert!(event(&mut st, WM_LBUTTONDOWN, 210));
        assert_eq!(st.points, points);
        assert!(event(&mut st, WM_RBUTTONUP, 210));
        assert_eq!(st.phase, Phase::Idle);
        assert!(!st.left_click);
        assert!(event(&mut st, WM_LBUTTONUP, 210));
        assert!(!st.swallowed_left_down);
        assert!(!event(&mut st, WM_LBUTTONDOWN, 210));
        assert!(!event(&mut st, WM_LBUTTONUP, 210));
    }

    #[test]
    fn cancellation_preserves_pending_left_release() {
        let mut st = state(TriggerButton::Right);
        event(&mut st, WM_MOUSEMOVE, 100);
        event(&mut st, WM_LBUTTONDOWN, 100);
        assert!(event(&mut st, WM_MBUTTONDOWN, 100));
        assert_eq!(st.phase, Phase::Idle);
        assert!(!st.left_click);
        assert!(event(&mut st, WM_LBUTTONUP, 100));
    }

    #[test]
    fn edges_are_thin_and_corners_extend_along_the_edges() {
        let r = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };
        let at = |x: i32, y: i32| {
            let (t, b, l, rt) = (y < 3, y >= 1077, x < 3, x >= 1917);
            classify(&r, (x, y), t, b, l, rt)
        };
        assert_eq!(at(960, 0), Some(Region::Top));
        assert_eq!(at(960, 2), Some(Region::Top));
        assert_eq!(at(960, 10), None);
        assert_eq!(at(10, 540), None);
        assert_eq!(at(0, 540), Some(Region::Left));
        assert_eq!(at(1919, 1079), Some(Region::BottomRight));
        assert_eq!(at(20, 0), Some(Region::TopLeft));
        assert_eq!(at(0, 20), Some(Region::TopLeft));
        assert_eq!(at(40, 0), Some(Region::Top));
        assert_eq!(at(1919, 20), Some(Region::TopRight));
    }

    #[test]
    fn edge_click_release_and_drag_hand_back_the_button() {
        // 只为边角单击接管的侧键：原地松开算边角单击并吞掉
        let up = MSLLHOOKSTRUCT { mouseData: 2 << 16, ..Default::default() };
        let mut st = state(TriggerButton::X2);
        st.region = Some(Region::Top);
        assert!(handle_with_blacklist(&mut st, WM_XBUTTONUP, &up, || false));
        assert_eq!(st.phase, Phase::Idle);
        // 拖动超过阈值：交还系统，后续抬起原样放行
        let mut st = state(TriggerButton::X2);
        st.region = Some(Region::Top);
        assert!(!event(&mut st, WM_MOUSEMOVE, 100));
        assert_eq!(st.phase, Phase::Idle);
        assert!(!handle_with_blacklist(&mut st, WM_XBUTTONUP, &up, || false));
    }

    #[test]
    fn click_requires_a_right_button_stroke() {
        let mut st = state(TriggerButton::Right);
        assert!(!event(&mut st, WM_LBUTTONDOWN, 0));
        assert!(!st.left_click);
        let mut st = state(TriggerButton::Middle);
        event(&mut st, WM_MOUSEMOVE, 100);
        assert!(event(&mut st, WM_LBUTTONDOWN, 100));
        assert!(!st.left_click);
        assert!(event(&mut st, WM_MBUTTONUP, 100));
        assert!(event(&mut st, WM_LBUTTONUP, 100));
    }
}

/// 允许无按键触发的边角；全屏程序里不接管
fn edge_region(pt: (i32, i32)) -> Option<Region> {
    region_at(pt)
        .filter(|r| REGION_MASK.load(Relaxed) & r.bit() != 0)
        .filter(|_| !super::window::foreground_fullscreen())
}

/// 配置了「边角 + 单击 btn」的边角；全屏程序里不接管
fn edge_click_region(pt: (i32, i32), btn: TriggerButton) -> Option<Region> {
    let mask = CLICK_MASK.load(Relaxed);
    if mask == 0 {
        return None;
    }
    region_at(pt)
        .filter(|r| mask & r.click_bit(btn) != 0)
        .filter(|_| !super::window::foreground_fullscreen())
}

/// 鼠标进出边角时回报；按键手势进行中不提示
fn track_hover(st: &mut HookState, pt: (i32, i32)) {
    let want = HOVER.load(Relaxed) && ENABLED.load(Relaxed) && st.button.is_none();
    let region = if want { region_at(pt) } else { None };
    if region != st.hover {
        st.hover = region;
        send(HookEvent::EdgeHover { region });
    }
}

/// 鼠标所在显示器矩形，短时间内复用缓存
fn monitor_rect(pt: (i32, i32)) -> Option<RECT> {
    let inside = |r: &RECT| pt.0 >= r.left && pt.0 < r.right && pt.1 >= r.top && pt.1 < r.bottom;
    if let Some((r, at)) = MONITOR.get()
        && inside(&r)
        && at.elapsed() < Duration::from_millis(MONITOR_CACHE_MS)
    {
        return Some(r);
    }
    let p = POINT { x: pt.0, y: pt.1 };
    let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe {
        let hm = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
        if !GetMonitorInfoW(hm, &mut mi).as_bool() {
            return None;
        }
    }
    MONITOR.set(Some((mi.rcMonitor, Instant::now())));
    Some(mi.rcMonitor)
}

/// 鼠标所在显示器的四角四边；与相邻显示器相接的边不算
fn region_at(pt: (i32, i32)) -> Option<Region> {
    let r = monitor_rect(pt)?;
    let outer = |x: i32, y: i32| unsafe {
        MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONULL).is_invalid()
    };
    let left = pt.0 < r.left + EDGE_SIZE && outer(r.left - 1, pt.1);
    let right = pt.0 >= r.right - EDGE_SIZE && outer(r.right, pt.1);
    let top = pt.1 < r.top + EDGE_SIZE && outer(pt.0, r.top - 1);
    let bottom = pt.1 >= r.bottom - EDGE_SIZE && outer(pt.0, r.bottom);
    classify(&r, pt, top, bottom, left, right)
}

/// 贴着某条边且离角落不超过 CORNER_SIZE 就算角
fn classify(r: &RECT, pt: (i32, i32), top: bool, bottom: bool, left: bool, right: bool) -> Option<Region> {
    let near_left = pt.0 < r.left + CORNER_SIZE;
    let near_right = pt.0 >= r.right - CORNER_SIZE;
    let near_top = pt.1 < r.top + CORNER_SIZE;
    let near_bottom = pt.1 >= r.bottom - CORNER_SIZE;
    Some(match () {
        _ if (top && near_left) || (left && near_top) => Region::TopLeft,
        _ if (top && near_right) || (right && near_top) => Region::TopRight,
        _ if (bottom && near_left) || (left && near_bottom) => Region::BottomLeft,
        _ if (bottom && near_right) || (right && near_bottom) => Region::BottomRight,
        _ if top => Region::Top,
        _ if bottom => Region::Bottom,
        _ if left => Region::Left,
        _ if right => Region::Right,
        _ => return None,
    })
}
