//! Shared, physical-pixel desktop selector. The Win32 window and loop live on the caller's worker.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmFlush, DwmGetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, w};

#[path = "region_elements.rs"]
mod elements;

type Region = (i32, i32, i32, i32);
const MIN_SIDE: i32 = 16;
const DRAG_THRESHOLD: u32 = 6;
const ELEMENT_TIMER: usize = 1;
static ACTIVE: AtomicBool = AtomicBool::new(false);
static WINDOW_CLASS: OnceLock<bool> = OnceLock::new();

#[derive(Default)]
struct Selection {
    screen: RECT,
    windows: Vec<(usize, RECT)>,
    cursor: Option<POINT>,
    components: HashMap<usize, Vec<elements::Component>>,
    pending: Option<(usize, Receiver<Vec<elements::Component>>)>,
    hover: Option<RECT>,
    start: Option<POINT>,
    drag: Option<RECT>,
    result: Option<Region>,
    frame: Option<PaintBuffer>,
    /// 截图模式：框选前冻结的整屏画面，框选时以它为底而不是透出实时桌面。
    frozen: Option<Rc<Frozen>>,
    /// 框选完成后窗口留作截图编辑的暗色背景，不再响应框选。
    backdrop: bool,
    /// 背景模式下右键 / 关闭转交给截图编辑窗口。
    owner: Option<HWND>,
}

/// 冻结的虚拟桌面画面（物理像素，BGRA，不透明）。
pub(super) struct Frozen {
    pub(super) screen: RECT,
    bright: Vec<u32>,
    dim: Vec<u32>,
}

impl Frozen {
    fn capture(screen: RECT) -> windows::core::Result<Self> {
        let (width, height) = (screen.right - screen.left, screen.bottom - screen.top);
        let buffer = PaintBuffer::new(width, height)?;
        unsafe {
            let desktop = GetDC(None);
            if desktop.is_invalid() {
                return Err(windows::core::Error::from_thread());
            }
            let result = BitBlt(
                buffer.dc,
                0,
                0,
                width,
                height,
                Some(desktop),
                screen.left,
                screen.top,
                SRCCOPY | CAPTUREBLT,
            );
            ReleaseDC(None, desktop);
            result?;
            let _ = GdiFlush();
        }
        let count = width as usize * height as usize;
        let bright: Vec<u32> = unsafe { std::slice::from_raw_parts(buffer.pixels, count) }
            .iter()
            .map(|pixel| pixel | 0xff00_0000)
            .collect();
        let dim = bright.iter().map(|&pixel| dimmed(pixel)).collect();
        Ok(Self { screen, bright, dim })
    }

    /// 截取桌面坐标中的矩形（须位于屏幕内），返回 BGRA 字节。
    pub(super) fn crop(&self, rect: RECT) -> Vec<u8> {
        let stride = (self.screen.right - self.screen.left) as usize;
        let (x, y) = ((rect.left - self.screen.left) as usize, (rect.top - self.screen.top) as usize);
        let width = (rect.right - rect.left) as usize;
        let mut out = Vec::with_capacity(width * (rect.bottom - rect.top) as usize * 4);
        for row in 0..(rect.bottom - rect.top) as usize {
            let start = (y + row) * stride + x;
            for pixel in &self.bright[start..start + width] {
                out.extend_from_slice(&pixel.to_le_bytes());
            }
        }
        out
    }
}

/// 与实时框选时 0x90 黑色遮罩叠加后的颜色一致。
fn dimmed(pixel: u32) -> u32 {
    let scale = |shift: u32| (((pixel >> shift) & 0xff) * 111 / 255) << shift;
    0xff00_0000 | scale(16) | scale(8) | scale(0)
}

/// 截图编辑期间保留的冻结背景窗口；释放时关闭窗口并结束框选会话。
pub(super) struct Backdrop {
    hwnd: HWND,
    _session: Session,
}

impl Backdrop {
    /// 编辑窗口接管后整屏变暗，选区由编辑窗口自己显示。
    pub(super) fn attach(&self, owner: HWND) {
        SELECTION.with(|cell| {
            let mut state = cell.borrow_mut();
            state.owner = Some(owner);
            state.drag = None;
            state.hover = None;
        });
        let _ = render(self.hwnd, None);
    }
}

impl Drop for Backdrop {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
        SELECTION.with(|cell| *cell.borrow_mut() = Selection::default());
    }
}

