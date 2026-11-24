use anyhow::{Context, Result, bail, ensure};
use gesture_platform::{
    RecordingButton, draw_recording_button, paint_toolbar_surface, release_toolbar_frame,
    set_toolbar_rounding, show_toolbar_frame,
};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};
use tauri::AppHandle;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows::Win32::System::Com::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::DRAWITEMSTRUCT;
use windows::Win32::UI::Controls::Dialogs::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, VK_ESCAPE};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Interface, PCWSTR, PWSTR, w};

static ACTIVE: AtomicBool = AtomicBool::new(false);
const DEFAULT_FPS: u32 = 30;
const FRAME_RATES: [u32; 5] = [15, 24, 30, 60, 120];
const START_STOP: usize = 100;
const CANCEL: usize = 101;
const SETTINGS: usize = 102;
const FINISH: u32 = WM_APP + 1;
const HOVER_TIMER: usize = 1;

#[derive(Clone, Copy)]
struct Region {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Quality {
    Hd,
    FullHd,
    #[default]
    Original,
}

impl Quality {
    const ALL: [Self; 3] = [Self::Hd, Self::FullHd, Self::Original];

    fn label(self) -> &'static str {
        match self {
            Self::Hd => "720p",
            Self::FullHd => "1080p",
            Self::Original => "原始",
        }
    }

    fn dimensions(self, region: Region) -> (u32, u32) {
        // Cap both axes without stretching or upscaling. Rotate the limits for portrait.
        let (mut max_width, mut max_height) = match self {
            Self::Hd => (1280, 720),
            Self::FullHd => (1920, 1080),
            Self::Original => (4096, 4096),
        };
        if region.height > region.width {
            std::mem::swap(&mut max_width, &mut max_height);
        }
        let (width, height) = (region.width as f64, region.height as f64);
        let scale = (f64::from(max_width) / width)
            .min(f64::from(max_height) / height)
            .min(1.0);
        (
            ((width * scale).round() as u32 & !1).max(2),
            ((height * scale).round() as u32 & !1).max(2),
        )
    }

    fn bitrate(self, width: u32, height: u32, fps: u32) -> u32 {
        let divisor = match self {
            Self::Hd => 10,
            Self::FullHd => 7,
            Self::Original => 5,
        };
        (u64::from(width) * u64::from(height) * u64::from(fps) / divisor).clamp(750_000, 60_000_000)
            as u32
    }
}

pub fn spawn(app: AppHandle) {
    if ACTIVE.swap(true, Ordering::AcqRel) {
        crate::engine::toast(&app, "已有一个录屏任务");
        return;
    }
    let failure_app = app.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("region-recorder-ui".into())
        .spawn(move || {
            struct ResetActive;
            impl Drop for ResetActive {
                fn drop(&mut self) {
                    ACTIVE.store(false, Ordering::Release);
                }
            }
            let _active = ResetActive;
            let result = gesture_platform::select_region()
                .map_err(anyhow::Error::msg)
                .and_then(|selected| match selected {
                    Some((x, y, width, height)) => show_controls(
                        &app,
                        Region {
                            x,
                            y,
                            width,
                            height,
                        },
                    ),
                    None => Ok(()),
                });
            if let Err(error) = result {
                report(&app, error);
            }
        })
    {
        ACTIVE.store(false, Ordering::Release);
        report(&failure_app, error.into());
    }
}

fn report(app: &AppHandle, error: anyhow::Error) {
    log::error!("录屏失败：{error:#}");
    crate::engine::toast(app, &format!("录屏失败：{error:#}"));
}

struct Window(HWND);
impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            if IsWindow(Some(self.0)).as_bool() {
                let _ = DestroyWindow(self.0);
            }
        }
    }
}

struct WindowState(HWND);
impl Drop for WindowState {
    fn drop(&mut self) {
        unsafe { SetWindowLongPtrW(self.0, GWLP_USERDATA, 0) };
    }
}

enum Command {
    Stop,
    Cancel,
}

struct Job {
    command: Sender<Command>,
    result: Receiver<Result<tempfile::TempDir>>,
    finishing: bool,
    cancelled: bool,
}

struct Controls {
    size: Region,
    app: AppHandle,
    button: HWND,
    settings: HWND,
    fps: u32,
    quality: Quality,
    border: Window,
    job: Option<Job>,
}

