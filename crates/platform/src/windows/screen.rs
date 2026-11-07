//! 按物理像素抓取桌面。资源均在调用线程中释放，不进入鼠标钩子。
use std::io::Cursor;
use std::path::PathBuf;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN};

const MAX_PIXELS: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn intersection(self, bounds: Self) -> Option<Self> {
        let left = i64::from(self.x).max(i64::from(bounds.x));
        let top = i64::from(self.y).max(i64::from(bounds.y));
        let right = (i64::from(self.x) + i64::from(self.width)).min(i64::from(bounds.x) + i64::from(bounds.width));
        let bottom = (i64::from(self.y) + i64::from(self.height)).min(i64::from(bounds.y) + i64::from(bounds.height));
        (right > left && bottom > top).then_some(Self { x: left as i32, y: top as i32, width: (right - left).max(0) as u32, height: (bottom - top).max(0) as u32 })
    }
}

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Frame {
    fn byte_len(width: u32, height: u32) -> Result<usize> {
        let pixels = u64::from(width) * u64::from(height);
        ensure!(pixels > 0 && pixels <= MAX_PIXELS && width <= 32768 && height <= 32768, "图片尺寸超出范围");
        Ok((pixels * 4) as usize)
    }

    pub fn png(&self) -> Result<Vec<u8>> {
        ensure!(self.rgba.len() == Self::byte_len(self.width, self.height)?, "图片数据长度错误");
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, self.width, self.height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_compression(png::Compression::Fast);
            encoder.write_header()?.write_image_data(&self.rgba)?;
        }
        Ok(out)
    }

    pub fn from_png(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() <= 128 * 1024 * 1024, "图片文件过大");
        let mut decoder = png::Decoder::new(Cursor::new(bytes));
        decoder.set_limits(png::Limits { bytes: (MAX_PIXELS * 4) as usize });
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info()?;
        let size = Self::byte_len(reader.info().width, reader.info().height)?;
        let mut data = vec![0; reader.output_buffer_size().context("图片尺寸错误")?];
        let info = reader.next_frame(&mut data)?;
        let mut rgba = Vec::with_capacity(size);
        match info.color_type {
            png::ColorType::Rgba => rgba.extend_from_slice(&data[..info.buffer_size()]),
            png::ColorType::Rgb => for p in data[..info.buffer_size()].chunks_exact(3) { rgba.extend_from_slice(&[p[0], p[1], p[2], 255]); },
            png::ColorType::Grayscale => for &v in &data[..info.buffer_size()] { rgba.extend_from_slice(&[v, v, v, 255]); },
            png::ColorType::GrayscaleAlpha => for p in data[..info.buffer_size()].chunks_exact(2) { rgba.extend_from_slice(&[p[0], p[0], p[0], p[1]]); },
            _ => bail!("图片颜色格式不支持"),
        }
        ensure!(rgba.len() == size, "图片像素长度错误");
        Ok(Self { width: info.width, height: info.height, rgba })
    }
}

pub fn desktop_bounds() -> Rect {
    let _dpi = PhysicalPixels::new();
    unsafe { Rect { x: GetSystemMetrics(SM_XVIRTUALSCREEN), y: GetSystemMetrics(SM_YVIRTUALSCREEN), width: GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1) as u32, height: GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1) as u32 } }
}

/// 锁屏时桌面图像和输入桌面可能不一致，直接退出，避免选错画中画源或卡住剪贴板。
pub fn ensure_interactive_desktop() -> Result<()> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::StationsAndDesktops::*;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::System::RemoteDesktop::*;
    // 新版 Windows 的锁屏画面可能仍在 Default 桌面上，需同时检查会话锁定标志。
    unsafe {
        let mut data = windows::core::PWSTR::null();
        let mut bytes = 0;
        if WTSQuerySessionInformationW(None, WTS_CURRENT_SESSION, WTS_INFO_CLASS(25), &mut data, &mut bytes).is_ok() {
            let locked = if bytes as usize >= size_of::<WTSINFOEXW>() && !data.is_null() {
                let info = &*(data.0 as *const WTSINFOEXW);
                info.Level == 1 && info.Data.WTSInfoExLevel1.SessionFlags == WTS_SESSIONSTATE_LOCK as i32
            } else { false };
            WTSFreeMemory(data.0.cast());
            ensure!(!locked, "系统已锁屏，请解锁 Windows 后重试");
        }
    }
    unsafe fn name(handle: HDESK) -> Result<Vec<u16>> {
        let mut data = [0u16; 256];
        unsafe { GetUserObjectInformationW(HANDLE(handle.0), UOI_NAME, Some(data.as_mut_ptr().cast()), size_of_val(&data) as u32, None) }?;
        let len = data.iter().position(|&c| c == 0).unwrap_or(data.len());
        Ok(data[..len].to_vec())
    }
    unsafe {
        let input = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS)
            .map_err(|_| anyhow::anyhow!("当前桌面处于锁屏或不可交互状态，请解锁 Windows 后重试"))?;
        let result = (|| -> Result<()> {
            let current = GetThreadDesktop(GetCurrentThreadId())?;
            ensure!(name(input)? == name(current)?, "系统已切换至锁屏或安全桌面，请解锁 Windows 后重试");
            Ok(())
        })();
        let _ = CloseDesktop(input);
        result
    }
}