struct PaintBuffer {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    pixels: *mut u32,
    width: usize,
}

impl PaintBuffer {
    fn new(width: i32, height: i32) -> windows::core::Result<Self> {
        unsafe {
            let mut buffer = Self {
                dc: HDC::default(),
                bitmap: HBITMAP::default(),
                previous: HGDIOBJ::default(),
                pixels: std::ptr::null_mut(),
                width: width as usize,
            };
            buffer.dc = CreateCompatibleDC(None);
            if buffer.dc.is_invalid() {
                return Err(windows::core::Error::from_thread());
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut pixels = std::ptr::null_mut();
            buffer.bitmap = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut pixels, None, 0)?;
            buffer.pixels = pixels.cast();
            buffer.previous = SelectObject(buffer.dc, buffer.bitmap.into());
            if buffer.previous.is_invalid() {
                return Err(windows::core::Error::from_thread());
            }
            Ok(buffer)
        }
    }

    fn fill(&mut self, rect: RECT, pixel: u32) {
        for y in rect.top..rect.bottom {
            let offset = y as usize * self.width + rect.left as usize;
            let length = (rect.right - rect.left) as usize;
            // All rectangles are clipped to the top-down 32-bit DIB before painting.
            unsafe { std::slice::from_raw_parts_mut(self.pixels.add(offset), length) }.fill(pixel);
        }
    }

    /// 从同尺寸的整屏像素复制矩形区域。
    fn copy(&mut self, rect: RECT, source: &[u32]) {
        for y in rect.top..rect.bottom {
            let offset = y as usize * self.width + rect.left as usize;
            let length = (rect.right - rect.left) as usize;
            unsafe { std::slice::from_raw_parts_mut(self.pixels.add(offset), length) }
                .copy_from_slice(&source[offset..offset + length]);
        }
    }
}

impl Drop for PaintBuffer {
    fn drop(&mut self) {
        unsafe {
            if !self.previous.is_invalid() {
                SelectObject(self.dc, self.previous);
            }
            if !self.bitmap.is_invalid() {
                let _ = DeleteObject(self.bitmap.into());
            }
            if !self.dc.is_invalid() {
                let _ = DeleteDC(self.dc);
            }
        }
    }
}

impl Selection {
    fn update_hover(&mut self, point: POINT) {
        self.cursor = Some(point);
        self.hover = Some(
            if let Some((window, bounds)) = window_at(&self.windows, point) {
                if let Some(components) = self.components.get(&window) {
                    elements::component_at(components, point).unwrap_or(bounds)
                } else {
                    if self.pending.is_none() {
                        self.pending =
                            elements::request(window, bounds).map(|result| (window, result));
                    }
                    bounds
                }
            } else {
                monitor_at(point).unwrap_or(self.screen)
            },
        );
    }

    fn poll_components(&mut self) -> Option<RECT> {
        let point = self.cursor.filter(|_| self.start.is_none())?;
        if let Some((window, receiver)) = &self.pending {
            let components = match receiver.try_recv() {
                Ok(components) => components,
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => Vec::new(),
            };
            self.components.insert(*window, components);
            self.pending = None;
        } else if let Some((window, bounds)) = window_at(&self.windows, point) {
            if !self.components.contains_key(&window) {
                self.pending = elements::request(window, bounds).map(|result| (window, result));
            }
            return None;
        } else {
            return None;
        }
        let before = self.client_selection();
        self.update_hover(point);
        changed_area(before, self.client_selection())
    }

    fn update_drag(&mut self, point: POINT) {
        if let Some(start) = self.start
            && (self.drag.is_some()
                || start.x.abs_diff(point.x).max(start.y.abs_diff(point.y)) >= DRAG_THRESHOLD)
        {
            self.drag = Some(rectangle(start, point, self.screen));
        }
    }

    fn client_selection(&self) -> Option<RECT> {
        let mut rect = self.drag.or(self.hover)?;
        if rect.left == rect.right || rect.top == rect.bottom {
            return None;
        }
        rect.left -= self.screen.left;
        rect.right -= self.screen.left;
        rect.top -= self.screen.top;
        rect.bottom -= self.screen.top;
        Some(rect)
    }
}

thread_local! {
    static SELECTION: RefCell<Selection> = RefCell::new(Selection::default());
}

pub(super) fn is_selecting() -> bool {
    ACTIVE.load(Ordering::Acquire)
}

