//! DWM 直接合成实时画面，不轮询截图、不向 WebView 传输视频帧。
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, OnceLock, mpsc};

use anyhow::{Result, ensure};
use serde::Serialize;
use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

use super::screen::Rect;

#[derive(Clone, Serialize)]
pub struct PipInfo { pub hwnd: isize, pub title: String, pub paused: bool }
static WINDOWS: LazyLock<Mutex<HashMap<isize, PipInfo>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static REGISTERED: OnceLock<()> = OnceLock::new();

pub fn list() -> Vec<PipInfo> { WINDOWS.lock().unwrap().values().cloned().collect() }
pub fn close(hwnd: isize) -> Result<()> {
    ensure!(WINDOWS.lock().unwrap().contains_key(&hwnd), "画中画窗口已关闭");
    unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0)) }?;
    Ok(())
}

struct Pip {
    source: HWND,
    thumbnail: isize,
    crop: RECT,
    paused: bool,
    topmost: bool,
    title: String,
    buttons: [HWND; 3],
}
impl Drop for Pip { fn drop(&mut self) { let _ = unsafe { DwmUnregisterThumbnail(self.thumbnail) }; } }

pub fn open(source: isize, selected: Rect) -> Result<isize> {
    ensure!(WINDOWS.lock().unwrap().len() < 12, "画中画数量已达 12 个，请先关闭部分窗口");
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::Builder::new().name("picture-in-picture".into()).spawn(move || {
        let result = create(source, selected);
        match result {
            Ok(hwnd) => {
                let _ = tx.send(Ok(hwnd.0 as isize));
                unsafe {
                    let mut msg = MSG::default();
                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            }
            Err(e) => { let _ = tx.send(Err(format!("{e:#}"))); }
        }
    })?;
    rx.recv()?.map_err(anyhow::Error::msg)
}

fn create(source: isize, selected: Rect) -> Result<HWND> {
    unsafe {
        let source = HWND(source as *mut _);
        ensure!(IsWindow(Some(source)).as_bool(), "源窗口已关闭，请重新选择");
        let mut source_rect = RECT::default();
        GetWindowRect(source, &mut source_rect)?;
        let bounds = Rect { x: source_rect.left, y: source_rect.top, width: (source_rect.right - source_rect.left).max(0) as u32, height: (source_rect.bottom - source_rect.top).max(0) as u32 };
        let crop = selected.intersection(bounds).ok_or_else(|| anyhow::anyhow!("选区没有落在目标窗口内"))?;
        let crop = RECT { left: crop.x - bounds.x, top: crop.y - bounds.y, right: crop.x - bounds.x + crop.width as i32, bottom: crop.y - bounds.y + crop.height as i32 };
        let instance = GetModuleHandleW(None)?;
        REGISTERED.get_or_init(|| {
            let cls = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: instance.into(), lpszClassName: w!("NextGesturesPictureInPicture"), hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(), hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0), ..Default::default() };
            RegisterClassW(&cls);
        });
        let mut text = [0u16; 512];
        let len = GetWindowTextW(source, &mut text).max(0) as usize;
        let title = String::from_utf16_lossy(&text[..len]);
        let width = 560;
        let height = ((crop.bottom - crop.top) as f64 / (crop.right - crop.left) as f64 * 540.0).clamp(120.0, 600.0) as i32 + 90;
        let hwnd = CreateWindowExW(WS_EX_TOPMOST | WS_EX_TOOLWINDOW, w!("NextGesturesPictureInPicture"), w!("画中画 · NextGestures"), WS_OVERLAPPEDWINDOW, CW_USEDEFAULT, CW_USEDEFAULT, width, height, None, None, Some(instance.into()), None)?;
        let thumbnail = match DwmRegisterThumbnail(hwnd, source) {
            Ok(value) => value,
            Err(e) => { let _ = DestroyWindow(hwnd); return Err(e.into()); }
        };
        let mut buttons = [HWND::default(); 3];
        for (i, label) in [w!("暂停"), w!("取消置顶"), w!("关闭")].into_iter().enumerate() {
            buttons[i] = CreateWindowExW(WINDOW_EX_STYLE(0), w!("BUTTON"), label, WS_CHILD | WS_VISIBLE, 0, 0, 110, 30, Some(hwnd), Some(HMENU((100 + i) as *mut _)), Some(instance.into()), None).unwrap_or_default();
            SendMessageW(buttons[i], WM_SETFONT, Some(WPARAM(GetStockObject(DEFAULT_GUI_FONT).0 as usize)), Some(LPARAM(1)));
        }
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(Box::new(Pip { source, thumbnail, crop, paused: false, topmost: true, title: title.clone(), buttons })) as isize);
        WINDOWS.lock().unwrap().insert(hwnd.0 as isize, PipInfo { hwnd: hwnd.0 as isize, title, paused: false });
        SetTimer(Some(hwnd), 1, 250, None);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        update(hwnd);
        Ok(hwnd)
    }
}