fn show_controls(app: &AppHandle, region: Region) -> Result<()> {
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance = GetModuleHandleW(None)?.into();
        let class = w!("NextGesturesRecordingControls");
        let border_class = w!("NextGesturesRecordingBorder");
        RegisterClassW(&WNDCLASSW {
            hInstance: instance,
            lpszClassName: class,
            lpfnWndProc: Some(controls_proc),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        });
        RegisterClassW(&WNDCLASSW {
            hInstance: instance,
            lpszClassName: border_class,
            lpfnWndProc: Some(border_proc),
            ..Default::default()
        });
        let selected = RECT {
            left: region.x,
            top: region.y,
            right: region.x + region.width,
            bottom: region.y + region.height,
        };
        let monitor = MonitorFromRect(&selected, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        ensure!(
            GetMonitorInfoW(monitor, &mut info).as_bool(),
            "读取屏幕工作区失败"
        );
        let work = info.rcWork;
        let (mut dpi_x, mut dpi_y) = (GetDpiForSystem(), GetDpiForSystem());
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let scale_dpi = dpi_x as i32;
        let scale = |value: i32| (value * scale_dpi + 48) / 96;
        let (bar_w, bar_h, gap) = (scale(290), scale(42), scale(8));
        let (x, y) = toolbar_position(selected, work, bar_w, bar_h, gap);
        let toolbar = Window(CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_CONTROLPARENT,
            class,
            w!("区域录屏"),
            WS_POPUP | WS_CLIPCHILDREN,
            x,
            y,
            bar_w,
            bar_h,
            None,
            None,
            Some(instance),
            None,
        )?);
        // The bar can overlap a selection that fills the work area. Exclude it from capture
        // where Windows supports it. The outline's window region never overlaps the selection.
        let _ = SetWindowDisplayAffinity(toolbar.0, WDA_EXCLUDEFROMCAPTURE);
        set_toolbar_rounding(toolbar.0, bar_w, bar_h, f64::from(dpi_x) / 96.0, true);
        let button_style = WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32);
        let button = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("BUTTON"),
            w!("开始录制"),
            button_style,
            scale(8),
            scale(5),
            scale(92),
            scale(30),
            Some(toolbar.0),
            Some(HMENU(START_STOP as _)),
            Some(instance),
            None,
        )?;
        let cancel = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("BUTTON"),
            w!("取消"),
            button_style,
            scale(252),
            scale(5),
            scale(30),
            scale(30),
            Some(toolbar.0),
            Some(HMENU(CANCEL as _)),
            Some(instance),
            None,
        )?;
        let settings = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("BUTTON"),
            w!("原始 · 30 FPS"),
            button_style,
            scale(108),
            scale(5),
            scale(142),
            scale(30),
            Some(toolbar.0),
            Some(HMENU(SETTINGS as _)),
            Some(instance),
            None,
        )?;
        let line = scale(3).max(2);
        let outline_w = region.width + line * 2;
        let outline_h = region.height + line * 2;
        let border = Window(CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
            border_class,
            w!(""),
            WS_POPUP,
            region.x - line,
            region.y - line,
            outline_w,
            outline_h,
            None,
            None,
            Some(instance),
            None,
        )?);
        // An outside stroke falls off the desktop when a selection touches an edge.
        // Move the affected side inside the visible screen when capture can exclude it.
        let excluded = SetWindowDisplayAffinity(border.0, WDA_EXCLUDEFROMCAPTURE).is_ok();
        let monitor_bounds = info.rcMonitor;
        let mut intersect = RECT::default();
        let _ = IntersectRect(&mut intersect, &selected, &monitor_bounds);
        let bounds = if intersect == selected {
            monitor_bounds
        } else {
            let left = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let top = GetSystemMetrics(SM_YVIRTUALSCREEN);
            RECT {
                left,
                top,
                right: left + GetSystemMetrics(SM_CXVIRTUALSCREEN),
                bottom: top + GetSystemMetrics(SM_CYVIRTUALSCREEN),
            }
        };
        let (left, top, right, bottom) = if excluded {
            (
                if selected.left - line < bounds.left {
                    line
                } else {
                    0
                },
                if selected.top - line < bounds.top {
                    line
                } else {
                    0
                },
                if selected.right + line > bounds.right {
                    outline_w - line
                } else {
                    outline_w
                },
                if selected.bottom + line > bounds.bottom {
                    outline_h - line
                } else {
                    outline_h
                },
            )
        } else {
            (0, 0, outline_w, outline_h)
        };
        let outer = CreateRectRgn(left, top, right, bottom);
        let inner = CreateRectRgn(left + line, top + line, right - line, bottom - line);
        let combined = CombineRgn(Some(outer), Some(outer), Some(inner), RGN_DIFF);
        let _ = DeleteObject(inner.into());
        if combined == RGN_ERROR || SetWindowRgn(border.0, Some(outer), false) == 0 {
            let _ = DeleteObject(outer.into());
            bail!("绘制录制边框失败");
        }
        let mut controls = Controls {
            size: region,
            app: app.clone(),
            button,
            settings,
            fps: DEFAULT_FPS,
            quality: Quality::default(),
            border,
            job: None,
        };
        // Keep mutable state in this stack frame, and only lend it to the message procedure
        // during each call. Dialogs run outside the procedure, avoiding reentrant state access.
        SetWindowLongPtrW(
            toolbar.0,
            GWLP_USERDATA,
            (&mut controls as *mut Controls) as isize,
        );
        let _state = WindowState(toolbar.0);
        let _ = ShowWindow(controls.border.0, SW_SHOWNOACTIVATE);
        let _ = ShowWindow(toolbar.0, SW_SHOW);
        let _ = SetForegroundWindow(toolbar.0);
        let _ = SetTimer(Some(toolbar.0), HOVER_TIMER, 60, None);
        let mut hot = HWND::default();
        let mut message = MSG::default();
        loop {
            match GetMessageW(&mut message, None, 0, 0).0 {
                -1 => bail!("录屏控制窗口消息循环失败"),
                0 => break,
                _ => {}
            }
            if message.message == FINISH {
                if let Some(job) = controls.job.take() {
                    let _ = ShowWindow(controls.border.0, SW_HIDE);
                    let _ = ShowWindow(toolbar.0, SW_HIDE);
                    let result = job.result.recv().context("录屏线程异常退出")?;
                    if !job.cancelled {
                        export_recording(app, toolbar.0, result?)?;
                    }
                }
                break;
            }
            if message.hwnd == toolbar.0
                && message.message == WM_TIMER
                && message.wParam.0 == HOVER_TIMER
            {
                let mut cursor = POINT::default();
                let next = if GetCursorPos(&mut cursor).is_ok() {
                    WindowFromPoint(cursor)
                } else {
                    HWND::default()
                };
                if hot != next {
                    for child in [button, settings, cancel] {
                        if child == hot || child == next {
                            let _ = InvalidateRect(Some(child), None, false);
                        }
                    }
                    hot = next;
                }
                continue;
            }
            if message.message == WM_KEYDOWN && message.wParam.0 == VK_ESCAPE.0 as usize {
                let _ = PostMessageW(Some(toolbar.0), WM_CLOSE, WPARAM(0), LPARAM(0));
            } else if !IsDialogMessageW(toolbar.0, &message).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        Ok(())
    }
}