struct Session(DPI_AWARENESS_CONTEXT);
impl Drop for Session {
    fn drop(&mut self) {
        unsafe { SetThreadDpiAwarenessContext(self.0) };
        ACTIVE.store(false, Ordering::Release);
    }
}

/// Click the highlighted window or monitor, or drag a custom rectangle.
/// Escape and right click cancel.
/// Returns (left, top, width, height) in virtual desktop physical pixels.
pub fn select_region() -> Result<Option<Region>, String> {
    if ACTIVE.swap(true, Ordering::AcqRel) {
        return Err("已有框选窗口打开".into());
    }

    let session = Session(unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
    });
    if session.0.is_invalid() {
        return Err("初始化框选窗口 DPI 失败".into());
    }
    unsafe { select(false, || {}) }
        .map(|selected| selected.map(|(region, _)| region))
        .map_err(|err| err.to_string())
}

/// 截图框选：先冻结整屏（随后调用 `captured`），在冻结画面上框选；
/// 选中后框选窗口不关闭，作为暗色背景交给截图编辑使用。
/// 返回 None 表示另一个框选或截图正在进行，或用户取消。
pub(super) fn select_frozen(
    captured: impl FnOnce(),
) -> Result<Option<(Region, Rc<Frozen>, Backdrop)>, String> {
    if ACTIVE.swap(true, Ordering::AcqRel) {
        captured();
        return Ok(None);
    }
    let session = Session(unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
    });
    if session.0.is_invalid() {
        captured();
        return Err("初始化框选窗口 DPI 失败".into());
    }
    match unsafe { select(true, captured) } {
        Ok(Some((region, Some(hwnd)))) => {
            let frozen = SELECTION.with(|cell| cell.borrow().frozen.clone());
            let frozen = frozen.expect("frozen selection keeps its capture");
            Ok(Some((region, frozen, Backdrop { hwnd, _session: session })))
        }
        Ok(_) => Ok(None),
        Err(err) => Err(err.to_string()),
    }
}

/// 第二个返回值是留作背景的窗口（仅冻结模式选中时）。
unsafe fn select(
    freeze: bool,
    captured: impl FnOnce(),
) -> windows::core::Result<Option<(Region, Option<HWND>)>> {
    unsafe {
        if !*WINDOW_CLASS.get_or_init(|| {
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(procedure),
                lpszClassName: w!("NextGesturesRegionSelector"),
                hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
                ..Default::default()
            }) != 0
        }) {
            captured();
            return Err(windows::core::Error::from_thread());
        }
        let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
        let height = GetSystemMetrics(SM_CYVIRTUALSCREEN);
        let screen = RECT {
            left: x,
            top: y,
            right: x + width,
            bottom: y + height,
        };
        let frozen = if freeze { Some(Frozen::capture(screen)) } else { None };
        captured();
        let frozen = frozen.transpose()?.map(Rc::new);
        let frame = PaintBuffer::new(width, height)?;
        // Snapshot the underlying top-level windows before the selector enters the Z order.
        let windows = visible_windows(screen);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            w!("NextGesturesRegionSelector"),
            w!("框选区域 · Esc 取消"),
            WS_POPUP,
            x,
            y,
            width,
            height,
            None,
            None,
            None,
            None,
        )?;
        SELECTION.with(|cell| {
            let mut state = cell.borrow_mut();
            *state = Selection {
                screen,
                windows,
                frame: Some(frame),
                frozen,
                ..Default::default()
            };
            let mut cursor = POINT::default();
            let _ = GetCursorPos(&mut cursor);
            state.update_hover(cursor);
        });
        let run = (|| {
            render(hwnd, None)?;
            let _ = ShowWindow(hwnd, SW_SHOW);
            super::pin::activate_native_window(hwnd);
            SetTimer(Some(hwnd), ELEMENT_TIMER, 32, None);
            let mut message = MSG::default();
            while IsWindow(Some(hwnd)).as_bool()
                && !SELECTION.with(|cell| cell.borrow().backdrop)
            {
                match GetMessageW(&mut message, None, 0, 0).0 {
                    -1 => return Err(windows::core::Error::from_thread()),
                    0 => break,
                    _ => {
                        let _ = TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                }
            }
            Ok(SELECTION.with(|cell| cell.borrow_mut().result.take()))
        })();
        if let Ok(Some(region)) = run
            && freeze
            && IsWindow(Some(hwnd)).as_bool()
        {
            let _ = KillTimer(Some(hwnd), ELEMENT_TIMER);
            return Ok(Some((region, Some(hwnd))));
        }
        if IsWindow(Some(hwnd)).as_bool() {
            let _ = DestroyWindow(hwnd);
        }
        SELECTION.with(|cell| *cell.borrow_mut() = Selection::default());
        // The pin and recorder capture the desktop only after the overlay has left composition.
        if run.as_ref().is_ok_and(Option::is_some) {
            let _ = DwmFlush();
        }
        run.map(|region| region.map(|region| (region, None)))
    }
}

