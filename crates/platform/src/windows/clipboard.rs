use std::time::Duration;

use anyhow::{Result, bail};
use windows::Win32::Foundation::{HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};

const CF_UNICODETEXT: u32 = 13;

fn open() -> Result<()> {
    for _ in 0..10 {
        if unsafe { OpenClipboard(None) }.is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    bail!("剪贴板被其他程序占用")
}

pub fn get_text() -> Result<String> {
    open()?;
    let out = (|| -> Result<String> {
        unsafe {
            let handle = GetClipboardData(CF_UNICODETEXT)?;
            let mem = HGLOBAL(handle.0);
            let p = GlobalLock(mem) as *const u16;
            if p.is_null() {
                bail!("剪贴板锁定失败");
            }
            let mut len = 0usize;
            while *p.add(len) != 0 {
                len += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(p, len));
            let _ = GlobalUnlock(mem);
            Ok(text)
        }
    })();
    unsafe { CloseClipboard() }.ok();
    out
}

pub fn set_text(text: &str) -> Result<()> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    open()?;
    let out = (|| -> Result<()> {
        unsafe {
            EmptyClipboard()?;
            let mem = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2)?;
            let p = GlobalLock(mem) as *mut u16;
            if p.is_null() {
                bail!("剪贴板分配失败");
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
            let _ = GlobalUnlock(mem);
            SetClipboardData(CF_UNICODETEXT, Some(HANDLE(mem.0)))?;
            Ok(())
        }
    })();
    unsafe { CloseClipboard() }.ok();
    out
}

/// CF_DIB 的行从下往上排列，像素为 BGRA；原图不携带窗口透明度或装饰。
fn image_dib(width: i32, height: i32, pixels: &[u8]) -> Result<Vec<u8>> {
    anyhow::ensure!(
        width > 0 && height > 0 && width as u64 * height as u64 * 4 == pixels.len() as u64,
        "图片像素长度错误"
    );
    let mut dib = Vec::with_capacity(40 + pixels.len());
    dib.extend_from_slice(&40u32.to_le_bytes());
    dib.extend_from_slice(&width.to_le_bytes());
    dib.extend_from_slice(&height.to_le_bytes());
    dib.extend_from_slice(&1u16.to_le_bytes());
    dib.extend_from_slice(&32u16.to_le_bytes());
    dib.extend_from_slice(&[0u8; 24]);
    for row in pixels.chunks_exact(width as usize * 4).rev() {
        dib.extend_from_slice(row);
    }
    Ok(dib)
}

pub fn set_image_bgra(
    owner: windows::Win32::Foundation::HWND,
    width: i32,
    height: i32,
    pixels: &[u8],
) -> Result<()> {
    use windows::Win32::Foundation::GlobalFree;
    let dib = image_dib(width, height, pixels)?;
    let mut opened = false;
    for _ in 0..10 {
        if unsafe { OpenClipboard(Some(owner)) }.is_ok() {
            opened = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    anyhow::ensure!(opened, "剪贴板被其他程序占用，请稍后重试");
    let result = (|| -> Result<()> {
        unsafe {
            let memory = GlobalAlloc(GMEM_MOVEABLE, dib.len())?;
            let target = GlobalLock(memory);
            if target.is_null() {
                let _ = GlobalFree(Some(memory));
                bail!("图片剪贴板分配失败");
            }
            std::ptr::copy_nonoverlapping(dib.as_ptr(), target.cast::<u8>(), dib.len());
            let _ = GlobalUnlock(memory);
            // 成功后内存所有权归剪贴板；失败时自行释放。
            if let Err(e) = EmptyClipboard()
                .and_then(|_| SetClipboardData(8, Some(HANDLE(memory.0))).map(|_| ()))
            {
                let _ = GlobalFree(Some(memory));
                return Err(e.into());
            }
            Ok(())
        }
    })();
    let _ = unsafe { CloseClipboard() };
    result
}

#[cfg(test)]
mod image_tests {
    use super::*;
    #[test]
    fn dib_exports_original_pixels_bottom_up() {
        let dib = image_dib(1, 2, &[0, 0, 255, 255, 255, 0, 0, 255]).unwrap();
        assert_eq!(dib.len(), 48);
        assert_eq!(i32::from_le_bytes(dib[8..12].try_into().unwrap()), 2);
        assert_eq!(&dib[40..], &[255, 0, 0, 255, 0, 0, 255, 255]);
        assert!(image_dib(2, 2, &[0; 4]).is_err());
    }
}

/// 向前台窗口发 Ctrl+C 取当前选中文本，取完还原原剪贴板内容
pub fn read_selection() -> Result<String> {
    let backup = get_text().ok();
    let _ = set_text("");
    super::input::send_hotkey(&["ctrl".into(), "c".into()])?;

    let mut text = String::new();
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(25));
        match get_text() {
            Ok(s) if !s.is_empty() => {
                text = s;
                break;
            }
            _ => {}
        }
    }

    if let Some(b) = backup {
        let _ = set_text(&b);
    }
    Ok(text)
}