fn toolbar_position(region: RECT, work: RECT, width: i32, height: i32, gap: i32) -> (i32, i32) {
    let clamp_x = |x: i32| x.clamp(work.left, (work.right - width).max(work.left));
    let clamp_y = |y: i32| y.clamp(work.top, (work.bottom - height).max(work.top));
    let x = clamp_x(region.left + (region.right - region.left - width) / 2);
    if region.bottom + gap + height <= work.bottom {
        (x, clamp_y(region.bottom + gap))
    } else if region.top - gap - height >= work.top {
        (x, clamp_y(region.top - gap - height))
    } else if region.right + gap + width <= work.right {
        (clamp_x(region.right + gap), clamp_y(region.bottom - height))
    } else if region.left - gap - width >= work.left {
        (
            clamp_x(region.left - gap - width),
            clamp_y(region.bottom - height),
        )
    } else {
        (x, clamp_y(region.bottom - height - gap))
    }
}

unsafe extern "system" fn border_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut paint);
                let color = if GetWindowLongPtrW(hwnd, GWLP_USERDATA) == 0 {
                    0x00e88329
                } else {
                    0x002e36ef
                };
                let brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(color));
                FillRect(dc, &paint.rcPaint, brush);
                let _ = DeleteObject(brush.into());
                let _ = EndPaint(hwnd, &paint);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

unsafe extern "system" fn controls_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_ERASEBKGND => return LRESULT(1),
            WM_SHOWWINDOW => show_toolbar_frame(hwnd, wp.0 != 0),
            WM_DESTROY => release_toolbar_frame(hwnd),
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut paint);
                let mut rect = RECT::default();
                let _ = GetClientRect(hwnd, &mut rect);
                paint_toolbar_surface(
                    hwnd,
                    dc,
                    &rect,
                    f64::from(GetDpiForWindow(hwnd).max(96)) / 96.0,
                );
                let _ = EndPaint(hwnd, &paint);
                return LRESULT(0);
            }
            WM_DRAWITEM if lp.0 != 0 => {
                // Drawing is reentrant during SetWindowText; read the button only.
                let item = &*(lp.0 as *const DRAWITEMSTRUCT);
                let kind = match item.CtlID as usize {
                    START_STOP if GetWindowLongPtrW(item.hwndItem, GWLP_USERDATA) != 0 => {
                        RecordingButton::Stop
                    }
                    START_STOP => RecordingButton::Start,
                    SETTINGS => RecordingButton::Settings,
                    CANCEL => RecordingButton::Close,
                    _ => return DefWindowProcW(hwnd, msg, wp, lp),
                };
                let mut caption = [0u16; 64];
                let length = if matches!(kind, RecordingButton::Close) {
                    0
                } else {
                    GetWindowTextW(item.hwndItem, &mut caption) as usize
                };
                let label = String::from_utf16_lossy(&caption[..length]);
                let mut cursor = POINT::default();
                let hot =
                    GetCursorPos(&mut cursor).is_ok() && WindowFromPoint(cursor) == item.hwndItem;
                let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
                draw_recording_button(item, scale, kind, &label, hot);
                return LRESULT(1);
            }
            _ => {}
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Controls;
        // Only command and close messages access state. APIs below can reenter the procedure
        // with paint/activation messages, which are handled without that state.
        if !ptr.is_null() && matches!(msg, WM_COMMAND | WM_CLOSE) {
            let controls = &mut *ptr;
            match (msg, wp.0 & 0xffff) {
                (WM_COMMAND, START_STOP) => {
                    if let Some(job) = &mut controls.job {
                        if !job.finishing {
                            job.finishing = true;
                            let _ = job.command.send(Command::Stop);
                            let _ = EnableWindow(controls.button, false);
                            let _ = SetWindowTextW(controls.button, w!("正在保存…"));
                            let _ = ShowWindow(controls.border.0, SW_HIDE);
                        }
                    } else {
                        SetWindowLongPtrW(controls.border.0, GWLP_USERDATA, 1);
                        let _ = RedrawWindow(
                            Some(controls.border.0),
                            None,
                            None,
                            RDW_INVALIDATE | RDW_UPDATENOW,
                        );
                        let _ = DwmFlush();
                        let (send_command, command) = mpsc::channel();
                        let (send_result, result) = mpsc::channel();
                        let handle = hwnd.0 as usize;
                        let size = controls.size;
                        let fps = controls.fps;
                        let quality = controls.quality;
                        match std::thread::Builder::new()
                            .name("region-recorder-encoder".into())
                            .spawn(move || {
                                let result = record(size, quality, fps, command);
                                let _ = send_result.send(result);
                                let _ = PostMessageW(
                                    Some(HWND(handle as _)),
                                    FINISH,
                                    WPARAM(0),
                                    LPARAM(0),
                                );
                            }) {
                            Ok(_) => {
                                controls.job = Some(Job {
                                    command: send_command,
                                    result,
                                    finishing: false,
                                    cancelled: false,
                                });
                                let _ = EnableWindow(controls.settings, false);
                                SetWindowLongPtrW(controls.button, GWLP_USERDATA, 1);
                                let _ = SetWindowTextW(controls.button, w!("停止录制"));
                            }
                            Err(error) => {
                                SetWindowLongPtrW(controls.border.0, GWLP_USERDATA, 0);
                                let _ = RedrawWindow(
                                    Some(controls.border.0),
                                    None,
                                    None,
                                    RDW_INVALIDATE | RDW_UPDATENOW,
                                );
                                report(&controls.app, error.into());
                            }
                        }
                    }
                    return LRESULT(0);
                }
                (WM_COMMAND, SETTINGS) if controls.job.is_none() => {
                    let settings = controls.settings;
                    let fps = controls.fps;
                    let quality = controls.quality;
                    // The menu runs a nested message loop. Release the state borrow before
                    // entering it and reacquire it only after TrackPopupMenu returns.
                    if let Some(choice) = choose_settings(hwnd, settings, fps, quality) {
                        let controls = &mut *ptr;
                        match choice {
                            Setting::Rate(fps) => controls.fps = fps,
                            Setting::Quality(quality) => controls.quality = quality,
                        }
                        let caption: Vec<u16> =
                            format!("{} · {} FPS", controls.quality.label(), controls.fps)
                                .encode_utf16()
                                .chain(Some(0))
                                .collect();
                        let _ = SetWindowTextW(settings, PCWSTR(caption.as_ptr()));
                    }
                    return LRESULT(0);
                }
                (WM_COMMAND, CANCEL) | (WM_CLOSE, _) => {
                    let _ = ShowWindow(controls.border.0, SW_HIDE);
                    let _ = ShowWindow(hwnd, SW_HIDE);
                    if let Some(job) = &mut controls.job {
                        job.cancelled = true;
                        let _ = job.command.send(Command::Cancel);
                    } else {
                        PostQuitMessage(0);
                    }
                    return LRESULT(0);
                }
                _ => {}
            }
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}

