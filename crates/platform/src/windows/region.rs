//! Shared, physical-pixel desktop selector. The Win32 window and loop live on the caller's worker.
use std::cell::RefCell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

type Region = (i32, i32, i32, i32);
const MASK_KEY: COLORREF = COLORREF(0x00ff00ff);
const MIN_SIDE: i32 = 16;
static ACTIVE: AtomicBool = AtomicBool::new(false);
static WINDOW_CLASS: OnceLock<bool> = OnceLock::new();

#[derive(Default)]
struct Selection {
    screen: RECT,
    start: Option<POINT>,
    current: Option<POINT>,
    result: Option<Region>,
    frame: Option<PaintBuffer>,
}

struct PaintBuffer {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    interior: HBRUSH,
    outline: HBRUSH,
}

impl PaintBuffer {
    fn new(width: i32, height: i32) -> windows::core::Result<Self> {
        unsafe {
            let mut buffer = Self {
                dc: HDC::default(),
                bitmap: HBITMAP::default(),
                previous: HGDIOBJ::default(),
                interior: CreateSolidBrush(MASK_KEY),
                outline: CreateSolidBrush(COLORREF(0x00ffd255)),
            };
            let screen = GetDC(None);
            if screen.is_invalid() {
                return Err(windows::core::Error::from_thread());
            }
            buffer.dc = CreateCompatibleDC(Some(screen));
            buffer.bitmap = CreateCompatibleBitmap(screen, width, height);
            let _ = ReleaseDC(None, screen);
            if buffer.dc.is_invalid()
                || buffer.bitmap.is_invalid()
                || buffer.interior.is_invalid()
                || buffer.outline.is_invalid()
            {
                return Err(windows::core::Error::from_thread());
            }
            buffer.previous = SelectObject(buffer.dc, buffer.bitmap.into());
            if buffer.previous.is_invalid() {
                return Err(windows::core::Error::from_thread());
            }
            Ok(buffer)
        }
    }
}

impl Drop for PaintBuffer {
    fn drop(&mut self) {
        unsafe {
            if !self.previous.is_invalid() {
                SelectObject(self.dc, self.previous);
            }
            let objects: [HGDIOBJ; 3] = [
                self.bitmap.into(),
                self.interior.into(),
                self.outline.into(),
            ];
            for object in objects {
                if !object.is_invalid() {
                    let _ = DeleteObject(object);
                }
            }
            if !self.dc.is_invalid() {
                let _ = DeleteDC(self.dc);
            }
        }
    }
}

impl Selection {
    fn client_selection(&self) -> Option<RECT> {
        let (a, b) = self.start.zip(self.current)?;
        let mut rect = rectangle(a, b, self.screen);
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

/// Drag with the left mouse button. Escape and right click cancel.
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
            *cell.borrow_mut() = Selection {
                screen,
                frame: Some(frame),
                ..Default::default()
            }
        });
        let run = (|| {
            SetLayeredWindowAttributes(hwnd, MASK_KEY, 144, LWA_ALPHA | LWA_COLORKEY)?;
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = UpdateWindow(hwnd);
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
                let started = SELECTION.with(|cell| {
                    let mut state = cell.borrow_mut();
                    if let Some(point) = mouse_point(lp, state.screen) {
                        state.start = Some(point);
                        state.current = Some(point);
                        true
                    } else {
                        false
                    }
                });
                if started {
                    SetCapture(hwnd);
                }
            }
            WM_MOUSEMOVE => {
                let dirty = SELECTION.with(|cell| {
                    let mut state = cell.borrow_mut();
                    state.start?;
                    let before = state.client_selection();
                    state.current = mouse_point(lp, state.screen);
                    let after = state.client_selection();
                    if before == after {
                        return None;
                    }
                    match (before, after) {
                        (Some(a), Some(b)) => {
                            let mut union = RECT::default();
                            let _ = UnionRect(&mut union, &a, &b);
                            Some(union)
                        }
                        (a, b) => a.or(b),
                    }
                });
                if let Some(rect) = dirty {
                    let _ = InvalidateRect(Some(hwnd), Some(&rect), false);
                }
            }
            WM_LBUTTONUP => {
                let (selected, dirty) = SELECTION.with(|cell| {
                    let mut state = cell.borrow_mut();
                    let dirty = state.client_selection();
                    let Some(start) = state.start.take() else {
                        return (false, dirty);
                    };
                    state.current = None;
                    let Some(end) = mouse_point(lp, state.screen) else {
                        return (false, dirty);
                    };
                    let rect = rectangle(start, end, state.screen);
                    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
                    if w >= MIN_SIDE && h >= MIN_SIDE {
                        state.result = Some((rect.left, rect.top, w, h));
                    }
                    (state.result.is_some(), dirty)
                });
                let _ = ReleaseCapture();
                if selected {
                    let _ = DestroyWindow(hwnd);
                } else if let Some(rect) = dirty {
                    let _ = InvalidateRect(Some(hwnd), Some(&rect), false);
                }
            }
            WM_ERASEBKGND => return LRESULT(1),
            WM_CAPTURECHANGED => {
                let interrupted = SELECTION.with(|cell| cell.borrow().start.is_some());
                if interrupted {
                    cancel(hwnd);
                }
            }
            WM_PAINT => paint(hwnd),
            _ => return DefWindowProcW(hwnd, msg, wp, lp),
        }
        LRESULT(0)
    }
}

fn paint(hwnd: HWND) {
    unsafe {
        let mut paint = PAINTSTRUCT::default();
        let dc = BeginPaint(hwnd, &mut paint);
        SELECTION.with(|cell| {
            let state = cell.borrow();
            let Some(frame) = &state.frame else {
                return;
            };
            let dirty = paint.rcPaint;
            let _ = FillRect(frame.dc, &dirty, HBRUSH(GetStockObject(BLACK_BRUSH).0));
            if let Some(rect) = state.client_selection() {
                let mut inside = RECT::default();
                if IntersectRect(&mut inside, &dirty, &rect).as_bool() {
                    let _ = FillRect(frame.dc, &inside, frame.interior);
                    let _ = FrameRect(frame.dc, &rect, frame.outline);
                }
            }
            // Publish the composed pixels in one operation. The layered window
            // never displays the intermediate paint that dims the entire selection.
            let _ = BitBlt(
                dc,
                dirty.left,
                dirty.top,
                dirty.right - dirty.left,
                dirty.bottom - dirty.top,
                Some(frame.dc),
                dirty.left,
                dirty.top,
                SRCCOPY,
            );
        });
        let _ = EndPaint(hwnd, &paint);
    }
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
    #[ignore = "opens the live Windows desktop overlay"]
    fn native_selector_drag_and_escape() {
        use std::time::{Duration, Instant};
        use windows::core::PCWSTR;

        fn run(messages: &[(u32, WPARAM, LPARAM)]) -> Option<Region> {
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

        assert_eq!(
            run(&[(WM_KEYDOWN, WPARAM(usize::from(VK_ESCAPE.0)), LPARAM(0))]),
            None
        );
        let region = run(&[
            (WM_LBUTTONDOWN, WPARAM(1), point(190, 160)),
            (WM_MOUSEMOVE, WPARAM(1), point(150, 130)),
            (WM_LBUTTONUP, WPARAM(0), point(60, 50)),
        ]);
        let (_, _, width, height) = region.expect("drag returns a desktop region");
        assert_eq!((width, height), (130, 110));
    }
}
