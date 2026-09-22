use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, MAX_PATH, POINT};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC,
    DeleteObject, GetDIBits, GetObjectW, HGDIOBJ,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Shell::{SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGetFileInfoW};
use windows::Win32::UI::WindowsAndMessaging::{
    CopyImage, DestroyIcon, GA_ROOT, GetAncestor, GetForegroundWindow, GetIconInfo, GetWindowTextW,
    GetWindowThreadProcessId, HCURSOR, ICONINFO, IDC_CROSS, IMAGE_CURSOR, LR_COPYFROMRESOURCE,
    LoadCursorW, OCR_NORMAL, SPI_SETCURSORS, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetSystemCursor,
    SystemParametersInfoW, WindowFromPoint,
};
use windows::core::PWSTR;

/// 拾取到的窗口信息，供前端在拾取弹窗里做确认展示
#[derive(Debug, Clone, Default)]
pub struct WindowInfo {
    pub hwnd: isize,
    pub exe: String,
    pub path: String,
    pub title: String,
    /// `data:image/png;base64,...`，取不到则为空串
    pub icon: String,
}

/// 当前前台窗口所属程序的 exe 文件名（小写，例如 `notepad.exe`）
pub fn foreground_exe() -> String {
    let hwnd = unsafe { GetForegroundWindow() };
    exe_of_hwnd(hwnd)
}

pub fn exe_of_hwnd(hwnd: HWND) -> String {
    if hwnd.0.is_null() {
        return String::new();
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    process_path(pid).map(|p| exe_name(&p)).unwrap_or_default()
}

/// 坐标处顶层窗口句柄，供拖动中做低开销的"是否切换到新窗口"判断
pub fn root_hwnd_at(pt: (i32, i32)) -> isize {
    let hwnd = unsafe { WindowFromPoint(POINT { x: pt.0, y: pt.1 }) };
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    let target = if root.0.is_null() { hwnd } else { root };
    target.0 as isize
}

/// 准心拾取：返回坐标处顶层窗口的完整信息（exe/路径/标题/图标）。
/// 指向本进程自己的窗口时返回 `None`。
pub fn window_info_from_point(pt: (i32, i32)) -> Option<WindowInfo> {
    let hwnd = unsafe { WindowFromPoint(POINT { x: pt.0, y: pt.1 }) };
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    let target = if root.0.is_null() { hwnd } else { root };
    if target.0.is_null() {
        return None;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(target, Some(&mut pid)) };
    if pid == 0 || pid == std::process::id() {
        return None;
    }
    let path = process_path(pid)?;
    Some(WindowInfo {
        hwnd: target.0 as isize,
        exe: exe_name(&path),
        title: window_title(target),
        icon: icon_data_url(&path),
        path,
    })
}

fn exe_name(full_path: &str) -> String {
    full_path.rsplit(['\\', '/']).next().unwrap_or_default().to_ascii_lowercase()
}

fn process_path(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = [0u16; MAX_PATH as usize];
    let mut len = buf.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    };
    unsafe { CloseHandle(handle) }.ok();
    if ok.is_err() {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if len <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..len as usize])
}

fn icon_cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn icon_data_url(path: &str) -> String {
    if let Some(hit) = icon_cache().lock().unwrap().get(path) {
        return hit.clone();
    }
    let url = extract_icon(path).unwrap_or_default();
    icon_cache().lock().unwrap().insert(path.to_string(), url.clone());
    url
}

fn extract_icon(path: &str) -> Option<String> {
    ensure_com_initialized();
    let wpath: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut info = SHFILEINFOW::default();
    let ok = unsafe {
        SHGetFileInfoW(
            windows::core::PCWSTR(wpath.as_ptr()),
            windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        )
    };
    if ok == 0 || info.hIcon.is_invalid() {
        return None;
    }
    let png = png_from_icon(info.hIcon);
    unsafe { DestroyIcon(info.hIcon) }.ok();
    png.map(|bytes| format!("data:image/png;base64,{}", base64_encode(&bytes)))
}

fn png_from_icon(hicon: windows::Win32::UI::WindowsAndMessaging::HICON) -> Option<Vec<u8>> {
    let mut info = ICONINFO::default();
    unsafe { GetIconInfo(hicon, &mut info) }.ok()?;
    let hbm_color = info.hbmColor;
    let hbm_mask = info.hbmMask;

    let mut bmp = BITMAP::default();
    let size = std::mem::size_of::<BITMAP>() as i32;
    let got = unsafe { GetObjectW(HGDIOBJ(hbm_color.0), size, Some(&mut bmp as *mut _ as *mut _)) };
    if got == 0 || bmp.bmWidth <= 0 || bmp.bmHeight <= 0 {
        let _ = unsafe { DeleteObject(HGDIOBJ(hbm_color.0)) };
        let _ = unsafe { DeleteObject(HGDIOBJ(hbm_mask.0)) };
        return None;
    }
    let (w, h) = (bmp.bmWidth, bmp.bmHeight);

    let hdc = unsafe { CreateCompatibleDC(None) };
    let color = read_dib(hdc, hbm_color, w, h);
    // 32bpp 图标的 alpha 可能全为 0，此时回落到掩码位图判定透明区域
    let opaque = color.as_ref().is_some_and(|p| p.iter().skip(3).step_by(4).any(|&a| a != 0));
    let mask = if opaque { None } else { read_dib(hdc, hbm_mask, w, h) };
    let _ = unsafe { DeleteDC(hdc) };
    let _ = unsafe { DeleteObject(HGDIOBJ(hbm_color.0)) };
    let _ = unsafe { DeleteObject(HGDIOBJ(hbm_mask.0)) };

    let mut pixels = color?;
    // BGRA -> RGBA
    for px in pixels.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    if let Some(mask) = mask {
        for (px, m) in pixels.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
            px[3] = if m[0] == 0 { 255 } else { 0 };
        }
    } else if !opaque {
        for px in pixels.chunks_exact_mut(4) {
            px[3] = 255;
        }
    }

    encode_png(w as u32, h as u32, &pixels)
}

fn read_dib(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    hbm: windows::Win32::Graphics::Gdi::HBITMAP,
    w: i32,
    h: i32,
) -> Option<Vec<u8>> {
    let header = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: w,
        biHeight: -h,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0 as u32,
        biSizeImage: (w * h * 4) as u32,
        ..Default::default()
    };
    let mut bmi = BITMAPINFO { bmiHeader: header, ..Default::default() };
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    let rows = unsafe {
        GetDIBits(
            hdc,
            hbm,
            0,
            h as u32,
            Some(pixels.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        )
    };
    (rows != 0).then_some(pixels)
}

fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header().ok()?.write_image_data(rgba).ok()?;
    Some(out)
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 { TABLE[(b2 & 0x3f) as usize] as char } else { '=' });
    }
    out
}

/// 拾取态下把系统默认光标临时替换成十字准心，`restore_cursor` 还原
pub fn set_pick_cursor() {
    unsafe {
        let Ok(cross) = LoadCursorW(None, IDC_CROSS) else { return };
        let Ok(owned) = CopyImage(HANDLE(cross.0), IMAGE_CURSOR, 0, 0, LR_COPYFROMRESOURCE) else {
            return;
        };
        let _ = SetSystemCursor(HCURSOR(owned.0), OCR_NORMAL);
    }
}

pub fn restore_cursor() {
    unsafe {
        let _ = SystemParametersInfoW(SPI_SETCURSORS, 0, None, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    }
}

fn ensure_com_initialized() {
    thread_local! {
        static INITED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    INITED.with(|inited| {
        if !inited.get() {
            let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
            inited.set(true);
        }
    });
}