enum Setting {
    Rate(u32),
    Quality(Quality),
}

unsafe fn choose_settings(
    hwnd: HWND,
    settings: HWND,
    fps: u32,
    quality: Quality,
) -> Option<Setting> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let mut bounds = RECT::default();
        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, w!("清晰度"));
        let quality_labels = [w!("720p 流畅"), w!("1080p 高清"), w!("原始分辨率")];
        for (index, (&option, label)) in Quality::ALL.iter().zip(quality_labels).enumerate() {
            let checked = if option == quality {
                MF_CHECKED
            } else {
                MF_UNCHECKED
            };
            let _ = AppendMenuW(menu, MF_STRING | checked, 200 + index, label);
        }
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, w!("帧率"));
        for rate in FRAME_RATES {
            let checked = if rate == fps {
                MF_CHECKED
            } else {
                MF_UNCHECKED
            };
            let label: Vec<u16> = format!("{rate} FPS")
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let _ = AppendMenuW(
                menu,
                MF_STRING | checked,
                rate as usize,
                PCWSTR(label.as_ptr()),
            );
        }
        let result = if GetWindowRect(settings, &mut bounds).is_ok() {
            TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY | TPM_WORKAREA,
                bounds.left,
                bounds.bottom,
                Some(0),
                hwnd,
                None,
            )
            .0 as u32
        } else {
            0
        };
        let _ = DestroyMenu(menu);
        if FRAME_RATES.contains(&result) {
            Some(Setting::Rate(result))
        } else {
            Quality::ALL
                .get(result.checked_sub(200)? as usize)
                .copied()
                .map(Setting::Quality)
        }
    }
}

fn save_dialog(owner: HWND) -> Result<Option<PathBuf>> {
    unsafe {
        let mut file = vec![0u16; 32_768];
        let now = windows::Win32::System::SystemInformation::GetLocalTime();
        let filename = format!(
            "{:04}-{:02}-{:02} {:02}-{:02}-{:02}",
            now.wYear, now.wMonth, now.wDay, now.wHour, now.wMinute, now.wSecond
        );
        for (slot, unit) in file.iter_mut().zip(filename.encode_utf16()) {
            *slot = unit;
        }
        let mut dialog = OPENFILENAMEW {
            lStructSize: size_of::<OPENFILENAMEW>() as u32,
            hwndOwner: owner,
            lpstrFilter: w!("MP4 视频 (*.mp4)\0*.mp4\0GIF 动图 (*.gif)\0*.gif\0"),
            nFilterIndex: 1,
            lpstrFile: PWSTR(file.as_mut_ptr()),
            nMaxFile: file.len() as u32,
            lpstrTitle: w!("保存录屏"),
            Flags: OFN_EXPLORER
                | OFN_NOCHANGEDIR
                | OFN_OVERWRITEPROMPT
                | OFN_PATHMUSTEXIST
                | OFN_ENABLEHOOK,
            lpfnHook: Some(save_dialog_hook),
            lpstrDefExt: w!("mp4"),
            ..Default::default()
        };
        if GetSaveFileNameW(&mut dialog).as_bool() {
            let end = file
                .iter()
                .position(|&character| character == 0)
                .unwrap_or(file.len());
            let path = PathBuf::from(std::ffi::OsString::from_wide(&file[..end]));
            ensure!(
                path.extension().is_some_and(
                    |ext| ext.eq_ignore_ascii_case("mp4") || ext.eq_ignore_ascii_case("gif")
                ),
                "请使用 .mp4 或 .gif 文件扩展名"
            );
            Ok(Some(path))
        } else {
            let code = CommDlgExtendedError().0;
            ensure!(code == 0, "打开保存窗口失败（0x{code:04x}）");
            Ok(None)
        }
    }
}

