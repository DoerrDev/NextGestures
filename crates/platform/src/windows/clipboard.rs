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
