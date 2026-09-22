use std::cell::RefCell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU8, AtomicU32, Ordering::Relaxed};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use gesture_core::{Region, TriggerButton, WheelDir};
use windows::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
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

/// 屏幕边角判定范围（像素）
const REGION_SIZE: i32 = 48;
/// 无按键手势：滚轮停这么久就算一次手势结束
const IDLE_MS: u64 = 400;
/// 会话进行中的空闲检测步长
const IDLE_TICK_MS: u64 = 50;
/// 一格滚轮的 delta，硬件/驱动可能拆成多条小 delta 发出来
const WHEEL_DELTA: i32 = 120;
/// 唤醒钩子线程做空闲检查的自定义消息。不能用 WM_TIMER：它是最低优先级消息，
/// 鼠标一动就会被钩子事件饿死，导致多次滚轮被并进同一个会话
const WM_IDLE_TICK: u32 = WM_APP + 1;

#[derive(Debug)]
pub enum HookEvent {
    /// 移动超过阈值，正式进入手势态
    Start { button: Option<TriggerButton>, region: Option<Region>, point: (i32, i32) },
    Move { point: (i32, i32) },
    Wheel { dir: WheelDir, region: Option<Region> },
    End {
        button: Option<TriggerButton>,
        region: Option<Region>,
        /// 没有画轨迹（纯滚轮）时为空
        points: Vec<(i32, i32)>,
        times: Vec<u32>,
        wheel: Vec<WheelDir>,
    },
    Cancel,
    /// 候选态直接松手，需要补发一次真实点击
    PassThroughClick { button: TriggerButton },
    /// 准心拾取结果
    Picked(Option<super::window::WindowInfo>),
    /// 拾取拖动中，当前指向窗口变化
    PickHover(Option<super::window::WindowInfo>),
}

static TX: OnceLock<Sender<HookEvent>> = OnceLock::new();
static TRIGGER_MASK: AtomicU8 = AtomicU8::new(0);
static REGION_MASK: AtomicU8 = AtomicU8::new(0);
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

pub fn set_move_threshold(px: i32) {
    MOVE_THRESHOLD.store(px.max(1), Relaxed);
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Relaxed);
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
    phase: Phase,
    /// None 表示无按键的边角手势
    button: Option<TriggerButton>,
    region: Option<Region>,
    start: (i32, i32),
    /// 是否已经移动超过阈值（画了轨迹）
    moved: bool,
    points: Vec<(i32, i32)>,
    /// 与 points 一一对应的系统时间戳（ms）
    times: Vec<u32>,
    /// 折叠后的滚轮序列：同向连续滚动只算一格，方向反转才追加
    wheel: Vec<WheelDir>,
    /// 还没攒满一格的滚轮 delta
    wheel_acc: i32,
    /// 最后一次滚轮时间，用来判定"滚轮停了"
    last_wheel: Option<Instant>,
}

thread_local! {
    static STATE: RefCell<HookState> = const {
        RefCell::new(HookState {
            phase: Phase::Idle,
            button: None,
            region: None,
            start: (0, 0),
            moved: false,
            points: Vec::new(),
            times: Vec::new(),
            wheel: Vec::new(),
            wheel_acc: 0,
            last_wheel: None,
        })
    };
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
        let _ = tx.try_send(ev);
    }
}

fn reset(st: &mut HookState) {
    st.phase = Phase::Idle;
    st.button = None;
    st.region = None;
    st.moved = false;
    st.points.clear();
    st.times.clear();
    st.wheel.clear();
    st.wheel_acc = 0;
    st.last_wheel = None;
    TICKING.store(false, Relaxed);
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
    let (button, region) = (st.button, st.region);
    reset(st);
    send(HookEvent::End { button, region, points, times, wheel });
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

    if msg == WM_MOUSEWHEEL {
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
        // 边角滚轮手势不记录轨迹
        if st.phase == Phase::Idle || st.button.is_none() {
            return false;
        }
        st.points.push(pt);
        st.times.push(ms.time);
        if st.moved {
            send(HookEvent::Move { point: pt });
        } else {
            let (dx, dy) = (pt.0 - st.start.0, pt.1 - st.start.1);
            let th = MOVE_THRESHOLD.load(Relaxed);
            if dx * dx + dy * dy >= th * th {
                st.moved = true;
                st.phase = Phase::Gesture;
                send(HookEvent::Start { button: st.button, region: st.region, point: st.start });
                send(HookEvent::Move { point: pt });
            }
        }
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
        return true;
    }

    let Some((btn, down)) = button_of(msg, ms) else {
        return false;
    };

    if down {
        if st.phase != Phase::Idle {
            reset(st);
            send(HookEvent::Cancel);
            return true;
        }
        if !ENABLED.load(Relaxed) || TRIGGER_MASK.load(Relaxed) & btn.bit() == 0 {
            return false;
        }
        begin(st, Some(btn), None, pt, ms.time);
        return true;
    }

    if st.button != Some(btn) {
        return false;
    }
    match st.phase {
        Phase::Candidate => {
            reset(st);
            send(HookEvent::PassThroughClick { button: btn });
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

/// 允许无按键触发的边角
fn edge_region(pt: (i32, i32)) -> Option<Region> {
    region_at(pt).filter(|r| REGION_MASK.load(Relaxed) & r.bit() != 0)
}

/// 鼠标所在显示器的四角四边；与相邻显示器相接的边不算
fn region_at(pt: (i32, i32)) -> Option<Region> {
    let p = POINT { x: pt.0, y: pt.1 };
    let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe {
        let hm = MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST);
        if !GetMonitorInfoW(hm, &mut mi).as_bool() {
            return None;
        }
    }
    let r = mi.rcMonitor;
    let outer = |x: i32, y: i32| unsafe {
        MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONULL).is_invalid()
    };
    let left = pt.0 < r.left + REGION_SIZE && outer(r.left - 1, pt.1);
    let right = pt.0 >= r.right - REGION_SIZE && outer(r.right, pt.1);
    let top = pt.1 < r.top + REGION_SIZE && outer(pt.0, r.top - 1);
    let bottom = pt.1 >= r.bottom - REGION_SIZE && outer(pt.0, r.bottom);
    Some(match (top, bottom, left, right) {
        (true, _, true, _) => Region::TopLeft,
        (true, _, _, true) => Region::TopRight,
        (_, true, true, _) => Region::BottomLeft,
        (_, true, _, true) => Region::BottomRight,
        (true, ..) => Region::Top,
        (_, true, ..) => Region::Bottom,
        (_, _, true, _) => Region::Left,
        (_, _, _, true) => Region::Right,
        _ => return None,
    })
}