unsafe extern "system" fn save_dialog_hook(
    hwnd: HWND,
    message: u32,
    _: WPARAM,
    data: LPARAM,
) -> usize {
    if message != WM_NOTIFY || data.0 == 0 {
        return 0;
    }
    unsafe {
        let notify = &*(data.0 as *const OFNOTIFYW);
        if notify.hdr.code == CDN_TYPECHANGE && !notify.lpOFN.is_null() {
            if let Ok(dialog) = GetParent(hwnd) {
                let ext = if (*notify.lpOFN).nFilterIndex == 2 {
                    w!("gif")
                } else {
                    w!("mp4")
                };
                SendMessageW(
                    dialog,
                    CDM_SETDEFEXT,
                    None,
                    Some(LPARAM(ext.as_ptr() as isize)),
                );
            }
        }
    }
    0
}

fn export_recording(app: &AppHandle, owner: HWND, video: tempfile::TempDir) -> Result<()> {
    let source = video.path().join("recording.mp4");
    let result = (|| {
        let Some(path) = save_dialog(owner)? else {
            return Ok(None);
        };
        let copy_from = source.clone();
        let copy_to = path.clone();
        std::thread::Builder::new()
            .name("region-recorder-export".into())
            .spawn(move || save_recording(&copy_from, &copy_to))?
            .join()
            .map_err(|_| anyhow::anyhow!("录屏导出线程异常退出"))??;
        Ok::<_, anyhow::Error>(Some(path))
    })();
    match result {
        Ok(Some(path)) => {
            crate::engine::toast(app, &format!("录屏已保存：{}", path.display()));
            Ok(())
        }
        Ok(None) => Ok(()),
        Err(error) => {
            // Preserve the finalized MP4 when export fails, including when the GIF encoder
            // or the selected drive rejects the output.
            let _retained = video.keep();
            let details = format!("{error:#}\n\n原始 MP4 保存在：\n{}", source.display());
            let details: Vec<u16> = details.encode_utf16().chain(Some(0)).collect();
            unsafe {
                MessageBoxW(
                    Some(owner),
                    PCWSTR(details.as_ptr()),
                    w!("保存录屏失败"),
                    MB_OK | MB_ICONERROR,
                );
            }
            Err(error)
        }
    }
}

fn save_recording(source: &Path, destination: &Path) -> Result<()> {
    let folder = destination.parent().context("找不到 MP4 的保存目录")?;
    let mut staged = tempfile::NamedTempFile::new_in(folder).context("创建临时输出文件失败")?;
    if destination
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("gif"))
    {
        export_gif(source, staged.as_file_mut()).context("导出 GIF 失败")?;
    } else {
        std::io::copy(&mut std::fs::File::open(source)?, &mut staged).context("写入 MP4 失败")?;
    }
    staged.as_file().sync_all()?;
    staged.persist(destination).context("保存录屏失败")?;
    Ok(())
}

