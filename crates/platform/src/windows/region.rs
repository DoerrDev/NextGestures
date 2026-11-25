//! Shared, physical-pixel desktop selector. The Win32 window and loop live on the caller's worker.
use std::cell::RefCell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
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

type Region = (i32, i32, i32, i32);
const MIN_SIDE: i32 = 16;
const DRAG_THRESHOLD: u32 = 6;
static ACTIVE: AtomicBool = AtomicBool::new(false);
static WINDOW_CLASS: OnceLock<bool> = OnceLock::new();

#[derive(Default)]
struct Selection {
    screen: RECT,
    windows: Vec<RECT>,
    hover: Option<RECT>,
    start: Option<POINT>,
    drag: Option<RECT>,
    result: Option<Region>,
    frame: Option<PaintBuffer>,
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
        self.hover = Some(
            window_at(&self.windows, point)
                .or_else(|| monitor_at(point))
                .unwrap_or(self.screen),
        );
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
    unsafe { select() }.map_err(|err| err.to_string())
}

unsafe fn select() -> windows::core::Result<Option<Region>> {
    unsafe {
        if !*WINDOW_CLASS.get_or_init(|| {
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(procedure),
                lpszClassName: w!("NextGesturesRegionSelector"),
                hCursor: LoadCursorW(None, IDC_CROSS).unwrap_or_default(),
                ..Default::default()
            }) != 0
        }) {
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
            let mut message = MSG::default();
            while IsWindow(Some(hwnd)).as_bool() {
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
        if IsWindow(Some(hwnd)).as_bool() {
            let _ = DestroyWindow(hwnd);
        }
        SELECTION.with(|cell| *cell.borrow_mut() = Selection::default());
        // The pin and recorder capture the desktop only after the overlay has left composition.
        if run.as_ref().is_ok_and(Option::is_some) {
            let _ = DwmFlush();
        }
        run
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

fn visible_windows(screen: RECT) -> Vec<RECT> {
    unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> BOOL {
        let windows = unsafe { &mut *(data.0 as *mut Vec<RECT>) };
        if let Some(bounds) = window_bounds(hwnd) {
            windows.push(bounds);
        }
        true.into()
    }

    let mut windows = Vec::new();
    let _ = unsafe {
        EnumWindows(
            Some(collect),
            LPARAM(&mut windows as *mut Vec<RECT> as isize),
        )
    };
    windows
        .into_iter()
        .filter_map(|rect| intersection(rect, screen))
        .filter(|&rect| selection_region(rect).is_some())
        .collect()
}

fn window_bounds(hwnd: HWND) -> Option<RECT> {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return None;
        }
        let style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if style & WS_EX_TRANSPARENT.0 != 0 {
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

fn window_at(windows: &[RECT], point: POINT) -> Option<RECT> {
    windows.iter().copied().find(|rect| {
        (rect.left..rect.right).contains(&point.x) && (rect.top..rect.bottom).contains(&point.y)
    })
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

unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_DISPLAYCHANGE | WM_CANCELMODE | WM_CLOSE | WM_RBUTTONDOWN => {
                cancel(hwnd);
            }
            WM_KEYDOWN | WM_SYSKEYDOWN if wp.0 == usize::from(VK_ESCAPE.0) => {
                cancel(hwnd);
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
                        return (true, None);
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
        let frame = state.frame.as_mut().expect("selector paint buffer");
        frame.fill(dirty, 0x9000_0000);
        if let Some(rect) = selection
            && let Some(inside) = intersection(dirty, rect)
        {
            // Nonzero alpha keeps the highlighted interior nearly clear and still hit-testable.
            frame.fill(inside, 0x0100_0000);
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
            windows: vec![front, back],
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