/// 贴图可用的剪贴板内容。
pub enum PinContent {
    /// 不透明 BGRA 像素（带透明度的图片已合成到白底上）。
    Image { width: i32, height: i32, pixels: Vec<u8> },
    /// 纯文本，以及来源程序提供的 RTF（Word、WPS、写字板等）。
    Text { text: String, rtf: Option<Vec<u8>> },
}

/// 剪贴板内容每次变化都会递增，用来判断是否已经贴过当前内容。
pub fn sequence_number() -> u32 {
    unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() }
}

fn registered(name: windows::core::PCWSTR) -> u32 {
    unsafe { windows::Win32::System::DataExchange::RegisterClipboardFormatW(name) }
}

/// 读取剪贴板里某格式的原始字节；须已打开剪贴板。
fn raw(format: u32) -> Option<Vec<u8>> {
    use windows::Win32::System::DataExchange::IsClipboardFormatAvailable;
    use windows::Win32::System::Memory::GlobalSize;
    if format == 0 || unsafe { IsClipboardFormatAvailable(format) }.is_err() {
        return None;
    }
    unsafe {
        let handle = GetClipboardData(format).ok()?;
        let memory = HGLOBAL(handle.0);
        let size = GlobalSize(memory);
        let pointer = GlobalLock(memory) as *const u8;
        if pointer.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(pointer, size).to_vec();
        let _ = GlobalUnlock(memory);
        (!bytes.is_empty()).then_some(bytes)
    }
}

/// 图片优先（PNG 保留透明度，其次 DIB），否则取文字；都没有返回 None。
pub fn pin_content() -> Result<Option<PinContent>> {
    use windows::core::w;
    let png_format = registered(w!("PNG"));
    let rtf_format = registered(w!("Rich Text Format"));
    open()?;
    let (png, dib, text, rtf) = (
        raw(png_format),
        raw(17).or_else(|| raw(8)),
        raw(CF_UNICODETEXT),
        raw(rtf_format),
    );
    unsafe { CloseClipboard() }.ok();
    let image = png
        .and_then(|bytes| {
            decode_png(&bytes)
                .inspect_err(|e| log::warn!("剪贴板 PNG 解码失败: {e:#}"))
                .ok()
        })
        .or_else(|| {
            dib.and_then(|bytes| {
                decode_dib(&bytes)
                    .inspect_err(|e| log::warn!("剪贴板 DIB 解码失败: {e:#}"))
                    .ok()
            })
        });
    if let Some((width, height, pixels)) = image {
        return Ok(Some(PinContent::Image { width, height, pixels }));
    }
    let text = text.map(|bytes| {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .take_while(|&unit| unit != 0)
            .collect();
        String::from_utf16_lossy(&units)
    });
    let rtf = rtf.map(|mut bytes| {
        if let Some(end) = bytes.iter().position(|&b| b == 0) {
            bytes.truncate(end);
        }
        bytes
    });
    Ok(match (text, rtf) {
        (Some(text), rtf) if !text.trim().is_empty() => Some(PinContent::Text { text, rtf }),
        (_, Some(rtf)) if !rtf.is_empty() => {
            Some(PinContent::Text { text: String::new(), rtf: Some(rtf) })
        }
        _ => None,
    })
}

/// 直通 alpha 的像素合成到白底，输出不透明 BGRA。
fn over_white(blue: u8, green: u8, red: u8, alpha: u8) -> [u8; 4] {
    let mix = |c: u8| {
        ((u32::from(c) * u32::from(alpha) + 255 * (255 - u32::from(alpha)) + 127) / 255) as u8
    };
    [mix(blue), mix(green), mix(red), 255]
}

fn decode_png(bytes: &[u8]) -> Result<(i32, i32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info()?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| anyhow::anyhow!("PNG 尺寸过大"))?;
    let mut buffer = vec![0; size];
    let info = reader.next_frame(&mut buffer)?;
    let (width, height) = (info.width as usize, info.height as usize);
    let channels = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        other => bail!("不支持的 PNG 颜色类型 {other:?}"),
    };
    let mut pixels = Vec::with_capacity(width * height * 4);
    for row in buffer.chunks(info.line_size).take(height) {
        for px in row.chunks_exact(channels).take(width) {
            let (r, g, b, a) = match channels {
                1 => (px[0], px[0], px[0], 255),
                2 => (px[0], px[0], px[0], px[1]),
                3 => (px[0], px[1], px[2], 255),
                _ => (px[0], px[1], px[2], px[3]),
            };
            pixels.extend_from_slice(&over_white(b, g, r, a));
        }
    }
    anyhow::ensure!(pixels.len() == width * height * 4, "PNG 数据不完整");
    Ok((width as i32, height as i32, pixels))
}