fn export_gif(source: &Path, destination: &mut std::fs::File) -> Result<()> {
    let _media = MediaFoundation::start()?;
    let url: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let reader = MFCreateSourceReaderFromURL(PCWSTR(url.as_ptr()), None::<&IMFAttributes>)?;
        let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        let output = MFCreateMediaType()?;
        output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        output.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
        reader.SetCurrentMediaType(stream, None, &output)?;
        let format = reader.GetCurrentMediaType(stream)?;
        let size = format.GetUINT64(&MF_MT_FRAME_SIZE)?;
        let (width, height) = ((size >> 32) as usize, (size & 0xffff_ffff) as usize);
        ensure!(
            width > 0 && height > 0 && width % 2 == 0 && height % 2 == 0,
            "GIF 视频源尺寸无效"
        );
        let stride = format
            .GetUINT32(&MF_MT_DEFAULT_STRIDE)
            .unwrap_or(width as u32) as usize;
        // Export GIF as a compact preview. MP4 retains the full capture size and FPS.
        let ratio = (854.0 / width as f64).min(480.0 / height as f64).min(1.0);
        let out_width = (width as f64 * ratio).round().max(1.0) as usize;
        let out_height = (height as f64 * ratio).round().max(1.0) as usize;
        let mut gif = gif::Encoder::new(destination, out_width as u16, out_height as u16, &[])?;
        gif.set_repeat(gif::Repeat::Infinite)?;
        let mut packed = Vec::new();
        let mut rgb = vec![0; out_width * out_height * 3];
        let mut pending: Option<(i64, gif::Frame<'static>)> = None;
        let mut end = 0;
        let mut next_gif_time = 0;
        const GIF_PERIOD: i64 = 1_000_000;
        loop {
            let mut sample = None;
            let mut time = 0;
            let mut flags = 0;
            reader.ReadSample(
                stream,
                0,
                None,
                Some(&mut flags),
                Some(&mut time),
                Some(&mut sample),
            )?;
            ensure!(
                flags & MF_SOURCE_READERF_ERROR.0 as u32 == 0,
                "解码 GIF 视频源失败"
            );
            if let Some(sample) = sample {
                end = time + sample.GetSampleDuration().unwrap_or(GIF_PERIOD);
                if time >= next_gif_time {
                    next_gif_time = (time / GIF_PERIOD + 1) * GIF_PERIOD;
                    let buffer = sample.ConvertToContiguousBuffer()?;
                    if let Ok(surface) = buffer.cast::<IMF2DBuffer>() {
                        let length = surface.GetContiguousLength()? as usize;
                        let (pitch, plane_height) = nv12_layout(width, height, width, length)
                            .context("GIF 视频源像素布局无效")?;
                        packed.resize(length, 0);
                        surface.ContiguousCopyTo(&mut packed)?;
                        nv12_to_gif_rgb(
                            &packed,
                            (width, height),
                            (pitch, plane_height),
                            &mut rgb,
                            (out_width, out_height),
                        );
                    } else {
                        let mut address = std::ptr::null_mut();
                        let mut length = 0;
                        buffer.Lock(&mut address, None, Some(&mut length))?;
                        let length = length as usize;
                        if let Some(layout) = nv12_layout(width, height, stride, length) {
                            let source = std::slice::from_raw_parts(address, length);
                            nv12_to_gif_rgb(
                                source,
                                (width, height),
                                layout,
                                &mut rgb,
                                (out_width, out_height),
                            );
                            buffer.Unlock()?;
                        } else {
                            buffer.Unlock()?;
                            bail!("GIF 视频源像素布局无效");
                        }
                    }
                    if let Some((start, mut frame)) = pending.take() {
                        frame.delay = gif_delay(time - start);
                        gif.write_frame(&frame)?;
                    }
                    pending = Some((
                        time,
                        gif::Frame::from_rgb_speed(out_width as u16, out_height as u16, &rgb, 15),
                    ));
                }
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
        }
        let (start, mut final_frame) = pending.context("录屏没有可导出的 GIF 画面")?;
        final_frame.delay = gif_delay(end - start);
        gif.write_frame(&final_frame)?;
        // Finish the trailer explicitly so a write error does not get dropped.
        gif.into_inner()?;
    }
    Ok(())
}

fn gif_delay(duration_100ns: i64) -> u16 {
    ((duration_100ns + 50_000) / 100_000).clamp(2, i64::from(u16::MAX)) as u16
}

fn nv12_layout(
    width: usize,
    height: usize,
    stride: usize,
    length: usize,
) -> Option<(usize, usize)> {
    // H.264 packs in 16×16 macroblocks. The decoded NV12 surface may still have that
    // coded size even when the stream reports the smaller, visible frame.
    let (coded_width, coded_height) = (width.next_multiple_of(16), height.next_multiple_of(16));
    [
        (width, height),
        (coded_width, coded_height),
        (stride, height),
        (stride, coded_height),
    ]
    .into_iter()
    .find(|&(pitch, rows)| pitch >= width && pitch * rows * 3 / 2 == length)
}

fn nv12_to_gif_rgb(
    source: &[u8],
    visible: (usize, usize),
    layout: (usize, usize),
    rgb: &mut [u8],
    output: (usize, usize),
) {
    let (width, height) = visible;
    let (out_width, out_height) = output;
    let (stride, plane_height) = layout;
    let (luma, chroma) = source.split_at(stride * plane_height);
    for row in 0..out_height {
        for column in 0..out_width {
            let x = column * width / out_width;
            let y = row * height / out_height;
            let uv = y / 2 * stride + (x & !1);
            let c = (i32::from(luma[y * stride + x]) - 16).max(0) * 298;
            let d = i32::from(chroma[uv]) - 128;
            let e = i32::from(chroma[uv + 1]) - 128;
            let clamp = |v: i32| ((v + 128) >> 8).clamp(0, 255) as u8;
            let index = (row * out_width + column) * 3;
            rgb[index] = clamp(c + 409 * e);
            rgb[index + 1] = clamp(c - 100 * d - 208 * e);
            rgb[index + 2] = clamp(c + 516 * d);
        }
    }
}

struct MediaFoundation;
impl MediaFoundation {
    fn start() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .context("初始化录屏线程失败")?;
            if let Err(error) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
                CoUninitialize();
                return Err(error)
                    .context("启动 Windows Media Foundation 失败，请检查是否安装媒体功能包");
            }
        }
        Ok(Self)
    }
}
impl Drop for MediaFoundation {
    fn drop(&mut self) {
        unsafe {
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}

struct DesktopFrame {
    screen: HDC,
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *const u8,
    region: Region,
    width: i32,
    height: i32,
}
impl DesktopFrame {
    unsafe fn new(region: Region, width: u32, height: u32) -> Result<Self> {
        unsafe {
            let screen = GetDC(None);
            ensure!(!screen.is_invalid(), "读取桌面设备失败");
            let mut frame = Self {
                screen,
                dc: HDC::default(),
                bitmap: HBITMAP::default(),
                previous: HGDIOBJ::default(),
                bits: std::ptr::null(),
                region,
                width: width as i32,
                height: height as i32,
            };
            frame.dc = CreateCompatibleDC(Some(screen));
            ensure!(!frame.dc.is_invalid(), "创建录屏缓冲区失败");
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: frame.width,
                    biHeight: -frame.height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            frame.bitmap =
                CreateDIBSection(Some(frame.dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
            frame.bits = bits.cast();
            frame.previous = SelectObject(frame.dc, frame.bitmap.into());
            ensure!(
                !frame.previous.is_invalid() && !frame.bits.is_null(),
                "无法访问录屏像素缓冲区"
            );
            if frame.width != region.width || frame.height != region.height {
                SetStretchBltMode(frame.dc, COLORONCOLOR);
            }
            Ok(frame)
        }
    }

    fn next(&mut self) -> Result<&[u8]> {
        let r = self.region;
        unsafe {
            if self.width == r.width && self.height == r.height {
                BitBlt(
                    self.dc,
                    0,
                    0,
                    self.width,
                    self.height,
                    Some(self.screen),
                    r.x,
                    r.y,
                    SRCCOPY | CAPTUREBLT,
                )
                .context("桌面捕获失败")?;
            } else {
                StretchBlt(
                    self.dc,
                    0,
                    0,
                    self.width,
                    self.height,
                    Some(self.screen),
                    r.x,
                    r.y,
                    r.width,
                    r.height,
                    SRCCOPY | CAPTUREBLT,
                )
                .ok()
                .context("桌面缩放捕获失败")?;
            }
            let _ = GdiFlush();
            Ok(std::slice::from_raw_parts(
                self.bits,
                self.width as usize * self.height as usize * 4,
            ))
        }
    }
}
impl Drop for DesktopFrame {
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
            let _ = ReleaseDC(None, self.screen);
        }
    }
}

fn media_type(
    subtype: &windows::core::GUID,
    width: u32,
    height: u32,
    fps: u32,
) -> Result<IMFMediaType> {
    unsafe {
        let media = MFCreateMediaType()?;
        media.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        media.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        media.SetUINT64(
            &MF_MT_FRAME_SIZE,
            (u64::from(width) << 32) | u64::from(height),
        )?;
        media.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(fps) << 32) | 1)?;
        media.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1)?;
        media.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        Ok(media)
    }
}