pub fn capture(bounds: Rect) -> Result<Frame> {
    let _dpi = PhysicalPixels::new();
    let size = Frame::byte_len(bounds.width, bounds.height)?;
    unsafe {
        let src = GetDC(None);
        ensure!(!src.is_invalid(), "获取桌面画面失败");
        let dst = CreateCompatibleDC(Some(src));
        if dst.is_invalid() { ReleaseDC(None, src); bail!("创建截图缓冲区失败"); }
        let info = BITMAPINFO { bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: bounds.width as i32, biHeight: -(bounds.height as i32), biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() }, ..Default::default() };
        let mut bits = std::ptr::null_mut();
        let bitmap = match CreateDIBSection(Some(src), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(bitmap) => bitmap,
            Err(e) => { let _ = DeleteDC(dst); ReleaseDC(None, src); return Err(e.into()); }
        };
        let previous = SelectObject(dst, HGDIOBJ(bitmap.0));
        let result = (|| -> Result<Frame> {
            BitBlt(dst, 0, 0, bounds.width as i32, bounds.height as i32, Some(src), bounds.x, bounds.y, SRCCOPY | CAPTUREBLT)?;
            ensure!(!bits.is_null(), "截图像素为空");
            let mut rgba = std::slice::from_raw_parts(bits as *const u8, size).to_vec();
            for p in rgba.chunks_exact_mut(4) { p.swap(0, 2); p[3] = 255; }
            Ok(Frame { width: bounds.width, height: bounds.height, rgba })
        })();
        SelectObject(dst, previous);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        let _ = DeleteDC(dst);
        ReleaseDC(None, src);
        result
    }
}

pub fn save_png(owner: isize, bytes: &[u8]) -> Result<Option<PathBuf>> {
    use windows::Win32::UI::Controls::Dialogs::*;
    use windows::core::{PCWSTR, PWSTR};
    let mut path = [0u16; 32768];
    let initial: Vec<_> = "截图.png".encode_utf16().collect();
    path[..initial.len()].copy_from_slice(&initial);
    let filter: Vec<_> = "PNG 图片\0*.png\0\0".encode_utf16().collect();
    let ext: Vec<_> = "png\0".encode_utf16().collect();
    let mut dialog = OPENFILENAMEW { lStructSize: size_of::<OPENFILENAMEW>() as u32, hwndOwner: HWND(owner as *mut _), lpstrFile: PWSTR(path.as_mut_ptr()), nMaxFile: path.len() as u32, lpstrFilter: PCWSTR(filter.as_ptr()), lpstrDefExt: PCWSTR(ext.as_ptr()), Flags: OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR, ..Default::default() };
    if !unsafe { GetSaveFileNameW(&mut dialog) }.as_bool() {
        let code = unsafe { CommDlgExtendedError() };
        ensure!(code.0 == 0, "保存对话框失败：{code:?}");
        return Ok(None);
    }
    let len = path.iter().position(|&c| c == 0).unwrap_or(path.len());
    let path = PathBuf::from(String::from_utf16_lossy(&path[..len]));
    std::fs::write(&path, bytes)?;
    Ok(Some(path))
}

#[derive(Serialize)]
pub struct OcrLanguage { pub tag: String, pub name: String }

pub fn ocr_languages() -> Result<Vec<OcrLanguage>> {
    use windows::Media::Ocr::OcrEngine;
    let _apartment = Apartment::new()?;
    Ok(OcrEngine::AvailableRecognizerLanguages()?.into_iter().map(|language| Ok(OcrLanguage { tag: language.LanguageTag()?.to_string(), name: language.DisplayName()?.to_string() })).collect::<windows::core::Result<Vec<_>>>()?)
}