fn rectangle(a: POINT, b: POINT, limits: RECT) -> RECT {
    RECT {
        left: a.x.min(b.x).clamp(limits.left, limits.right),
        top: a.y.min(b.y).clamp(limits.top, limits.bottom),
        right: a.x.max(b.x).clamp(limits.left, limits.right),
        bottom: a.y.max(b.y).clamp(limits.top, limits.bottom),
    }
}

fn intersection(a: RECT, b: RECT) -> Option<RECT> {
    let mut rect = RECT::default();
    unsafe { IntersectRect(&mut rect, &a, &b) }
        .as_bool()
        .then_some(rect)
}

fn visible_windows(screen: RECT) -> Vec<(usize, RECT)> {
    unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> BOOL {
        let windows = unsafe { &mut *(data.0 as *mut Vec<(usize, RECT)>) };
        if let Some(bounds) = window_bounds(hwnd) {
            windows.push((hwnd.0 as usize, bounds));
        }
        true.into()
    }

    let mut windows = Vec::new();
    let _ = unsafe {
        EnumWindows(
            Some(collect),
            LPARAM(&mut windows as *mut Vec<(usize, RECT)> as isize),
        )
    };
    windows
        .into_iter()
        .filter_map(|(hwnd, rect)| intersection(rect, screen).map(|rect| (hwnd, rect)))
        .filter(|&(_, rect)| selection_region(rect).is_some())
        .collect()
}

fn window_bounds(hwnd: HWND) -> Option<RECT> {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return None;
        }
        let style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        // Layered, non-activating tool windows include desktop overlays whose
        // click-through behavior comes from hit testing instead of the style bit.
        let overlay = (WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW).0;
        if style & WS_EX_TRANSPARENT.0 != 0 || style & overlay == overlay {
            return None;
        }
        let mut class = [0_u16; 64];
        let length = GetClassNameW(hwnd, &mut class) as usize;
        // Treat the desktop and taskbar as monitor fallback, not capture targets.
        if [
            w!("Progman"),
            w!("WorkerW"),
            w!("Shell_TrayWnd"),
            w!("Shell_SecondaryTrayWnd"),
        ]
        .iter()
        .any(|name| name.as_wide() == &class[..length])
        {
            return None;
        }
        let mut cloaked = 0_u32;
        let _ = DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast(),
            size_of::<u32>() as u32,
        );
        if cloaked != 0 {
            return None;
        }
        // DWM reports visible physical bounds without invisible resize margins.
        let mut rect = RECT::default();
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut rect as *mut RECT).cast(),
            size_of::<RECT>() as u32,
        )
        .is_err()
        {
            GetWindowRect(hwnd, &mut rect).ok()?;
        }
        Some(rect)
    }
}

fn contains(rect: RECT, point: POINT) -> bool {
    (rect.left..rect.right).contains(&point.x) && (rect.top..rect.bottom).contains(&point.y)
}

fn window_at(windows: &[(usize, RECT)], point: POINT) -> Option<(usize, RECT)> {
    windows
        .iter()
        .copied()
        .find(|&(_, rect)| contains(rect, point))
}

fn monitor_at(point: POINT) -> Option<RECT> {
    let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetMonitorInfoW(monitor, &mut info) }
        .as_bool()
        .then_some(info.rcMonitor)
}

fn mouse_point(position: LPARAM, screen: RECT) -> Option<POINT> {
    // Capture mouse coordinates from the event, so a later cursor move cannot change its endpoint.
    // Win32 mouse LPARAM uses signed 16-bit coordinates. Fall back for very large desktops.
    if screen.right - screen.left <= i32::from(i16::MAX)
        && screen.bottom - screen.top <= i32::from(i16::MAX)
    {
        return Some(POINT {
            x: screen.left + i32::from(position.0 as i16),
            y: screen.top + i32::from((position.0 >> 16) as i16),
        });
    }
    let mut point = POINT::default();
    unsafe { GetCursorPos(&mut point) }.ok().map(|()| point)
}