fn create_writer(
    path: &Path,
    width: u32,
    height: u32,
    fps: u32,
    bitrate: u32,
    allow_hardware: bool,
) -> Result<(IMFSinkWriter, u32)> {
    let url: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let mut attributes = None;
        MFCreateAttributes(&mut attributes, 2)?;
        let attributes = attributes.context("创建编码器配置失败")?;
        attributes.SetUINT32(
            &MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS,
            u32::from(allow_hardware),
        )?;
        if allow_hardware {
            attributes.SetUINT32(&MF_LOW_LATENCY, 1)?;
        }
        let writer =
            MFCreateSinkWriterFromURL(PCWSTR(url.as_ptr()), None::<&IMFByteStream>, &attributes)
                .context("创建 MP4 编码器失败")?;
        let output = media_type(&MFVideoFormat_H264, width, height, fps)?;
        output.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
        let stream = writer
            .AddStream(&output)
            .context("Windows H.264 编码器拒绝此画面大小")?;
        let input = media_type(&MFVideoFormat_NV12, width, height, fps)?;
        input.SetUINT32(&MF_MT_DEFAULT_STRIDE, width)?;
        writer
            .SetInputMediaType(stream, &input, None::<&IMFAttributes>)
            .context("初始化 H.264 编码器失败，请检查 Windows 媒体功能")?;
        writer.BeginWriting().context("启动 MP4 编码失败")?;
        Ok((writer, stream))
    }
}