/// 解析 CF_DIB / CF_DIBV5：24 / 32 位，BI_RGB 或标准 BGRA 掩码。
fn decode_dib(bytes: &[u8]) -> Result<(i32, i32, Vec<u8>)> {
    let u32_at = |at: usize| -> Result<u32> {
        let field = bytes.get(at..at + 4).ok_or_else(|| anyhow::anyhow!("DIB 头不完整"))?;
        Ok(u32::from_le_bytes(field.try_into()?))
    };
    let header = u32_at(0)? as usize;
    anyhow::ensure!(header >= 40, "不支持的 DIB 头");
    let width = u32_at(4)? as i32;
    let raw_height = u32_at(8)? as i32;
    let bits = u32_at(12)? >> 16;
    let compression = u32_at(16)?;
    let colors = u32_at(32)? as usize;
    anyhow::ensure!(width > 0 && raw_height != 0 && raw_height != i32::MIN, "DIB 尺寸无效");
    anyhow::ensure!(bits == 24 || bits == 32, "不支持 {bits} 位 DIB");
    let mut offset = header + colors * 4;
    match compression {
        0 => {}
        // BI_BITFIELDS / BI_ALPHABITFIELDS：掩码紧跟 40 字节头或位于 V4/V5 头内，只接受标准 BGRA 排列。
        3 | 6 => {
            if header == 40 {
                offset += if compression == 3 { 12 } else { 16 };
            }
            anyhow::ensure!(
                bits == 32
                    && (u32_at(40)?, u32_at(44)?, u32_at(48)?)
                        == (0x00ff_0000, 0x0000_ff00, 0x0000_00ff),
                "不支持的 DIB 颜色掩码"
            );
        }
        _ => bail!("不支持压缩的 DIB"),
    }
    let height = raw_height.unsigned_abs() as usize;
    let width_px = width as usize;
    let stride = (width_px * bits as usize).div_ceil(32) * 4;
    let data = bytes
        .get(offset..offset + stride * height)
        .ok_or_else(|| anyhow::anyhow!("DIB 像素不完整"))?;
    let step = bits as usize / 8;
    let rows: Vec<&[u8]> = if raw_height > 0 {
        data.chunks_exact(stride).rev().collect()
    } else {
        data.chunks_exact(stride).collect()
    };
    // 很多程序的 32 位 DIB alpha 全为 0，此时按不透明处理。
    let alpha = bits == 32
        && rows
            .iter()
            .any(|row| row[..width_px * 4].chunks_exact(4).any(|p| p[3] != 0));
    let mut pixels = Vec::with_capacity(width_px * height * 4);
    for row in rows {
        for px in row[..width_px * step].chunks_exact(step) {
            let a = if alpha { px[3] } else { 255 };
            pixels.extend_from_slice(&over_white(px[0], px[1], px[2], a));
        }
    }
    Ok((width, height as i32, pixels))
}

#[cfg(test)]
mod pin_content_tests {
    use super::*;
    #[test]
    fn dib_round_trip_and_transparent_pixels_land_on_white() {
        let dib = image_dib(1, 2, &[0, 0, 255, 255, 255, 0, 0, 255]).unwrap();
        assert_eq!(decode_dib(&dib).unwrap(), (1, 2, vec![0, 0, 255, 255, 255, 0, 0, 255]));
        let mut half = image_dib(1, 1, &[0, 0, 0, 128]).unwrap();
        assert_eq!(decode_dib(&half).unwrap().2, vec![127, 127, 127, 255]);
        // alpha 全为 0 视为不透明。
        half[43] = 0;
        assert_eq!(decode_dib(&half).unwrap().2, vec![0, 0, 0, 255]);
        let mut rgb = vec![0u8; 40];
        rgb[0] = 40;
        rgb[4] = 1;
        rgb[8] = 1;
        rgb[14] = 24;
        rgb.extend_from_slice(&[10, 20, 30, 0]);
        assert_eq!(decode_dib(&rgb).unwrap().2, vec![10, 20, 30, 255]);
    }
    #[test]
    fn png_alpha_is_flattened_to_white() {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 2, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&[255, 0, 0, 255, 0, 0, 0, 0])
                .unwrap();
        }
        assert_eq!(
            decode_png(&bytes).unwrap(),
            (2, 1, vec![0, 0, 255, 255, 255, 255, 255, 255])
        );
    }
}