pub fn ocr(frame: &Frame, language: &str) -> Result<String> {
    use windows::{Globalization::Language, Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap}, Media::Ocr::OcrEngine, Storage::Streams::DataWriter};
    let _apartment = Apartment::new()?;
    ensure!(frame.width <= OcrEngine::MaxImageDimension()? && frame.height <= OcrEngine::MaxImageDimension()?, "识别区域过大，请缩小选区");
    let engine = if language.is_empty() { OcrEngine::TryCreateFromUserProfileLanguages() } else { OcrEngine::TryCreateFromLanguage(&Language::CreateLanguage(&language.into())?) };
    let engine = engine.map_err(|_| anyhow::anyhow!("未找到对应的离线 OCR 语言包，请在 Windows 设置的语言选项中安装文字识别组件"))?;
    let writer = DataWriter::new()?;
    let mut bgra = frame.rgba.clone();
    for pixel in bgra.chunks_exact_mut(4) { pixel.swap(0, 2); }
    writer.WriteBytes(&bgra)?;
    let bitmap = SoftwareBitmap::CreateCopyFromBuffer(&writer.DetachBuffer()?, BitmapPixelFormat::Bgra8, frame.width as i32, frame.height as i32)?;
    let result = engine.RecognizeAsync(&bitmap)?.join()?;
    let lines = result.Lines()?.into_iter().map(|line| line.Text().map(|s| tidy_ocr_line(&s.to_string()))).collect::<windows::core::Result<Vec<_>>>()?;
    Ok(lines.join("\n"))
}

fn tidy_ocr_line(text: &str) -> String {
    let han = |c: char| matches!(c as u32, 0x3400..=0x9fff | 0xf900..=0xfaff | 0x20000..=0x2ffff);
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == ' ' && out.chars().last().is_some_and(han) && chars.clone().find(|c| *c != ' ').is_some_and(han) { continue; }
        out.push(c);
    }
    out
}

struct Apartment;
impl Apartment {
    fn new() -> Result<Self> {
        unsafe { windows::Win32::System::WinRT::RoInitialize(windows::Win32::System::WinRT::RO_INIT_MULTITHREADED) }?;
        Ok(Self)
    }
}
impl Drop for Apartment { fn drop(&mut self) { unsafe { windows::Win32::System::WinRT::RoUninitialize() } } }

struct PhysicalPixels(windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT);
impl PhysicalPixels {
    fn new() -> Self { Self(unsafe { windows::Win32::UI::HiDpi::SetThreadDpiAwarenessContext(windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }) }
}
impl Drop for PhysicalPixels {
    fn drop(&mut self) { if !self.0.0.is_null() { unsafe { windows::Win32::UI::HiDpi::SetThreadDpiAwarenessContext(self.0); } } }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_clips_negative_monitor_coordinates_without_overflow() {
        let bounds = Rect { x: -3840, y: -1080, width: 5760, height: 3240 };
        assert_eq!(Rect { x: -4000, y: -1200, width: 500, height: 400 }.intersection(bounds), Some(Rect { x: -3840, y: -1080, width: 340, height: 280 }));
        assert!(Rect { x: i32::MAX, y: 0, width: u32::MAX, height: 100 }.intersection(bounds).is_none());
        assert!(Rect { x: 0, y: 0, width: 0, height: 1 }.intersection(bounds).is_none());
    }
    #[test]
    fn png_roundtrip_preserves_color_and_alpha() {
        let frame = Frame { width: 2, height: 1, rgba: vec![255, 0, 1, 255, 2, 128, 255, 123] };
        assert_eq!(Frame::from_png(&frame.png().unwrap()).unwrap().rgba, frame.rgba);
        assert!(Frame::byte_len(32768, 32768).is_err());
        assert!(Frame::from_png(b"not a png").is_err());
    }
    #[test]
    fn ocr_spacing_preserves_english_and_joins_chinese_words() {
        assert_eq!(tidy_ocr_line("离 线 文 字 识 别 测 试"), "离线文字识别测试");
        assert_eq!(tidy_ocr_line("NextGestures 2026 test"), "NextGestures 2026 test");
    }
    #[test]
    fn two_4k_monitors_png_roundtrip_exceeds_the_default_decoder_limit() {
        let frame = Frame { width: 7680, height: 2160, rgba: vec![127; 7680 * 2160 * 4] };
        let restored = Frame::from_png(&frame.png().unwrap()).unwrap();
        assert_eq!((restored.width, restored.height), (7680, 2160));
        assert_eq!(restored.rgba, frame.rgba);
    }
}