// Convert the capture's top-down BGRA pixels to the H.264 encoder's required NV12
// layout. BT.601 limited-range luma is full size. Chroma is averaged over each 2×2 block.
fn bgra_to_nv12(source: &[u8], destination: &mut [u8], width: usize, height: usize) {
    let (luma, chroma) = destination.split_at_mut(width * height);
    let luma_pixel = |r: i32, g: i32, b: i32| (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8;
    for ((source, luma), uv) in source
        .chunks_exact(width * 8)
        .zip(luma.chunks_exact_mut(width * 2))
        .zip(chroma.chunks_exact_mut(width))
    {
        let (top, bottom) = source.split_at(width * 4);
        let (luma_top, luma_bottom) = luma.split_at_mut(width);
        for ((((top, bottom), luma_top), luma_bottom), uv) in top
            .chunks_exact(8)
            .zip(bottom.chunks_exact(8))
            .zip(luma_top.chunks_exact_mut(2))
            .zip(luma_bottom.chunks_exact_mut(2))
            .zip(uv.chunks_exact_mut(2))
        {
            let [b0, g0, r0, _, b1, g1, r1, _] = top else {
                unreachable!()
            };
            let [b2, g2, r2, _, b3, g3, r3, _] = bottom else {
                unreachable!()
            };
            let [b0, g0, r0, b1, g1, r1, b2, g2, r2, b3, g3, r3] =
                [*b0, *g0, *r0, *b1, *g1, *r1, *b2, *g2, *r2, *b3, *g3, *r3].map(i32::from);
            luma_top[0] = luma_pixel(r0, g0, b0);
            luma_top[1] = luma_pixel(r1, g1, b1);
            luma_bottom[0] = luma_pixel(r2, g2, b2);
            luma_bottom[1] = luma_pixel(r3, g3, b3);
            let (r, g, b) = (r0 + r1 + r2 + r3, g0 + g1 + g2 + g3, b0 + b1 + b2 + b3);
            uv[0] = (((-38 * r - 74 * g + 112 * b + 512) >> 10) + 128) as u8;
            uv[1] = (((112 * r - 94 * g - 18 * b + 512) >> 10) + 128) as u8;
        }
    }
}

fn frame_sample(width: u32, height: u32, pixels: &[u8], time: i64) -> Result<IMFSample> {
    let (width, height) = (width as usize, height as usize);
    let length = width * height * 3 / 2;
    unsafe {
        let buffer = MFCreateMemoryBuffer(length as u32)?;
        let mut address = std::ptr::null_mut();
        buffer.Lock(&mut address, None, None)?;
        bgra_to_nv12(
            pixels,
            std::slice::from_raw_parts_mut(address, length),
            width,
            height,
        );
        buffer.Unlock()?;
        buffer.SetCurrentLength(length as u32)?;
        let sample = MFCreateSample()?;
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime(time)?;
        Ok(sample)
    }
}

fn submit_frame(
    writer: &IMFSinkWriter,
    stream: u32,
    frame: &IMFSample,
    duration: i64,
) -> Result<()> {
    unsafe {
        frame.SetSampleDuration(duration)?;
        writer
            .WriteSample(stream, frame)
            .context("写入视频帧失败")?;
    }
    Ok(())
}

struct TimerResolution(bool);
impl TimerResolution {
    fn for_rate(fps: u32) -> Self {
        Self(fps >= 60 && unsafe { timeBeginPeriod(1) } == 0)
    }
}
impl Drop for TimerResolution {
    fn drop(&mut self) {
        if self.0 {
            unsafe { timeEndPeriod(1) };
        }
    }
}

fn record(
    mut region: Region,
    quality: Quality,
    fps: u32,
    command: Receiver<Command>,
) -> Result<tempfile::TempDir> {
    // H.264 NV12 uses even dimensions. Leave at most one rightmost/bottom pixel unused.
    region.width &= !1;
    region.height &= !1;
    let (width, height) = quality.dimensions(region);
    ensure!(
        width >= 16 && height >= 16,
        "输出画面的宽高至少需要 16 像素，请扩大选区"
    );
    let directory = tempfile::Builder::new()
        .prefix("nextgestures-recording-")
        .tempdir()?;
    let _media = MediaFoundation::start()?;
    unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let mut capture = unsafe { DesktopFrame::new(region, width, height) }?;
    let path = directory.path().join("recording.mp4");
    let bitrate = quality.bitrate(width, height, fps);
    let (writer, stream) = match create_writer(&path, width, height, fps, bitrate, true) {
        Ok(writer) => writer,
        Err(error) => {
            log::debug!("录屏硬件编码初始化回退：{error:#}");
            if path.exists() {
                std::fs::remove_file(&path).context("清理上一次录屏编码初始化失败")?;
            }
            create_writer(&path, width, height, fps, bitrate, false).with_context(|| {
                format!("编码器启动失败（{width}×{height}，{fps} FPS），请降低清晰度或帧率")
            })?
        }
    };
    let _timer = TimerResolution::for_rate(fps);
    let fps = u64::from(fps);
    let at_frame = |index: u64| Duration::from_nanos(index * 1_000_000_000 / fps);
    let timestamp = |index| (at_frame(index).as_nanos() / 100) as i64;
    let clock = Instant::now();
    let mut index = 0;
    let mut sample = frame_sample(width, height, capture.next()?, 0)?;
    let mut frames = 1u64;
    loop {
        let until = clock + at_frame(index + 1);
        match command.recv_timeout(until.saturating_duration_since(Instant::now())) {
            Ok(Command::Stop) => {
                let end = (clock.elapsed().as_nanos() / 100) as i64;
                submit_frame(
                    &writer,
                    stream,
                    &sample,
                    end.max(timestamp(index + 1)) - timestamp(index),
                )?;
                break;
            }
            Ok(Command::Cancel) | Err(RecvTimeoutError::Disconnected) => bail!("录屏已取消"),
            Err(RecvTimeoutError::Timeout) => {}
        }
        // Advance on the original time grid. When capture or encoding overruns,
        // a longer sample holds the previous frame and preserves playback time.
        let elapsed = clock.elapsed().as_nanos();
        let next = ((elapsed * u128::from(fps) / 1_000_000_000) as u64).max(index + 1);
        let new_sample = frame_sample(width, height, capture.next()?, timestamp(next))?;
        submit_frame(&writer, stream, &sample, timestamp(next) - timestamp(index))?;
        sample = new_sample;
        index = next;
        frames += 1;
    }
    log::info!(
        "录屏 {width}×{height}，目标 {fps} FPS，实际平均 {:.1} FPS",
        frames as f64 / clock.elapsed().as_secs_f64()
    );
    unsafe { writer.Finalize().context("完成 MP4 文件失败")? };
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires Windows Media Foundation with H.264 encoder and decoder"]
    fn media_foundation_roundtrip_cropped_frame() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mp4 = directory.path().join("roundtrip.mp4");
        let region = Region {
            x: 0,
            y: 0,
            width: 462,
            height: 286,
        };
        let (width, height) = (region.width as usize, region.height as usize);
        {
            let _media = MediaFoundation::start()?;
            let (writer, stream) =
                create_writer(&mp4, width as u32, height as u32, 24, 750_000, false)?;
            let mut pixels = vec![0; width * height * 4];
            for frame in 0..24 {
                for (index, pixel) in pixels.chunks_exact_mut(4).enumerate() {
                    let color = if index / width < height / 2 && index % width < frame * 20 {
                        [255, 0, 0, 255]
                    } else {
                        [0, 0, 255, 255]
                    };
                    pixel.copy_from_slice(&color);
                }
                let sample =
                    frame_sample(width as u32, height as u32, &pixels, frame as i64 * 416_667)?;
                submit_frame(&writer, stream, &sample, 416_667)?;
            }
            unsafe { writer.Finalize()? };
        }
        let gif = directory.path().join("roundtrip.gif");
        save_recording(&mp4, &gif)?;
        let mut options = gif::DecodeOptions::new();
        options.set_color_output(gif::ColorOutput::RGBA);
        let mut decoder = options.read_info(std::fs::File::open(gif)?)?;
        assert_eq!(
            (decoder.width(), decoder.height()),
            (width as u16, height as u16)
        );

        let mut count = 0;
        while let Some(frame) = decoder.read_next_frame()? {
            let upper = &frame.buffer[(20 * width + 20) * 4..][..3];
            let lower = &frame.buffer[((height - 20) * width + 20) * 4..][..3];
            if count == 0 {
                assert!(upper[0] > 180 && upper[2] < 40, "first frame: {upper:?}");
            }
            if count >= 5 {
                assert!(upper[0] < 40 && upper[2] > 180, "later frame: {upper:?}");
            }
            assert!(lower[0] > 180 && lower[2] < 40, "bottom: {lower:?}");
            count += 1;
        }
        assert_eq!(count, 10);
        Ok(())
    }
}