unsafe fn update(hwnd: HWND) {
    unsafe {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Pip;
        let Some(state) = ptr.as_mut() else { return };
        let mut client = RECT::default();
        if GetClientRect(hwnd, &mut client).is_err() { return; }
        let width = client.right.max(1);
        let height = (client.bottom - 42).max(1);
        let scale = (width as f64 / (state.crop.right - state.crop.left) as f64).min(height as f64 / (state.crop.bottom - state.crop.top) as f64);
        let dw = ((state.crop.right - state.crop.left) as f64 * scale) as i32;
        let dh = ((state.crop.bottom - state.crop.top) as f64 * scale) as i32;
        let minimized = IsIconic(state.source).as_bool();
        let props = DWM_THUMBNAIL_PROPERTIES { dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_RECTSOURCE | DWM_TNP_VISIBLE | DWM_TNP_OPACITY | DWM_TNP_SOURCECLIENTAREAONLY, rcDestination: RECT { left: (width-dw)/2, top: (height-dh)/2, right: (width+dw)/2, bottom: (height+dh)/2 }, rcSource: state.crop, opacity: 255, fVisible: (!minimized && !state.paused).into(), fSourceClientAreaOnly: false.into() };
        let _ = DwmUpdateThumbnailProperties(state.thumbnail, &props);
        if minimized || state.paused { let _ = InvalidateRect(Some(hwnd), None, false); }
        for (i, button) in state.buttons.iter().enumerate() { let _ = MoveWindow(*button, 8 + i as i32 * 116, height + 5, 110, 30, true); }
        let title = format!("画中画{} · {}", if minimized { "（源窗口已最小化）" } else if state.paused { "（已暂停）" } else { "" }, state.title);
        let wide: Vec<_> = title.encode_utf16().chain(Some(0)).collect();
        let _ = SetWindowTextW(hwnd, windows::core::PCWSTR(wide.as_ptr()));
        if let Some(info) = WINDOWS.lock().unwrap().get_mut(&(hwnd.0 as isize)) { info.paused = state.paused || minimized; }
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut paint);
                let mut rect = RECT::default();
                let _ = GetClientRect(hwnd, &mut rect);
                rect.bottom = (rect.bottom - 42).max(0);
                FillRect(dc, &rect, HBRUSH(GetStockObject(BLACK_BRUSH).0));
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Pip;
                if let Some(state) = ptr.as_ref() {
                    if state.paused || IsIconic(state.source).as_bool() {
                        SetTextColor(dc, windows::Win32::Foundation::COLORREF(0xffffff));
                        SetBkMode(dc, TRANSPARENT);
                        let mut text: Vec<u16> = if state.paused { "画中画已暂停" } else { "源窗口已最小化，恢复后继续显示" }.encode_utf16().collect();
                        DrawTextW(dc, &mut text, &mut rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                    }
                }
                let _ = EndPaint(hwnd, &paint);
                return LRESULT(0);
            }
            WM_SIZE => { update(hwnd); return LRESULT(0); }
            WM_TIMER => {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Pip;
                if let Some(state) = ptr.as_ref() {
                    if !IsWindow(Some(state.source)).as_bool() { let _ = DestroyWindow(hwnd); }
                    else { update(hwnd); }
                }
                return LRESULT(0);
            }
            WM_COMMAND => {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Pip;
                if let Some(state) = ptr.as_mut() {
                    match wp.0 & 0xffff {
                        100 => { state.paused = !state.paused; let _ = SetWindowTextW(state.buttons[0], if state.paused { w!("继续") } else { w!("暂停") }); }
                        101 => {
                            state.topmost = !state.topmost;
                            let _ = SetWindowPos(hwnd, Some(if state.topmost { HWND_TOPMOST } else { HWND_NOTOPMOST }), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                            let _ = SetWindowTextW(state.buttons[1], if state.topmost { w!("取消置顶") } else { w!("置顶") });
                        }
                        102 => { let _ = DestroyWindow(hwnd); return LRESULT(0); }
                        _ => {}
                    }
                }
                update(hwnd);
                return LRESULT(0);
            }
            WM_DESTROY => { PostQuitMessage(0); return LRESULT(0); }
            WM_NCDESTROY => {
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Pip;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                if !ptr.is_null() { drop(Box::from_raw(ptr)); }
                WINDOWS.lock().unwrap().remove(&(hwnd.0 as isize));
            }
            _ => {}
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}