unsafe fn cancel(hwnd: HWND) {
    SELECTION.with(|cell| cell.borrow_mut().start = None);
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

/// 截图编辑期间的背景：不再框选；点击把焦点还给编辑窗口，右键 / 关闭退出截图。
unsafe fn backdrop_procedure(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let owner = SELECTION.with(|cell| cell.borrow().owner);
    unsafe {
        match msg {
            // 点击背景不激活、不改变层级，避免盖住选区和工具栏。
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_MBUTTONDOWN => {
                if let Some(owner) = owner {
                    super::pin::activate_native_window(owner);
                }
                LRESULT(0)
            }
            WM_RBUTTONUP | WM_CLOSE | WM_DISPLAYCHANGE => {
                if let Some(owner) = owner {
                    let _ = PostMessageW(Some(owner), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                LRESULT(0)
            }
            WM_RBUTTONDOWN | WM_MOUSEMOVE | WM_LBUTTONUP | WM_TIMER | WM_CAPTURECHANGED
            | WM_CANCELMODE => LRESULT(0),
            WM_ERASEBKGND => LRESULT(1),
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if SELECTION.with(|cell| cell.try_borrow().is_ok_and(|state| state.backdrop)) {
        return unsafe { backdrop_procedure(hwnd, msg, wp, lp) };
    }
    unsafe {
        match msg {
            WM_DISPLAYCHANGE | WM_CANCELMODE | WM_CLOSE | WM_RBUTTONDOWN => {
                cancel(hwnd);
            }
            WM_KEYDOWN | WM_SYSKEYDOWN if wp.0 == usize::from(VK_ESCAPE.0) => {
                cancel(hwnd);
            }
            WM_TIMER if wp.0 == ELEMENT_TIMER => {
                let dirty = SELECTION.with(|cell| cell.borrow_mut().poll_components());
                if let Some(rect) = dirty
                    && render(hwnd, Some(rect)).is_err()
                {
                    cancel(hwnd);
                }
            }
            WM_LBUTTONDOWN => {
                let selection = SELECTION.with(|cell| {
                    let mut state = cell.borrow_mut();
                    let point = mouse_point(lp, state.screen)?;
                    let before = state.client_selection();
                    state.update_hover(point);
                    state.start = Some(point);
                    Some(changed_area(before, state.client_selection()))
                });
                if let Some(dirty) = selection {
                    SetCapture(hwnd);
                    if let Some(rect) = dirty
                        && render(hwnd, Some(rect)).is_err()
                    {
                        cancel(hwnd);
                    }
                }
            }
            WM_MOUSEMOVE => {
                let dirty = SELECTION.with(|cell| {
                    let mut state = cell.borrow_mut();
                    let point = mouse_point(lp, state.screen)?;
                    let before = state.client_selection();
                    if state.start.is_some() {
                        state.update_drag(point);
                    } else {
                        state.update_hover(point);
                    }
                    changed_area(before, state.client_selection())
                });
                if let Some(rect) = dirty
                    && render(hwnd, Some(rect)).is_err()
                {
                    cancel(hwnd);
                }
            }
            WM_LBUTTONUP => {
                let (selected, dirty) = SELECTION.with(|cell| {
                    let mut state = cell.borrow_mut();
                    if state.start.is_none() {
                        return (false, None);
                    }
                    let before = state.client_selection();
                    let end = mouse_point(lp, state.screen);
                    if let Some(point) = end {
                        state.update_drag(point);
                    }
                    state.start = None;
                    let target = match state.drag.take() {
                        Some(rect) => selection_region(rect),
                        None => state.hover.and_then(selection_region),
                    };
                    if end.is_some() && target.is_some() {
                        state.result = target;
                        // 截图模式保留窗口作背景，消息循环看到该标记后返回。
                        state.backdrop = state.frozen.is_some();
                        if state.backdrop
                            && let Some((x, y, width, height)) = target
                        {
                            state.drag = Some(RECT {
                                left: x,
                                top: y,
                                right: x + width,
                                bottom: y + height,
                            });
                        }
                        return (!state.backdrop, None);
                    }
                    if let Some(point) = end {
                        state.update_hover(point);
                    }
                    (false, changed_area(before, state.client_selection()))
                });
                let _ = ReleaseCapture();
                if selected {
                    let _ = DestroyWindow(hwnd);
                } else if let Some(rect) = dirty
                    && render(hwnd, Some(rect)).is_err()
                {
                    cancel(hwnd);
                }
            }
            WM_ERASEBKGND => return LRESULT(1),
            WM_CAPTURECHANGED => {
                let interrupted = SELECTION.with(|cell| cell.borrow().start.is_some());
                if interrupted {
                    cancel(hwnd);
                }
            }
            _ => return DefWindowProcW(hwnd, msg, wp, lp),
        }
        LRESULT(0)
    }
}

fn selection_region(rect: RECT) -> Option<Region> {
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    (width >= MIN_SIDE && height >= MIN_SIDE).then_some((rect.left, rect.top, width, height))
}

fn changed_area(before: Option<RECT>, after: Option<RECT>) -> Option<RECT> {
    match (before, after) {
        (Some(a), Some(b)) if a == b => None,
        (Some(a), Some(b)) => {
            let mut union = RECT::default();
            let _ = unsafe { UnionRect(&mut union, &a, &b) };
            Some(union)
        }
        (a, b) => a.or(b),
    }
}

fn render(hwnd: HWND, dirty: Option<RECT>) -> windows::core::Result<()> {
    let (dc, size, dirty) = SELECTION.with(|cell| {
        let mut state = cell.borrow_mut();
        let size = SIZE {
            cx: state.screen.right - state.screen.left,
            cy: state.screen.bottom - state.screen.top,
        };
        let dirty = dirty.unwrap_or(RECT {
            right: size.cx,
            bottom: size.cy,
            ..Default::default()
        });
        let selection = state.client_selection();
        let frozen = state.frozen.clone();
        let frame = state.frame.as_mut().expect("selector paint buffer");
        match &frozen {
            Some(frozen) => frame.copy(dirty, &frozen.dim),
            None => frame.fill(dirty, 0x9000_0000),
        }
        if let Some(rect) = selection
            && let Some(inside) = intersection(dirty, rect)
        {
            match &frozen {
                Some(frozen) => frame.copy(inside, &frozen.bright),
                // Nonzero alpha keeps the highlighted interior nearly clear and still hit-testable.
                None => frame.fill(inside, 0x0100_0000),
            }
            let sides = [
                RECT {
                    right: (rect.left + 2).min(rect.right),
                    ..rect
                },
                RECT {
                    left: (rect.right - 2).max(rect.left),
                    ..rect
                },
                RECT {
                    bottom: (rect.top + 2).min(rect.bottom),
                    ..rect
                },
                RECT {
                    top: (rect.bottom - 2).max(rect.top),
                    ..rect
                },
            ];
            for side in sides {
                if let Some(visible) = intersection(dirty, side) {
                    frame.fill(visible, 0xff55_d2ff);
                }
            }
        }
        (frame.dc, size, dirty)
    });
    let source = POINT::default();
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
        ..Default::default()
    };
    let info = UPDATELAYEREDWINDOWINFO {
        cbSize: size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
        psize: &size,
        hdcSrc: dc,
        pptSrc: &source,
        crKey: COLORREF(0),
        pblend: &blend,
        dwFlags: ULW_ALPHA,
        prcDirty: &dirty,
        ..Default::default()
    };
    unsafe { UpdateLayeredWindowIndirect(hwnd, &info) }.ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverse_drag_clips_to_negative_virtual_desktop_origin() {
        let screen = RECT {
            left: -1920,
            top: -180,
            right: 2560,
            bottom: 1440,
        };
        let a = POINT { x: 320, y: 480 };
        let b = POINT { x: -2100, y: -20 };
        let expected = RECT {
            left: -1920,
            top: -20,
            right: 320,
            bottom: 480,
        };
        assert_eq!(rectangle(a, b, screen), expected);
        assert_eq!(rectangle(b, a, screen), expected);
    }

    #[test]
    fn hover_picks_the_topmost_window_or_the_monitor() {
        let origin = POINT::default();
        let front = RECT {
            left: -10,
            top: -10,
            right: 50,
            bottom: 50,
        };
        let back = RECT {
            left: -20,
            top: -20,
            right: 100,
            bottom: 100,
        };
        let mut state = Selection {
            windows: vec![(1, front), (2, back)],
            components: HashMap::from([(1, Vec::new()), (2, Vec::new())]),
            ..Default::default()
        };
        state.update_hover(origin);
        assert_eq!(state.hover, Some(front));
        state.update_hover(POINT { x: 50, y: 10 });
        assert_eq!(state.hover, Some(back));
        state.windows.clear();
        state.update_hover(origin);
        let screen = monitor_at(origin).expect("primary monitor bounds");
        assert_eq!(state.hover, Some(screen));
        assert_eq!((screen.left, screen.top), (0, 0));
        assert!(selection_region(screen).is_some());
    }

    #[test]
    fn small_motion_keeps_the_hover_and_a_drag_stays_a_drag() {
        let screen = RECT {
            right: 100,
            bottom: 100,
            ..Default::default()
        };
        let mut state = Selection {
            screen,
            hover: Some(screen),
            start: Some(POINT { x: 20, y: 20 }),
            ..Default::default()
        };
        state.update_drag(POINT { x: 24, y: 23 });
        assert_eq!(state.client_selection(), Some(screen));
        assert_eq!(state.drag, None);
        state.update_drag(POINT { x: 48, y: 50 });
        assert_eq!(
            state.drag.and_then(selection_region),
            Some((20, 20, 28, 30))
        );
        state.update_drag(POINT { x: 20, y: 20 });
        assert!(state.drag.is_some());
        assert_eq!(state.drag.and_then(selection_region), None);
    }

    #[test]
    #[ignore = "opens the live Windows desktop overlay"]
    fn native_selector_snaps_to_webview_row() {
        use std::time::{Duration, Instant};
        use windows::core::PCWSTR;

        let expected: Vec<i32> = std::env::var("NEXTGESTURES_TEST_ROW")
            .expect("set NEXTGESTURES_TEST_ROW to a visible gesture row's x,y,width,height")
            .split(',')
            .map(|value| value.parse().unwrap())
            .collect();
        let [x, y, width, height] = expected[..] else {
            panic!("expected four coordinates")
        };
        let _dpi = Session(unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
        });
        let app = HWND(
            std::env::var("NEXTGESTURES_TEST_HWND")
                .expect("set NEXTGESTURES_TEST_HWND to the NextGestures main window handle")
                .parse::<usize>()
                .unwrap() as *mut _,
        );
        struct RestoreWindow {
            hwnd: HWND,
            visible: bool,
            topmost: bool,
        }
        impl Drop for RestoreWindow {
            fn drop(&mut self) {
                let _ = unsafe {
                    SetWindowPos(
                        self.hwnd,
                        Some(if self.topmost {
                            HWND_TOPMOST
                        } else {
                            HWND_NOTOPMOST
                        }),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE
                            | SWP_NOSIZE
                            | SWP_NOACTIVATE
                            | if self.visible {
                                SET_WINDOW_POS_FLAGS(0)
                            } else {
                                SWP_HIDEWINDOW
                            },
                    )
                };
            }
        }
        let _restore = unsafe {
            RestoreWindow {
                hwnd: app,
                visible: IsWindowVisible(app).as_bool(),
                topmost: GetWindowLongW(app, GWL_EXSTYLE) as u32 & WS_EX_TOPMOST.0 != 0,
            }
        };
        unsafe {
            SetWindowPos(
                app,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )
            .unwrap();
        }
        let cursor = POINT {
            x: x + width / 2,
            y: y + height / 2,
        };
        let bounds = window_bounds(app).unwrap_or_else(|| unsafe {
            panic!(
                "fixture HWND {app:?} is not selectable, visible={:?}, minimized={:?}, style={:#x}",
                IsWindowVisible(app),
                IsIconic(app),
                GetWindowLongW(app, GWL_EXSTYLE)
            )
        });
        let components = elements::request(app.0 as usize, bounds)
            .unwrap()
            .recv_timeout(Duration::from_secs(4))
            .unwrap();
        assert_eq!(
            elements::component_at(&components, cursor).and_then(selection_region),
            Some((x, y, width, height)),
            "WebView candidates for {bounds:?}: {components:?}",
        );
        let worker = std::thread::spawn(select_region);
        let deadline = Instant::now() + Duration::from_secs(5);
        let hwnd = loop {
            if let Ok(hwnd) =
                unsafe { FindWindowW(w!("NextGesturesRegionSelector"), PCWSTR::null()) }
                && unsafe { IsWindowVisible(hwnd).as_bool() }
            {
                break hwnd;
            }
            assert!(Instant::now() < deadline, "selector did not open");
            std::thread::sleep(Duration::from_millis(10));
        };
        let mut desktop = RECT::default();
        unsafe { GetWindowRect(hwnd, &mut desktop) }.unwrap();
        let position =
            LPARAM(((cursor.x - desktop.left) | ((cursor.y - desktop.top) << 16)) as isize);
        let outline_visible = loop {
            let pixel = unsafe {
                SendMessageW(hwnd, WM_MOUSEMOVE, None, Some(position));
                let dc = GetDC(None);
                let pixel = GetPixel(dc, x + 1, cursor.y);
                ReleaseDC(None, dc);
                pixel
            };
            if pixel == COLORREF(0x00ff_d255) || Instant::now() >= deadline {
                break pixel == COLORREF(0x00ff_d255);
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        unsafe {
            PostMessageW(Some(hwnd), WM_LBUTTONDOWN, WPARAM(1), position).unwrap();
            PostMessageW(Some(hwnd), WM_LBUTTONUP, WPARAM(0), position).unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while !worker.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !worker.is_finished() {
            let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
            panic!("selector did not close");
        }
        assert_eq!(worker.join().unwrap().unwrap(), Some((x, y, width, height)));
        assert!(outline_visible, "the component's outline did not appear");
    }

    #[test]
    #[ignore = "opens the live Windows desktop overlay"]
    fn native_selector_click_drag_and_escape() {
        use std::time::{Duration, Instant};
        use windows::core::PCWSTR;

        fn run(messages: &[(u32, WPARAM, LPARAM)], hover: Option<POINT>) -> Option<Region> {
            let worker = std::thread::spawn(select_region);
            let deadline = Instant::now() + Duration::from_secs(3);
            let hwnd = loop {
                if let Ok(hwnd) =
                    unsafe { FindWindowW(w!("NextGesturesRegionSelector"), PCWSTR::null()) }
                    && unsafe { IsWindowVisible(hwnd).as_bool() }
                {
                    break hwnd;
                }
                assert!(Instant::now() < deadline, "selector window did not open");
                std::thread::sleep(Duration::from_millis(10));
            };
            if let Some(cursor) = hover {
                let mut desktop = RECT::default();
                unsafe { GetWindowRect(hwnd, &mut desktop) }.unwrap();
                let position = point(cursor.x - desktop.left, cursor.y - desktop.top);
                unsafe {
                    SendMessageW(hwnd, WM_MOUSEMOVE, Some(WPARAM(0)), Some(position));
                    // A fully transparent interior would send the click to the app beneath.
                    assert_eq!(WindowFromPoint(cursor), hwnd);
                    PostMessageW(Some(hwnd), WM_LBUTTONDOWN, WPARAM(1), position).unwrap();
                    PostMessageW(Some(hwnd), WM_LBUTTONUP, WPARAM(0), position).unwrap();
                }
            }
            for &(msg, wparam, lparam) in messages {
                unsafe { PostMessageW(Some(hwnd), msg, wparam, lparam) }.unwrap();
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            while !worker.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if !worker.is_finished() {
                let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
                panic!("selector did not close");
            }
            worker.join().unwrap().unwrap()
        }

        fn point(x: i32, y: i32) -> LPARAM {
            LPARAM((x | (y << 16)) as isize)
        }

        let _dpi = Session(unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
        });
        assert_eq!(
            run(
                &[(WM_KEYDOWN, WPARAM(usize::from(VK_ESCAPE.0)), LPARAM(0))],
                None
            ),
            None
        );
        let region = run(
            &[
                (WM_LBUTTONDOWN, WPARAM(1), point(190, 160)),
                (WM_MOUSEMOVE, WPARAM(1), point(150, 130)),
                (WM_LBUTTONUP, WPARAM(0), point(60, 50)),
            ],
            None,
        );
        let (_, _, width, height) = region.expect("drag returns a desktop region");
        assert_eq!((width, height), (130, 110));

        unsafe {
            let fixture = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                w!("STATIC"),
                w!("NextGestures selector test"),
                WS_POPUP | WS_VISIBLE,
                80,
                90,
                240,
                180,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let _ = DwmFlush();
            let selected = run(&[], Some(POINT { x: 150, y: 150 }));
            DestroyWindow(fixture).unwrap();
            assert_eq!(selected, Some((80, 90, 240, 180)));
        }
    }
}
